from __future__ import annotations

from copy import deepcopy
from dataclasses import replace
import json
import math
import os
from pathlib import Path
import random
import unittest
from unittest import mock

import numpy as np
import torch

from rshogi_dl.checkpoint import load_checkpoint
from rshogi_dl._optimizers import BatchedMuon, MuonAdamW
from rshogi_dl._residual_norm import compiled_residual_forward
from rshogi_dl._tuned_ffn import tuned_ffn_forward
from rshogi_dl.export import export_onnx
from rshogi_dl.model import TEST_CONFIG, FeedForward, SquareTransformer
from rshogi_dl.paths import ArtifactPathError
from rshogi_dl.sharded import DatasetError, convert_jsonl, dataset_info, iter_batches
from rshogi_dl.trainer import TrainerConfig, TrainingError, evaluate, run_training
from rshogi_dl.trainer_checkpoint import (
    ResumeError, capture_rng, load_resume, restore_rng, validate_scaler_state,
)

from .support import artifact_tempdir, synthetic_records, write_jsonl


class TrainerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = artifact_tempdir()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.train = self.make_dataset("train", synthetic_records(seed=3, count=5))
        self.validation = self.make_dataset("validation", synthetic_records(seed=4, count=3))
        self.config = TrainerConfig(
            max_steps=7, batch_size=3, seed=17, threads=1, log_every=2,
            deterministic=True, validation_only=True, validation_batches=0,
        )

    def make_dataset(self, name, records) -> Path:
        source = self.root / f"{name}.jsonl"
        write_jsonl(source, records)
        output = self.root / name
        convert_jsonl(
            source, output, shard_records=2,
            teacher_kind="synthetic-validation-targets", split=name, training_status="validation-only",
        )
        return output

    def run_training(self, name, config=None, **options):
        return run_training(
            self.train, self.validation, self.root / name,
            TEST_CONFIG, config or self.config, **options,
        )

    def assert_nested_equal(self, left, right):
        self.assertEqual(type(left), type(right))
        if isinstance(left, torch.Tensor):
            self.assertTrue(torch.equal(left, right))
        elif isinstance(left, dict):
            self.assertEqual(left.keys(), right.keys())
            for key in left:
                self.assert_nested_equal(left[key], right[key])
        elif isinstance(left, (tuple, list)):
            self.assertEqual(len(left), len(right))
            for first, second in zip(left, right, strict=True):
                self.assert_nested_equal(first, second)
        else:
            self.assertEqual(left, right)

    def test_cpu_resume_is_bit_exact_across_shards_tail_and_epochs(self) -> None:
        full_report = self.run_training("full")
        first = self.run_training("first", replace(self.config, max_steps=3, checkpoint_every=1))
        self.assertEqual(first["progress"]["sample_cursor"], 3)
        self.assertEqual(first["progress"]["epoch"], 1)
        resumed_report = self.run_training("resumed", resume=self.root / "first" / "resume-final.pt")
        full = load_resume(self.root / "full" / "resume-final.pt")
        resumed = load_resume(self.root / "resumed" / "resume-final.pt")
        for field in ("model_state", "optimizer_state", "scaler_state", "scheduler_state", "progress", "rng"):
            self.assert_nested_equal(full[field], resumed[field])
        self.run_training("mid-resumed", resume=self.root / "first" / "resume-0000000002.pt")
        intermediate = load_resume(self.root / "mid-resumed" / "resume-final.pt")
        for field in ("model_state", "optimizer_state", "progress", "rng"):
            self.assert_nested_equal(full[field], intermediate[field])
        self.assertEqual(full_report["progress"], {
            "step": 7, "optimizer_updates": 7, "policy_optimizer_updates": 7,
            "epoch": 3, "sample_cursor": 3, "samples_seen": 18,
        })
        self.assertEqual(full_report["amp_skipped_steps"], 0)
        self.assertEqual(resumed_report["segment_samples"], 10)
        self.assertEqual(resumed_report["validation"]["rows"], 3)
        canonical = load_checkpoint(self.root / "resumed" / "checkpoint.pt")
        self.assert_nested_equal(canonical.model.state_dict(), resumed["model_state"])
        self.assertEqual(canonical.training_status, "validation-only")

    def test_packed_projection_resume_and_export(self) -> None:
        packed = replace(TEST_CONFIG, packed_qkv=True)
        first, resumed, full = (self.root / name for name in ("packed-first", "packed-resumed", "packed-full"))
        run_training(self.train, self.validation, first, packed, replace(self.config, max_steps=3))
        run_training(self.train, self.validation, full, packed, self.config)
        checkpoint = first / "resume-final.pt"
        run_training(self.train, self.validation, resumed, packed, self.config, resume=checkpoint)
        with self.assertRaises(ResumeError):
            run_training(self.train, self.validation, self.root / "different-layout", TEST_CONFIG, self.config,
                         resume=checkpoint)
        uninterrupted, restarted = (load_resume(path / "resume-final.pt") for path in (full, resumed))
        for key in ("model_state", "optimizer_state", "progress"):
            self.assert_nested_equal(uninterrupted[key], restarted[key])
        model = load_checkpoint(resumed / "checkpoint.pt").model
        self.assertEqual(model.config, packed)
        self.assertTrue(export_onnx(model, self.root / "packed.onnx").is_file())

    def test_cursor_seeks_without_opening_earlier_shards(self) -> None:
        expected = list(iter_batches(self.train, 1))[3:]
        (self.train / "shard-00000000" / "tokens.npy").unlink()
        actual = list(iter_batches(self.train, 1, start_record=3))
        self.assertEqual(len(expected), len(actual))
        for left, right in zip(expected, actual, strict=True):
            self.assertTrue(torch.equal(left.tokens, right.tokens))
        self.assertEqual(list(iter_batches(self.train, 1, start_record=5)), [])
        for cursor in (-1, 6, True):
            with self.assertRaises(DatasetError):
                next(iter_batches(self.train, 1, start_record=cursor))

    def test_manifest_fingerprint_is_constant_work(self) -> None:
        info = dataset_info(self.train)
        (self.train / "shard-00000000" / "metadata.json").unlink()
        self.assertEqual(dataset_info(self.train), info)

    def test_unknown_provenance_and_same_validation_need_explicit_permission(self) -> None:
        with self.assertRaises(TrainingError):
            self.run_training("unapproved", replace(self.config, validation_only=False))
        self.assertFalse((self.root / "unapproved").exists())
        with self.assertRaisesRegex(TrainingError, "distinct"):
            run_training(self.train, self.train, self.root / "same", TEST_CONFIG, self.config)

    def test_config_and_dataset_mismatch_fail_before_optimizer_update(self) -> None:
        self.run_training("first", replace(self.config, max_steps=2))
        checkpoint = self.root / "first" / "resume-final.pt"
        with mock.patch.object(torch.optim.AdamW, "step", side_effect=AssertionError("must not update")):
            with self.assertRaisesRegex(ResumeError, "configuration"):
                self.run_training("wrong-config", replace(self.config, learning_rate=0.02), resume=checkpoint)
            new_train = self.make_dataset("changed", synthetic_records(seed=9, count=5))
            with self.assertRaisesRegex(ResumeError, "dataset"):
                run_training(new_train, self.validation, self.root / "wrong-data", TEST_CONFIG, self.config, resume=checkpoint)
        self.assertFalse((self.root / "wrong-config").exists())
        self.assertFalse((self.root / "wrong-data").exists())

    def test_torn_checkpoint_and_invalid_progress_scheduler_scaler_are_rejected(self) -> None:
        self.run_training("first", replace(self.config, max_steps=2))
        source = self.root / "first" / "resume-final.pt"
        torn = self.root / "torn.pt"
        torn.write_bytes(source.read_bytes()[:300])
        with self.assertRaises(ResumeError):
            self.run_training("torn-run", resume=torn)
        for key, value in (
            ("scheduler_state", {}), ("scaler_state", {"bad": 1}),
            ("progress", {"step": 2, "epoch": 3, "sample_cursor": 1, "samples_seen": 16, "optimizer_updates": 2}),
        ):
            payload = load_resume(source)
            payload[key] = value
            path = self.root / f"bad-{key}.pt"
            torch.save(payload, path)
            with self.subTest(key=key), self.assertRaises(ResumeError):
                self.run_training(f"run-{key}", resume=path)
            self.assertFalse((self.root / f"run-{key}").exists())

    def test_optimizer_hyperparameter_tampering_is_rejected(self) -> None:
        self.run_training("first", replace(self.config, max_steps=2))
        payload = load_resume(self.root / "first" / "resume-final.pt")
        payload["optimizer_state"]["param_groups"][0]["lr"] = 0.75
        path = self.root / "wrong-optimizer.pt"
        torch.save(payload, path)
        with mock.patch.object(torch.optim.AdamW, "step", side_effect=AssertionError("must not update")):
            with self.assertRaisesRegex(ResumeError, "parameter groups"):
                self.run_training("wrong-optimizer", resume=path)
        self.assertFalse((self.root / "wrong-optimizer").exists())

    def test_missing_backbone_and_inconsistent_parameter_updates_are_rejected(self) -> None:
        self.run_training("first", replace(self.config, max_steps=2))
        source = self.root / "first" / "resume-final.pt"
        parameter_names = [name for name, _ in SquareTransformer(TEST_CONFIG).named_parameters()]
        for damage in (
            "missing-backbone", "backbone-step", "negative-second-moment",
            "missing-policy-weight", "missing-policy-both", "policy-bias-step",
        ):
            payload = load_resume(source)
            optimizer = payload["optimizer_state"]
            ids = dict(zip(parameter_names, optimizer["param_groups"][0]["params"], strict=True))
            state = optimizer["state"]
            backbone = ids["position_embedding"]
            policy_weight = ids["policy_head.weight"]
            policy_bias = ids["policy_head.bias"]
            if damage == "missing-backbone":
                del state[backbone]
            elif damage == "backbone-step":
                state[backbone]["step"].fill_(1)
            elif damage == "negative-second-moment":
                state[backbone]["exp_avg_sq"].flatten()[0] = -1
            elif damage == "missing-policy-weight":
                del state[policy_weight]
            elif damage == "missing-policy-both":
                del state[policy_weight]
                del state[policy_bias]
            else:
                state[policy_bias]["step"].fill_(1)
            path = self.root / f"{damage}.pt"
            torch.save(payload, path)
            with mock.patch.object(torch.optim.AdamW, "step", side_effect=AssertionError("must not update")):
                with self.subTest(damage=damage), self.assertRaises(ResumeError):
                    self.run_training(f"run-{damage}", resume=path)
            self.assertFalse((self.root / f"run-{damage}").exists())

    def test_policy_head_can_have_fewer_updates_and_resume_before_its_first_update(self) -> None:
        records = synthetic_records(seed=21, count=4)
        records[0]["policy"] = None
        records[2]["policy"] = None
        records[3]["policy"] = [[records[3]["legal_labels"][0], 1]]
        self.train = self.make_dataset("partly-null-policy", records)
        config = replace(self.config, batch_size=1, max_steps=4)
        report = self.run_training("policy-full", config)
        self.assertEqual(report["progress"]["optimizer_updates"], 4)
        self.assertEqual(report["progress"]["policy_optimizer_updates"], 2)
        first = self.run_training("policy-first", replace(config, max_steps=1))
        self.assertEqual(first["progress"]["policy_optimizer_updates"], 0)
        self.run_training("policy-resumed", config, resume=self.root / "policy-first" / "resume-final.pt")
        full = load_resume(self.root / "policy-full" / "resume-final.pt")
        resumed = load_resume(self.root / "policy-resumed" / "resume-final.pt")
        for field in ("model_state", "optimizer_state", "progress", "rng"):
            self.assert_nested_equal(full[field], resumed[field])

    def test_identical_array_content_with_different_split_metadata_is_rejected(self) -> None:
        duplicate = self.make_dataset("duplicate-validation", synthetic_records(seed=3, count=5))
        left, right = dataset_info(self.train), dataset_info(duplicate)
        self.assertNotEqual(left["fingerprint"], right["fingerprint"])
        self.assertEqual(left["manifest"]["dataset_content_sha256"], right["manifest"]["dataset_content_sha256"])
        with self.assertRaisesRegex(TrainingError, "distinct"):
            run_training(self.train, duplicate, self.root / "duplicate-run", TEST_CONFIG, self.config)
        self.assertFalse((self.root / "duplicate-run").exists())

    def test_actual_aot_trainer_preserves_null_head_state_and_resume_with_weight_decay(self) -> None:
        records = synthetic_records(seed=23, count=4)
        records[0]["policy"] = records[2]["policy"] = None
        records[1]["policy"] = [[records[1]["legal_labels"][0], 1]]
        records[3]["policy"] = [[records[3]["legal_labels"][0], 1]]
        self.train = self.make_dataset("aot-alternating-policy", records)
        config = replace(
            self.config, max_steps=4, batch_size=1, weight_decay=0.2,
            execution="compile", checkpoint_every=1,
        )
        original_compile = torch.compile
        def aot_compile(model, **options):
            self.assertEqual(options, {"backend": "inductor", "mode": "default"})
            return original_compile(model, backend="aot_eager")
        with mock.patch("rshogi_dl.trainer.torch.compile", side_effect=aot_compile):
            full_report = self.run_training("aot-full", config)
            first_report = self.run_training("aot-first", replace(config, max_steps=3))
            resumed_report = self.run_training(
                "aot-resumed", config, resume=self.root / "aot-first" / "resume-final.pt",
            )
        self.assertEqual(full_report["progress"]["policy_optimizer_updates"], 2)
        self.assertEqual(first_report["progress"]["sample_cursor"], 3)
        self.assertEqual(first_report["progress"]["policy_optimizer_updates"], 1)
        self.assertEqual(resumed_report["progress"], full_report["progress"])
        full = load_resume(self.root / "aot-full" / "resume-final.pt")
        resumed = load_resume(self.root / "aot-resumed" / "resume-final.pt")
        for field in ("model_state", "optimizer_state", "progress", "rng"):
            self.assert_nested_equal(full[field], resumed[field])
        before_null = load_resume(self.root / "aot-full" / "resume-0000000002.pt")
        after_null = load_resume(self.root / "aot-full" / "resume-0000000003.pt")
        self.assertEqual(before_null["progress"]["policy_optimizer_updates"], 1)
        self.assertEqual(after_null["progress"]["policy_optimizer_updates"], 1)
        self.assertEqual(after_null["progress"]["optimizer_updates"], 3)
        parameter_names = [name for name, _ in SquareTransformer(TEST_CONFIG).named_parameters()]
        parameter_ids = dict(zip(
            parameter_names, before_null["optimizer_state"]["param_groups"][0]["params"], strict=True,
        ))
        for name in ("policy_head.weight", "policy_head.bias"):
            self.assertTrue(torch.equal(before_null["model_state"][name], after_null["model_state"][name]))
            index = parameter_ids[name]
            self.assert_nested_equal(
                before_null["optimizer_state"]["state"][index],
                after_null["optimizer_state"]["state"][index],
            )
        self.run_training("aot-eager-reference", replace(config, execution="eager"))
        eager = load_resume(self.root / "aot-eager-reference" / "resume-final.pt")
        for name, expected in eager["model_state"].items():
            torch.testing.assert_close(full["model_state"][name], expected, rtol=1e-5, atol=1e-6)

    def test_final_checkpoint_survives_heldout_evaluation_failure(self) -> None:
        config = replace(self.config, max_steps=2)
        with mock.patch("rshogi_dl.trainer.evaluate", side_effect=TrainingError("validation failed")):
            with self.assertRaisesRegex(TrainingError, "validation failed"):
                self.run_training("evaluation-failed", config)
        path = self.root / "evaluation-failed"
        self.assertFalse((path / "report.json").exists())
        self.assertEqual(load_resume(path / "resume-final.pt")["progress"]["step"], 2)
        self.assertEqual(load_checkpoint(path / "checkpoint.pt").step, 2)
        report = self.run_training("evaluation-recovered", self.config, resume=path / "resume-final.pt")
        self.assertEqual(report["progress"]["step"], self.config.max_steps)

    def test_partial_amp_skip_accounting_and_resume_using_cpu_mock(self) -> None:
        original_step = torch.amp.GradScaler.step
        def skipping_step():
            calls = 0
            def step(scaler, optimizer, *args, **kwargs):
                nonlocal calls
                calls += 1
                if calls in (1, 3):
                    return None
                return original_step(scaler, optimizer, *args, **kwargs)
            return step
        config = replace(self.config, max_steps=6, log_every=1, checkpoint_every=2)
        with mock.patch.object(torch.amp.GradScaler, "step", autospec=True, side_effect=skipping_step()):
            full_report = self.run_training("skip-full", config)
        with mock.patch.object(torch.amp.GradScaler, "step", autospec=True, side_effect=skipping_step()):
            first = self.run_training("skip-first", replace(config, max_steps=4))
        self.assertEqual(first["amp_skipped_steps"], 2)
        self.assertEqual(first["progress"]["optimizer_updates"], 2)
        events = [json.loads(line) for line in (self.root / "skip-first" / "metrics.jsonl").read_text().splitlines()]
        self.assertEqual([event["amp_skipped_steps"] for event in events], [1, 1, 2, 2])
        resumed_report = self.run_training("skip-resumed", config, resume=self.root / "skip-first" / "resume-final.pt")
        self.assertEqual(full_report["amp_skipped_steps"], 2)
        self.assertEqual(resumed_report["amp_skipped_steps"], 2)
        full = load_resume(self.root / "skip-full" / "resume-final.pt")
        resumed = load_resume(self.root / "skip-resumed" / "resume-final.pt")
        for field in ("model_state", "optimizer_state", "progress", "rng"):
            self.assert_nested_equal(full[field], resumed[field])

    def test_no_actual_optimizer_update_is_not_reported_as_training(self) -> None:
        with mock.patch.object(torch.optim.AdamW, "step", return_value=None):
            with self.assertRaisesRegex(TrainingError, "no optimizer update"):
                self.run_training("skipped", replace(self.config, max_steps=1))
        self.assertFalse((self.root / "skipped" / "checkpoint.pt").exists())

    def test_checkpoint_cadence_is_bounded_and_run_never_overwrites(self) -> None:
        self.run_training("cadence", replace(self.config, max_steps=4, checkpoint_every=1))
        files = sorted(path.name for path in (self.root / "cadence").glob("resume*.pt"))
        self.assertEqual(files, ["resume-0000000001.pt", "resume-0000000002.pt", "resume-0000000003.pt", "resume-final.pt"])
        with self.assertRaises(ArtifactPathError):
            self.run_training("cadence")
        with self.assertRaises(ValueError):
            replace(self.config, max_steps=100, checkpoint_every=1)
        for field, value in (("max_steps", 0), ("max_steps", True), ("batch_size", 0), ("learning_rate", float("nan")), ("device", "other"), ("precision", "fp16")):
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                replace(self.config, **{field: value})

    def test_missing_policy_keeps_head_grad_none_with_weight_decay(self) -> None:
        records = synthetic_records(seed=12, count=4)
        for record in records:
            record["policy"] = None
        self.train = self.make_dataset("null-policy", records)
        previous = torch.random.get_rng_state()
        try:
            torch.random.default_generator.manual_seed(self.config.seed)
            initial = SquareTransformer(TEST_CONFIG)
        finally:
            torch.random.set_rng_state(previous)
        self.run_training("null-run", replace(self.config, max_steps=2, weight_decay=0.2))
        checkpoint = load_checkpoint(self.root / "null-run" / "checkpoint.pt")
        self.assertTrue(torch.equal(initial.policy_head.weight, checkpoint.model.policy_head.weight))
        self.assertTrue(torch.equal(initial.policy_head.bias, checkpoint.model.policy_head.bias))
        self.assertFalse(torch.equal(initial.value_head[-1].weight, checkpoint.model.value_head[-1].weight))

    def test_heldout_counts_and_brier_match_manual_uniform_model(self) -> None:
        records = synthetic_records(seed=5, count=3)
        records[0].update(legal_labels=[1, 2], policy=[[1, 1]], value=0.0)
        records[1].update(legal_labels=[3, 4, 5], policy=None, value=0.5)
        records[2].update(legal_labels=[], policy=None, value=1.0)
        validation = self.make_dataset("manual-validation", records)
        model = SquareTransformer(TEST_CONFIG)
        with torch.no_grad():
            for parameter in model.parameters():
                parameter.zero_()
        result = evaluate(model, validation, replace(self.config, batch_size=2), torch.device("cpu"))
        self.assertEqual(result["rows"], 3)
        self.assertEqual(result["policy_rows"], 1)
        self.assertAlmostEqual(result["policy_ce"], math.log(2), places=6)
        self.assertAlmostEqual(result["value_bce"], math.log(2), places=6)
        self.assertAlmostEqual(result["value_brier"], 1 / 6, places=6)
        for record in records:
            record["policy"] = None
        validation = self.make_dataset("manual-null-validation", records)
        result = evaluate(model, validation, self.config, torch.device("cpu"))
        self.assertIsNone(result["policy_ce"])
        self.assertEqual(result["policy_rows"], 0)

    def test_rng_round_trip_and_caller_rng_preserved(self) -> None:
        state = capture_rng(cuda=False)
        try:
            random.seed(25)
            np.random.seed(26)
            torch.random.default_generator.manual_seed(27)
            saved = capture_rng(cuda=False)
            expected = (random.random(), np.random.random(), torch.rand(4))
            restore_rng(saved, cuda=False)
            actual = (random.random(), np.random.random(), torch.rand(4))
            self.assertEqual(expected[:2], actual[:2])
            self.assertTrue(torch.equal(expected[2], actual[2]))
            before = capture_rng(cuda=False)
            self.run_training("rng-run", replace(self.config, max_steps=1))
            self.assert_nested_equal(before, capture_rng(cuda=False))
        finally:
            restore_rng(state, cuda=False)

    def test_cuda_never_falls_back_to_cpu(self) -> None:
        with mock.patch.object(torch.cuda, "is_available", return_value=False):
            with self.assertRaisesRegex(TrainingError, "no CPU fallback"):
                self.run_training("cuda", replace(self.config, device="cuda"))
        self.assertFalse((self.root / "cuda").exists())

    def test_cuda_graph_requires_cuda_bf16_fused_eager(self) -> None:
        for execution in ("cuda-graph", "cuda-graph-fused-norm"):
            options = {"device": "cuda", "precision": "bf16", "fused_optimizer": True, "execution": execution}
            replace(self.config, **options)
            for invalid in ({"device": "cpu", "precision": "fp32", "fused_optimizer": False},
                            {"precision": "fp16"}, {"fused_optimizer": False},
                            {"compile_mode": "reduce-overhead"}):
                with self.subTest(execution=execution, invalid=invalid), self.assertRaises(ValueError):
                    replace(self.config, **(options | invalid))

    def test_residual_norm_forward_matches_native_without_compiler(self) -> None:
        model = SquareTransformer(replace(TEST_CONFIG, packed_qkv=True))
        tokens = next(iter_batches(self.train, 2)).tokens
        with mock.patch("rshogi_dl._residual_norm.torch.compile", side_effect=lambda fn, **_: fn):
            fused = compiled_residual_forward(model)
        native_outputs, fused_outputs = model(tokens), fused(tokens)
        for native, actual in zip(native_outputs, fused_outputs, strict=True):
            torch.testing.assert_close(actual, native, rtol=0, atol=0)
        parameters = tuple(model.parameters())
        native_grads = torch.autograd.grad(sum(out.sum() for out in native_outputs), parameters)
        fused_grads = torch.autograd.grad(sum(out.sum() for out in fused_outputs), parameters)
        for native, actual in zip(native_grads, fused_grads, strict=True):
            torch.testing.assert_close(actual, native)

    def test_tuned_ffn_routes_full_batches_only(self) -> None:
        with self.assertRaisesRegex(ValueError, "whole-step CUDA Graph"):
            replace(self.config, compile_ffn=True)
        model = SquareTransformer(replace(TEST_CONFIG, packed_qkv=True))
        tokens = next(iter_batches(self.train, 2)).tokens
        for fused_norm in (False, True):
            compiled_batches = []

            def compile_mock(fn, **kwargs):
                if fn is not FeedForward.forward:
                    return fn
                self.assertEqual(kwargs["mode"], "max-autotune-no-cudagraphs")

                def run(layer, inputs):
                    compiled_batches.append(inputs.shape[0])
                    return fn(layer, inputs)

                return run

            with self.subTest(fused_norm=fused_norm), mock.patch("torch.compile", side_effect=compile_mock):
                forward = tuned_ffn_forward(model, 2, fused_norm=fused_norm)
                for batch in (tokens, tokens[:1], tokens):
                    for expected, actual in zip(model(batch), forward(batch), strict=True):
                        torch.testing.assert_close(actual, expected, rtol=0, atol=0)
                self.assertEqual(compiled_batches, [2] * (2 * len(model.blocks)))

    def test_fused_norm_cast_moves_the_bf16_conversion(self) -> None:
        with self.assertRaisesRegex(ValueError, "cuda-graph-fused-norm"):
            replace(self.config, fuse_norm_cast=True)
        model = SquareTransformer(replace(TEST_CONFIG, packed_qkv=True))
        tokens = next(iter_batches(self.train, 2)).tokens
        inputs = []

        def run_ffn(layer, value):
            inputs.append(value.dtype)
            return layer(value)

        with mock.patch("torch.compile", side_effect=lambda operation, **kwargs: operation):
            native = compiled_residual_forward(model)
            cast = compiled_residual_forward(model, run_ffn=run_ffn, cast_output=True)
            with torch.autocast("cpu", dtype=torch.bfloat16):
                for expected, actual in zip(native(tokens), cast(tokens), strict=True):
                    torch.testing.assert_close(actual, expected, rtol=0, atol=0)
        self.assertEqual(inputs, [torch.bfloat16] * len(model.blocks))

    def test_muon_config_and_checkpoint_parameter_partition(self) -> None:
        for options in ({"optimizer": "muon", "muon_learning_rate": 0.002},
                        {"muon_learning_rate": 0.002},
                        {"optimizer": "batched-muon", "device": "cuda", "precision": "bf16",
                         "fused_optimizer": True}):
            with self.subTest(options=options), self.assertRaises(ValueError):
                replace(self.config, **options)
        model = SquareTransformer(replace(TEST_CONFIG, packed_qkv=True))
        optimizer = MuonAdamW(model, lr=0.001, muon_lr=0.002, weight_decay=0.01,
                              capturable=False, batched=True)
        for p in model.parameters():
            p.grad = torch.ones_like(p)
        optimizer.step()
        saved = deepcopy(optimizer.state_dict())
        optimizer.validate_muon(1)
        names = saved["parameter_names"]
        self.assertIn("blocks.0.attention.qkv.weight", names["muon"])
        self.assertIn("policy_head.weight", names["adamw"])
        self.assertIn("token_embedding.weight", names["adamw"])
        self.assertEqual(len(names["muon"]) + len(names["adamw"]), len(tuple(model.parameters())))
        for slot, tensor, message in (
            ("step", lambda value: value.double(), "AdamW step"),
            ("exp_avg", lambda value: value.half(), "AdamW moments"),
        ):
            corrupted = deepcopy(saved)
            state = corrupted["adamw"]["state"][0]
            state[slot] = tensor(state[slot])
            with self.subTest(slot=slot), self.assertRaisesRegex(ResumeError, message):
                optimizer.load_state_dict(corrupted)
        state = saved["muon"]["state"][0]
        state["momentum_buffer"] = state["momentum_buffer"].double()
        with self.assertRaisesRegex(ResumeError, "momentum buffer"):
            optimizer.load_state_dict(saved)

    @unittest.skipUnless(os.environ.get("RSHOGI_DL_RUN_CUDA_TESTS") == "1", "requires an idle BF16 CUDA device")
    def test_cuda_graph_resume_after_eager_fallback(self) -> None:
        self.check_cuda_graph_resume("cuda-graph")

    @unittest.skipUnless(os.environ.get("RSHOGI_DL_RUN_CUDA_TESTS") == "1", "requires an idle BF16 CUDA device")
    def test_cuda_graph_fused_norm_resume_after_eager_fallback(self) -> None:
        self.check_cuda_graph_resume("cuda-graph-fused-norm")

    @unittest.skipUnless(os.environ.get("RSHOGI_DL_RUN_CUDA_TESTS") == "1", "requires an idle BF16 CUDA device")
    def test_cuda_graph_muon_resume_after_eager_fallback(self) -> None:
        self.check_cuda_graph_resume("cuda-graph", optimizer="muon")

    @unittest.skipUnless(os.environ.get("RSHOGI_DL_RUN_CUDA_TESTS") == "1", "requires an idle BF16 CUDA device")
    def test_cuda_graph_batched_muon_resume_after_eager_fallback(self) -> None:
        self.check_cuda_graph_resume("cuda-graph", optimizer="batched-muon")

    @unittest.skipUnless(os.environ.get("RSHOGI_DL_RUN_CUDA_TESTS") == "1", "requires an idle BF16 CUDA device")
    def test_batched_muon_matches_stock_updates_with_weight_decay(self) -> None:
        random = torch.Generator(device="cuda").manual_seed(91)
        reference = [torch.nn.Parameter(torch.randn(256, 128, device="cuda", generator=random)) for _ in range(3)]
        candidate = [torch.nn.Parameter(p.detach().clone()) for p in reference]
        options = {"lr": 0.002, "weight_decay": 0.01}
        standard = torch.optim.Muon(reference, **options, adjust_lr_fn="match_rms_adamw")
        batched = BatchedMuon(candidate, **options)
        for _ in range(3):
            before = [p.detach().clone() for p in reference]
            for p, q in zip(reference, candidate, strict=True):
                with torch.no_grad():
                    q.copy_(p)
                p.grad = q.grad = torch.randn(p.shape, device=p.device, generator=random)
            standard.step()
            batched.step()
            for initial, p, q in zip(before, reference, candidate, strict=True):
                torch.testing.assert_close(
                    standard.state[p]["momentum_buffer"], batched.state[q]["momentum_buffer"], rtol=0, atol=0,
                )
                error = torch.linalg.vector_norm(p - q) / torch.linalg.vector_norm(p - initial)
                self.assertLessEqual(float(error.detach()), 0.025)

    @unittest.skipUnless(os.environ.get("RSHOGI_DL_RUN_CUDA_TESTS") == "1", "requires an idle BF16 CUDA device")
    def test_cuda_graph_tuned_ffn_resume_after_eager_fallback(self) -> None:
        self.check_cuda_graph_resume("cuda-graph-fused-norm", optimizer="batched-muon", compile_ffn=True)

    @unittest.skipUnless(os.environ.get("RSHOGI_DL_RUN_CUDA_TESTS") == "1", "requires an idle BF16 CUDA device")
    def test_cuda_graph_norm_cast_resume_after_eager_fallback(self) -> None:
        self.check_cuda_graph_resume(
            "cuda-graph-fused-norm", optimizer="batched-muon", compile_ffn=True, fuse_norm_cast=True,
        )

    @unittest.skipUnless(os.environ.get("RSHOGI_DL_RUN_CUDA_TESTS") == "1", "requires an idle BF16 CUDA device")
    def test_cuda_graph_prefetch_resume_after_eager_fallback(self) -> None:
        self.check_cuda_graph_resume(
            "cuda-graph-fused-norm", optimizer="batched-muon", compile_ffn=True,
            fuse_norm_cast=True, prefetch_input=True,
        )

    def check_cuda_graph_resume(self, execution: str, optimizer: str = "adamw", *,
                                compile_ffn: bool = False, fuse_norm_cast: bool = False,
                                prefetch_input: bool = False) -> None:
        records = synthetic_records(seed=29, count=19)
        for index, record in enumerate(records):
            record["policy"] = None if index in (10, 11, 12) else [[record["legal_labels"][0], 1]]
        records[14]["legal_labels"] = list(range(513))
        records[14]["policy"] = [[0, 1]]
        self.train = self.make_dataset("mixed-policy", records)
        config = replace(
            self.config, max_steps=19, batch_size=2, checkpoint_every=4, weight_decay=0.01,
            device="cuda", precision="bf16", fused_optimizer=True, execution=execution,
            attention_backend="sdpa", optimizer=optimizer,
            muon_learning_rate=0.002 if optimizer != "adamw" else None,
            compile_ffn=compile_ffn,
            fuse_norm_cast=fuse_norm_cast,
            prefetch_input=prefetch_input,
        )
        full = self.run_training("graph", config)
        self.assertEqual(full["cuda_graph_replayed_steps"], 8)
        self.assertEqual(full["progress"]["optimizer_updates"], 19)
        self.assertEqual(full["progress"]["policy_optimizer_updates"], 17)
        with mock.patch("rshogi_dl._step_graph._WARMUP_STEPS", config.max_steps):
            eager = self.run_training("padded-eager", config)
        self.assertEqual(eager["cuda_graph_replayed_steps"], 0)
        expected = load_resume(self.root / "padded-eager" / "resume-final.pt")
        for step in (4, 8):
            path = self.root / "graph" / f"resume-{step:010d}.pt"
            resumed = self.run_training(f"graph-resumed-{step}", config, resume=path)
            self.assertEqual(resumed["progress"], full["progress"])
            self.assertGreater(resumed["cuda_graph_replayed_steps"], 0)
        runs = ["graph", "graph-resumed-4", "graph-resumed-8"]
        if prefetch_input:
            synchronous = replace(config, prefetch_input=False)
            self.run_training("synchronous", synchronous)
            self.run_training(
                "resume-without-prefetch", synchronous, resume=self.root / "graph/resume-0000000008.pt",
            )
            self.run_training(
                "resume-with-prefetch", config, resume=self.root / "synchronous/resume-0000000008.pt",
            )
            runs.extend(("synchronous", "resume-without-prefetch", "resume-with-prefetch"))
        for name in runs:
            checkpoint = load_resume(self.root / name / "resume-final.pt")
            for field in ("model_state", "optimizer_state", "progress", "rng"):
                self.assert_nested_equal(expected[field], checkpoint[field])

    def test_prefetch_requires_cuda_graph(self) -> None:
        with self.assertRaisesRegex(ValueError, "prefetch_input requires whole-step CUDA Graph execution"):
            replace(self.config, prefetch_input=True)

    def test_compile_mode_validation_forwarding_and_resume_identity(self) -> None:
        for options in (
            {"compile_mode": "max-autotune-no-cudagraphs"},
            {"execution": "compile", "compile_mode": "reduce-overhead"},
            {"execution": "compile", "compile_mode": "max-autotune"},
            {"execution": "compile", "compile_mode": "unknown"},
        ):
            with self.subTest(options=options), self.assertRaises(ValueError):
                replace(self.config, **options)
        config = replace(
            self.config, max_steps=1, execution="compile",
            compile_mode="max-autotune-no-cudagraphs",
        )
        with (
            mock.patch("rshogi_dl.trainer.torch.compile", side_effect=lambda model, **options: model) as compile_model,
            mock.patch("rshogi_dl.trainer.torch.compiler.cudagraph_mark_step_begin") as marker,
        ):
            report = self.run_training("compile-mock", config)
        compile_model.assert_called_once()
        self.assertEqual(compile_model.call_args.kwargs, {
            "backend": "inductor", "mode": "max-autotune-no-cudagraphs",
        })
        marker.assert_not_called()
        self.assertFalse(report["cudagraphs_requested"])
        checkpoint = self.root / "compile-mock" / "resume-final.pt"
        self.assertEqual(load_resume(checkpoint)["trainer_config"]["compile_mode"], config.compile_mode)
        with self.assertRaisesRegex(ResumeError, "configuration"):
            self.run_training("different-mode", replace(config, max_steps=2, compile_mode="default"), resume=checkpoint)
        payload = load_resume(checkpoint)
        del payload["trainer_config"]["compile_mode"]
        missing = self.root / "missing-compile-mode.pt"
        torch.save(payload, missing)
        with self.assertRaisesRegex(ResumeError, "configuration fields"):
            self.run_training("missing-mode", config, resume=missing)


