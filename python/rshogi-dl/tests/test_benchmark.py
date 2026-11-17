from __future__ import annotations

import contextlib
from dataclasses import replace
import io
import json
import math
import unittest
from pathlib import Path
from unittest.mock import patch

import torch

from rshogi_dl import benchmark
from rshogi_dl.model import TEST_CONFIG
from .support import artifact_tempdir, synthetic_records, write_jsonl


class BenchmarkConfigTest(unittest.TestCase):
    def test_rejects_invalid_or_unbounded_conditions(self) -> None:
        invalid = [
            {"steps": 0}, {"steps": 10001}, {"steps": True}, {"warmup": -1},
            {"batch_size": 0}, {"batch_size": 4097}, {"window_batches": 0},
            {"window_batches": 33}, {"threads": 0}, {"threads": 257}, {"seed": -1},
            {"seed": 2**63}, {"execution": "other"}, {"precision": "fp16"},
            {"attention_backend": "other"}, {"input_mode": "other"}, {"device": "cuda:999"}, {"fused_optimizer": True},
            {"fused_optimizer": 1}, {"learning_rate": 0.0}, {"learning_rate": float("nan")},
            {"weight_decay": -1.0}, {"weight_decay": float("inf")},
        ]
        for options in invalid:
            with self.subTest(options=options), self.assertRaises(ValueError):
                benchmark.BenchmarkConfig(**options)

    def test_cuda_does_not_fallback(self) -> None:
        config = benchmark.BenchmarkConfig(device="cuda")
        with patch.object(torch.cuda, "is_available", return_value=False):
            with self.assertRaisesRegex(benchmark.BenchmarkError, "no CPU fallback"):
                with benchmark._settings(config):
                    self.fail("must reject unavailable CUDA")


    def test_bf16_requires_hardware_support(self) -> None:
        config = benchmark.BenchmarkConfig(device="cuda", precision="bf16")
        with patch.object(torch.cuda, "is_available", return_value=True), \
             patch.object(torch.cuda, "is_bf16_supported", return_value=False) as supported:
            with self.assertRaisesRegex(benchmark.BenchmarkError, "BF16"):
                with benchmark._settings(config):
                    self.fail("must reject emulated BF16")
            supported.assert_called_once_with(including_emulation=False)


class BenchmarkTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.stack = contextlib.ExitStack()
        cls.addClassCleanup(cls.stack.close)
        cls.runs = Path(cls.stack.enter_context(artifact_tempdir()))
        cls.data = cls.runs / "fixture.jsonl"
        write_jsonl(cls.data, synthetic_records(seed=13, count=6))
        cls.config = benchmark.BenchmarkConfig(
            batch_size=2, steps=5, warmup=1, window_batches=4, seed=5
        )
        cls.report = benchmark.run_benchmark(cls.data, TEST_CONFIG, cls.config)

    def test_report_counts_updates_and_contains_finite_measurements(self) -> None:
        report = self.report
        self.assertEqual(report["attempted_steps"], 5)
        self.assertEqual(report["optimizer_updates"], 5)
        self.assertEqual(report["skipped_updates"], 0)
        self.assertEqual(report["window_batches_loaded"], 3)
        self.assertEqual(report["window_samples"], 6)
        self.assertEqual(report["training_status"], "validation-only")
        self.assertIsNone(report["cuda_stream_seconds"])
        self.assertIsNone(report["memory"])
        self.assertTrue(report["parameters_finite"])
        self.assertNotEqual(report["initial_weights_sha256"], report["final_weights_sha256"])
        for key in ("startup_seconds", "warmup_seconds", "measured_wall_seconds",
                    "samples_per_second", "initial_loss", "final_loss"):
            self.assertTrue(math.isfinite(report[key]), key)
            self.assertGreater(report[key], 0, key)
        json.dumps(report, allow_nan=False)

    def test_same_sequence_and_weights_for_stream_and_resident(self) -> None:
        streamed = benchmark.run_benchmark(
            self.data, TEST_CONFIG, replace(self.config, input_mode="stream")
        )
        for key in ("initial_weights_sha256", "final_weights_sha256", "input_window_sha256",
                    "losses_sha256", "initial_loss", "final_loss"):
            self.assertEqual(self.report[key], streamed[key], key)
        other = benchmark.run_benchmark(self.data, TEST_CONFIG, replace(self.config, seed=6))
        self.assertNotEqual(self.report["initial_weights_sha256"], other["initial_weights_sha256"])

    def test_sdpa_uses_the_same_initial_weights_and_loss(self) -> None:
        report = benchmark.run_benchmark(
            self.data, TEST_CONFIG, replace(self.config, attention_backend="sdpa")
        )
        self.assertEqual(report["benchmark_config"]["attention_backend"], "sdpa")
        self.assertEqual(report["initial_weights_sha256"], self.report["initial_weights_sha256"])
        self.assertAlmostEqual(report["initial_loss"], self.report["initial_loss"], places=5)
        self.assertAlmostEqual(report["final_loss"], self.report["final_loss"], places=5)

    def test_no_policy_rows_still_count_updates(self) -> None:
        data = self.runs / "value-only.jsonl"
        rows = synthetic_records(seed=14, count=2)
        for row in rows:
            row["policy"] = None
        write_jsonl(data, rows)
        report = benchmark.run_benchmark(data, TEST_CONFIG, replace(self.config, warmup=0, steps=1))
        self.assertEqual(report["optimizer_updates"], 1)

    def test_short_data_and_failed_training_do_not_return_success(self) -> None:
        short = self.runs / "short.jsonl"
        write_jsonl(short, synthetic_records(seed=15, count=1))
        with self.assertRaisesRegex(benchmark.BenchmarkError, "at least 2"):
            benchmark.run_benchmark(short, TEST_CONFIG, self.config)
        with patch.object(benchmark, "policy_value_loss") as loss:
            loss.return_value.total = torch.tensor(float("nan"))
            with self.assertRaisesRegex(benchmark.BenchmarkError, "initial loss"):
                benchmark.run_benchmark(self.data, TEST_CONFIG, self.config)

    def test_process_settings_are_restored_after_error(self) -> None:
        threads = torch.get_num_threads()
        precision = torch.get_float32_matmul_precision()
        rng = torch.random.get_rng_state()
        with self.assertRaises(FileNotFoundError):
            benchmark.run_benchmark(self.runs / "absent", TEST_CONFIG, self.config)
        self.assertEqual(threads, torch.get_num_threads())
        self.assertEqual(precision, torch.get_float32_matmul_precision())
        self.assertTrue(torch.equal(rng, torch.random.get_rng_state()))

    def test_cli_json_and_exclusive_output(self) -> None:
        output = self.runs / "benchmark.json"
        args = ["--data", str(self.data), "--config", "test", "--batch-size", "2",
                "--steps", "1", "--warmup", "0", "--output", str(output)]
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = benchmark.main(args)
        self.assertEqual(code, 0, stderr.getvalue())
        self.assertEqual(json.loads(stdout.getvalue()), json.loads(output.read_text()))
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(stderr):
            code = benchmark.main(args)
        self.assertEqual(code, 2)
        self.assertIn("overwrite", stderr.getvalue())
