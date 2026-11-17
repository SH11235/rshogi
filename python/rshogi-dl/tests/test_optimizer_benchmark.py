from __future__ import annotations

import contextlib
from dataclasses import asdict, replace
import inspect
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import torch

from rshogi_dl import benchmark, trainer
from rshogi_dl.model import TEST_CONFIG
from .support import artifact_tempdir, synthetic_records, write_jsonl


class OptimizerBenchmarkTest(unittest.TestCase):
    def setUp(self) -> None:
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.root = Path(self.stack.enter_context(artifact_tempdir()))
        self.data = self.root / "alternating.jsonl"
        rows = synthetic_records(seed=11, count=4)
        for row in rows[:2]:
            row["policy"] = None
        rows[2]["policy"] = [[rows[2]["legal_labels"][0], 1.0]]
        write_jsonl(self.data, rows)
        self.config = benchmark.BenchmarkConfig(
            batch_size=2, window_batches=2, warmup=0, steps=4, seed=7, weight_decay=.2,
        )

    def test_configuration_and_trainer_identity_are_separate(self) -> None:
        self.assertEqual(self.config.optimizer_execution, "eager")
        self.assertFalse(self.config.optimizer_capturable)
        for options in (
            {"optimizer_execution": "other"}, {"optimizer_capturable": 1},
            {"optimizer_capturable": True},
            {"device": "cuda", "optimizer_execution": "compile"},
        ):
            with self.subTest(options=options), self.assertRaises(ValueError):
                replace(self.config, **options)
        for execution in ("eager", "compile"):
            config = replace(self.config, device="cuda", optimizer_execution=execution,
                             optimizer_capturable=True)
            self.assertTrue(config.optimizer_capturable)
        identity = trainer._identity(trainer.TrainerConfig(max_steps=4))
        self.assertNotIn("optimizer_execution", identity)
        self.assertNotIn("optimizer_capturable", identity)
        self.assertNotIn("optimizer_execution", asdict(trainer.TrainerConfig(max_steps=4)))

    def test_same_optimizer_grad_none_state_and_scaler_dispatch(self) -> None:
        original_clear = benchmark.clear_unused_policy_head_gradients
        original_scaler = torch.amp.GradScaler
        eager = benchmark.run_benchmark(self.data, TEST_CONFIG, self.config)
        for input_mode in ("resident", "stream"):
            events = []
            owners = []
            heads = []

            def clear(model, present):
                original_clear(model, present)
                heads[:] = list(model.policy_head.parameters())
                events.append(("clear", present))

            def compile_step(step, **options):
                self.assertEqual(options, {"backend": "inductor", "fullgraph": False,
                                           "options": {"triton.cudagraphs": False}})
                optimizer = step.__self__
                owners.append(optimizer)
                self.assertEqual(optimizer.state, {})

                def compiled():
                    self.assertEqual(events[-1][0], "clear")
                    present = events[-1][1]
                    before = [(parameter.detach().clone(), {
                        name: value.clone() for name, value in optimizer.state.get(parameter, {}).items()
                    }) for parameter in heads]
                    for parameter in heads:
                        self.assertEqual(parameter.grad is None, not present)
                    result = step()
                    if not present:
                        for parameter, (weight, state) in zip(heads, before, strict=True):
                            self.assertTrue(torch.equal(parameter, weight))
                            self.assertEqual(optimizer.state.get(parameter, {}).keys(), state.keys())
                            for name, value in state.items():
                                self.assertTrue(torch.equal(optimizer.state[parameter][name], value))
                    events.append(("step", present))
                    return result

                return compiled

            class Scaler(original_scaler):
                def step(scaler, optimizer, *args, **kwargs):
                    self.assertIs(optimizer, owners[0])
                    self.assertNotIn("grad_scaler", inspect.signature(optimizer.step).parameters)
                    return super().step(optimizer, *args, **kwargs)

            with (
                patch.object(torch, "compile", side_effect=compile_step) as compiled,
                patch.object(torch.amp, "GradScaler", Scaler),
                patch.object(benchmark, "clear_unused_policy_head_gradients", side_effect=clear),
                patch.object(torch.compiler, "cudagraph_mark_step_begin", side_effect=AssertionError("no graph")),
            ):
                result = benchmark.run_benchmark(self.data, TEST_CONFIG, replace(
                    self.config, optimizer_execution="compile", input_mode=input_mode,
                ))
            compiled.assert_called_once()
            self.assertEqual(events, [(kind, present) for present in (False, True, False, True)
                                      for kind in ("clear", "step")])
            self.assertFalse(result["optimizer_cudagraphs_requested"])
            self.assertEqual(result["optimizer_capturable_resolved"], [False])
            for name in ("initial_weights_sha256", "final_weights_sha256", "losses_sha256",
                         "input_window_sha256", "optimizer_updates", "skipped_updates"):
                self.assertEqual(result[name], eager[name], name)

    def test_compile_failure_is_not_an_eager_result_and_settings_are_restored(self) -> None:
        for lazy in (False, True):
            failure = RuntimeError("compiler failed")

            def compile_step(step, **options):
                if not lazy:
                    raise failure
                def fail():
                    raise failure
                return fail

            with torch._dynamo.config.patch(suppress_errors=True, fail_on_recompile_limit_hit=False):
                with patch.object(torch, "compile", side_effect=compile_step):
                    with self.assertRaises(RuntimeError) as raised:
                        benchmark.run_benchmark(self.data, TEST_CONFIG,
                                                replace(self.config, optimizer_execution="compile"))
                self.assertIs(raised.exception, failure)
                self.assertTrue(torch._dynamo.config.suppress_errors)
                self.assertFalse(torch._dynamo.config.fail_on_recompile_limit_hit)

    def test_cli_configures_optimizer_without_compiling_forward(self) -> None:
        output, errors = io.StringIO(), io.StringIO()

        def compile_step(step, **options):
            self.assertIsInstance(step.__self__, torch.optim.AdamW)
            self.assertFalse(torch._dynamo.config.suppress_errors)
            self.assertTrue(torch._dynamo.config.fail_on_recompile_limit_hit)
            return step

        with (
            patch.object(torch, "compile", side_effect=compile_step) as compiled,
            contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors),
        ):
            status = benchmark.main([
                "--data", str(self.data), "--config", "test", "--batch-size", "2",
                "--steps", "2", "--warmup", "0", "--optimizer-execution", "compile",
            ])
        self.assertEqual(status, 0, errors.getvalue())
        compiled.assert_called_once()
        result = json.loads(output.getvalue())
        self.assertEqual(result["benchmark_config"]["execution"], "eager")
        self.assertEqual(result["benchmark_config"]["optimizer_execution"], "compile")


if __name__ == "__main__":
    unittest.main()