class ScalerValidationTest(unittest.TestCase):
    def test_fp16_scaler_keys_types_ranges_and_disabled_state(self) -> None:
        valid = {
            "scale": 65536.0, "growth_factor": 2.0, "backoff_factor": 0.5,
            "growth_interval": 2000, "_growth_tracker": 3,
        }
        validate_scaler_state(valid, enabled=True)
        validate_scaler_state({**valid, "scale": 1024.0, "_growth_tracker": 1999}, enabled=True)
        validate_scaler_state({}, enabled=False)
        cases = {
            "scale": (0.0, -1.0, True, 1, float("inf"), float("nan"), 1e-50, 1e40, 0.1),
            "growth_factor": (True, 1, 1.0, 0.0, 3.0, float("inf")),
            "backoff_factor": (True, 0.0, 1.0, -0.5, 0.25, float("nan")),
            "growth_interval": (True, 0, -1, 1.5, 1000, 2**40),
            "_growth_tracker": (True, -1, 2000, 1.0, 2**35),
        }
        for key, values in cases.items():
            for value in values:
                with self.subTest(key=key, value=value), self.assertRaises(ResumeError):
                    validate_scaler_state({**valid, key: value}, enabled=True)
        for state in ({}, {**valid, "extra": 0}, {key: value for key, value in valid.items() if key != "scale"}, []):
            with self.subTest(state=state), self.assertRaises(ResumeError):
                validate_scaler_state(state, enabled=True)
        with self.assertRaises(ResumeError):
            validate_scaler_state(valid, enabled=False)
        with self.assertRaisesRegex(ResumeError, "hyperparameters"):
            validate_scaler_state({**valid, "growth_interval": 2**40, "_growth_tracker": 2**35}, enabled=True)


if __name__ == "__main__":
    unittest.main()
