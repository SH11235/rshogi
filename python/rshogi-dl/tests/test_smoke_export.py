from __future__ import annotations

import contextlib
import io
import json
import random
import unittest
from pathlib import Path

import numpy as np
import onnx
import torch

from rshogi_dl import export, parity, smoke
from rshogi_dl.checkpoint import CheckpointError, load_checkpoint
from rshogi_dl.contract import (
    CPU_PARITY_ATOL,
    CPU_PARITY_RTOL,
    PARITY_BATCH_SIZES,
    POLICY_SIZE,
)
from rshogi_dl.model import TEST_CONFIG, SquareTransformer
from rshogi_dl.paths import ArtifactPathError

from .support import artifact_tempdir, random_tokens, synthetic_records, write_jsonl


def run_cli(main, argv: list[str]) -> tuple[int, str, str]:
    stdout, stderr = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        code = main(argv)
    return code, stdout.getvalue(), stderr.getvalue()


class SmokeAndExportTest(unittest.TestCase):
    """smoke -> checkpoint 再読込 -> export -> PyTorch / ONNX Runtime 照合を 1 回の成果物で確かめる。"""

    @classmethod
    def setUpClass(cls) -> None:
        cls.stack = contextlib.ExitStack()
        cls.addClassCleanup(cls.stack.close)
        cls.runs = Path(cls.stack.enter_context(artifact_tempdir(0)))
        cls.models = Path(cls.stack.enter_context(artifact_tempdir(1)))
        cls.fixture = cls.runs / "fixture.jsonl"
        write_jsonl(cls.fixture, synthetic_records(seed=3, count=20))

        cls.run_dir = cls.runs / "run-a"
        cls.report = smoke.run_smoke(cls.fixture, cls.run_dir, TEST_CONFIG, seed=1)
        cls.checkpoint_path = cls.run_dir / smoke.CHECKPOINT_NAME
        cls.onnx_path = cls.models / "model.onnx"
        code, cls.export_stdout, cls.export_stderr = run_cli(
            export.main, ["--checkpoint", str(cls.checkpoint_path), "--output", str(cls.onnx_path)]
        )
        if code != 0:
            raise AssertionError(f"export failed: {cls.export_stderr}")

    def test_smoke_report(self) -> None:
        report = self.report
        self.assertEqual((smoke.SMOKE_STEPS, smoke.SMOKE_BATCH_SIZE), (16, 8))
        self.assertEqual(report["steps"], 16)
        self.assertEqual(report["batch_size"], 8)
        self.assertEqual(report["training_status"], "validation-only")
        self.assertEqual(report["model_config"], TEST_CONFIG.to_dict())
        self.assertEqual(len(report["losses"]), 16)
        self.assertTrue(all(np.isfinite(report["losses"])))
        self.assertLess(report["first_batch_loss_after"], report["first_batch_loss_before"])
        for name, norm in report["max_gradient_norm"].items():
            with self.subTest(group=name):
                self.assertTrue(np.isfinite(norm))
                self.assertGreater(norm, 0.0)
        with open(self.run_dir / smoke.REPORT_NAME, encoding="utf-8") as stream:
            self.assertEqual(json.load(stream), report)
        self.assertEqual(
            sorted(path.name for path in self.run_dir.iterdir()),
            sorted([smoke.CHECKPOINT_NAME, smoke.REPORT_NAME]),
        )

    def test_same_seed_reproduces_and_other_seed_differs(self) -> None:
        repeat = smoke.run_smoke(self.fixture, self.runs / "run-b", TEST_CONFIG, seed=1)
        self.assertEqual(repeat["losses"], self.report["losses"])
        first = load_checkpoint(self.checkpoint_path).model.state_dict()
        second = load_checkpoint(self.runs / "run-b" / smoke.CHECKPOINT_NAME).model.state_dict()
        self.assertTrue(all(torch.equal(first[key], second[key]) for key in first))
        other = smoke.run_smoke(self.fixture, self.runs / "run-c", TEST_CONFIG, seed=2)
        self.assertNotEqual(other["losses"], self.report["losses"])

    def test_checkpoint_reload(self) -> None:
        checkpoint = load_checkpoint(self.checkpoint_path)
        self.assertEqual(checkpoint.model.config, TEST_CONFIG)
        self.assertEqual(checkpoint.step, 16)
        self.assertEqual(checkpoint.seed, 1)
        self.assertEqual(checkpoint.training_status, "validation-only")
        self.assertFalse(checkpoint.model.training)
        optimizer = torch.optim.AdamW(checkpoint.model.parameters())
        optimizer.load_state_dict(checkpoint.optimizer_state)
        self.assertEqual(optimizer.param_groups[0]["lr"], smoke.SMOKE_LEARNING_RATE)
        # 学習後の重みが保存されている (同じ seed の初期値とは違う)。
        torch.manual_seed(1)
        initial = SquareTransformer(TEST_CONFIG).state_dict()
        trained = checkpoint.model.state_dict()
        self.assertFalse(all(torch.equal(initial[key], trained[key]) for key in initial))

    def test_checkpoint_with_other_status_is_rejected(self) -> None:
        payload = torch.load(self.checkpoint_path, map_location="cpu", weights_only=True)
        payload["training_status"] = "released"
        tampered = self.runs / "tampered.pt"
        torch.save(payload, tampered)
        with self.assertRaises(CheckpointError):
            load_checkpoint(tampered)

    def test_smoke_cli_refuses_overwrite_and_bad_inputs(self) -> None:
        code, _, stderr = run_cli(
            smoke.main,
            ["--data", str(self.fixture), "--run-dir", str(self.run_dir), "--config", "test"],
        )
        self.assertEqual(code, 2)
        self.assertIn("overwrite", stderr)

        outside = self.runs.parent.parent / "other-trainer" / "run"
        code, _, stderr = run_cli(
            smoke.main, ["--data", str(self.fixture), "--run-dir", str(outside), "--config", "test"]
        )
        self.assertEqual(code, 2)
        self.assertFalse(outside.exists())

        short = self.runs / "short.jsonl"
        write_jsonl(short, synthetic_records(seed=4, count=7))
        short_run = self.runs / "run-short"
        code, _, stderr = run_cli(
            smoke.main, ["--data", str(short), "--run-dir", str(short_run), "--config", "test"]
        )
        self.assertEqual(code, 2)
        self.assertIn("at least 8", stderr)
        self.assertFalse(short_run.exists())

    def test_smoke_cli_success(self) -> None:
        run_dir = self.runs / "run-cli"
        code, stdout, stderr = run_cli(
            smoke.main,
            ["--data", str(self.fixture), "--run-dir", str(run_dir), "--config", "test", "--seed", "1"],
        )
        self.assertEqual(code, 0, stderr)
        self.assertIn("first-batch loss", stdout)
        self.assertTrue((run_dir / smoke.CHECKPOINT_NAME).is_file())

    def test_export_graph_and_metadata(self) -> None:
        self.assertEqual(self.export_stdout.strip(), str(self.onnx_path))
        model = onnx.load(str(self.onnx_path))
        metadata = {entry.key: entry.value for entry in model.metadata_props}
        self.assertEqual(
            metadata,
            {
                "rshogi.input_format": "stm-tokens-v1",
                "rshogi.policy_format": "direction-destination-2187-v1",
                "rshogi.value_format": "stm-expected-score-sigmoid-v1",
                "rshogi.architecture": "square-transformer-v1",
                "rshogi.model_config": '{"ffn_width": 128, "heads": 4, "layers": 2, "width": 32}',
                "rshogi.training_status": "validation-only",
            },
        )
        opsets = {entry.domain: entry.version for entry in model.opset_import}
        self.assertEqual(opsets.get("", opsets.get("ai.onnx")), 18)

        def shape(value_info) -> list:
            return [
                dim.dim_value if dim.HasField("dim_value") else dim.dim_param
                for dim in value_info.type.tensor_type.shape.dim
            ]

        graph = model.graph
        self.assertEqual([value.name for value in graph.input], ["tokens"])
        self.assertEqual(graph.input[0].type.tensor_type.elem_type, onnx.TensorProto.INT64)
        self.assertEqual(shape(graph.input[0]), ["batch", 96])
        self.assertEqual([value.name for value in graph.output], ["output_policy", "output_value"])
        self.assertEqual(shape(graph.output[0]), ["batch", POLICY_SIZE])
        self.assertEqual(shape(graph.output[1]), ["batch", 1])
        for output in graph.output:
            self.assertEqual(output.type.tensor_type.elem_type, onnx.TensorProto.FLOAT)

        op_types = [node.op_type for node in graph.node]
        self.assertFalse([op for op in op_types if "Conv" in op])
        self.assertEqual(op_types.count("Softmax"), TEST_CONFIG.layers)
        self.assertGreaterEqual(op_types.count("MatMul"), 2 * TEST_CONFIG.layers)
        self.assertEqual(op_types.count("Sigmoid"), 1)

    def test_verification_rejects_other_contracts(self) -> None:
        data = self.onnx_path.read_bytes()
        export.verify_onnx_bytes(data, TEST_CONFIG)
        model = onnx.load_from_string(data)
        del model.metadata_props[:]
        with self.assertRaisesRegex(export.ExportError, "metadata"):
            export.verify_onnx_bytes(model.SerializeToString(), TEST_CONFIG)
        fixed_batch = onnx.load_from_string(data)
        fixed_batch.graph.input[0].type.tensor_type.shape.dim[0].dim_value = 2
        with self.assertRaisesRegex(export.ExportError, "dynamic batch"):
            export.verify_onnx_bytes(fixed_batch.SerializeToString(), TEST_CONFIG)

    def test_export_refuses_overwrite_and_outside_paths(self) -> None:
        arguments = ["--checkpoint", str(self.checkpoint_path), "--output"]
        code, _, stderr = run_cli(export.main, [*arguments, str(self.onnx_path)])
        self.assertEqual(code, 2)
        self.assertIn("overwrite", stderr)
        outside = self.models.parent.parent / "model.onnx"
        code, _, _ = run_cli(export.main, [*arguments, str(outside)])
        self.assertEqual(code, 2)
        self.assertFalse(outside.exists())
        with self.assertRaisesRegex(ArtifactPathError, r"\.onnx"):
            export.export_onnx(load_checkpoint(self.checkpoint_path).model, self.models / "model.bin")
        self.assertFalse((self.models / "model.bin").exists())

    def _parity_tokens(self) -> np.ndarray:
        rng = random.Random(9)
        return np.array([random_tokens(rng) for _ in range(4)], dtype=np.int64)

    def test_torch_and_onnxruntime_agree_on_cpu(self) -> None:
        self.assertEqual((CPU_PARITY_RTOL, CPU_PARITY_ATOL), (1e-4, 1e-5))
        self.assertEqual(PARITY_BATCH_SIZES, (1, 2, 4))
        model = load_checkpoint(self.checkpoint_path).model
        session = parity.open_session(self.onnx_path, "cpu")
        tokens = self._parity_tokens()
        report = parity.check_parity(model, session, tokens, "cpu")
        self.assertEqual(set(report), {"batch1", "batch2", "batch4", "reordered", "repeated"})

        policy, value = parity.onnx_outputs(session, tokens)
        self.assertEqual(policy.shape, (4, POLICY_SIZE))
        self.assertEqual(value.shape, (4, 1))
        self.assertTrue(((value >= 0.0) & (value <= 1.0)).all())
        tolerance = {"rtol": CPU_PARITY_RTOL, "atol": CPU_PARITY_ATOL}

        # 行の並べ替えは出力の並べ替えになる。
        reversed_policy, reversed_value = parity.onnx_outputs(session, tokens[::-1].copy())
        np.testing.assert_allclose(reversed_policy[::-1], policy, **tolerance)
        np.testing.assert_allclose(reversed_value[::-1], value, **tolerance)
        # 同じ行を並べた batch は全行が単独実行と同じ結果になる。
        single_policy, single_value = parity.onnx_outputs(session, tokens[:1])
        repeated_policy, repeated_value = parity.onnx_outputs(session, np.repeat(tokens[:1], 4, axis=0))
        for row in range(4):
            np.testing.assert_allclose(repeated_policy[row : row + 1], single_policy, **tolerance)
            np.testing.assert_allclose(repeated_value[row : row + 1], single_value, **tolerance)
        # 同じ入力の再実行は bit 一致する。
        again_policy, again_value = parity.onnx_outputs(session, tokens)
        self.assertTrue(np.array_equal(policy, again_policy))
        self.assertTrue(np.array_equal(value, again_value))

    def test_parity_detects_a_different_model(self) -> None:
        torch.manual_seed(123)
        other = SquareTransformer(TEST_CONFIG).eval()
        session = parity.open_session(self.onnx_path, "cpu")
        with self.assertRaises(parity.ParityError):
            parity.check_parity(other, session, self._parity_tokens(), "cpu")

    def test_parity_cli(self) -> None:
        arguments = [
            "--checkpoint",
            str(self.checkpoint_path),
            "--onnx",
            str(self.onnx_path),
            "--data",
            str(self.fixture),
            "--provider",
            "cpu",
        ]
        code, stdout, stderr = run_cli(parity.main, arguments)
        self.assertEqual(code, 0, stderr)
        summary = json.loads(stdout)
        self.assertEqual(summary["provider"], "cpu")
        self.assertIn("CPUExecutionProvider", summary["provider_options"])


if __name__ == "__main__":
    unittest.main()
