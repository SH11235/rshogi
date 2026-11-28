from __future__ import annotations

import json
import lzma
import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest
from unittest import mock

from rshogi_dl import sharded
from rshogi_dl._shuffle_store import iter_shuffled_rows, partition_rows
from rshogi_dl.hcpe3_stream import FORMAT, iter_hcpe3_batches
from rshogi_dl.shuffle_dataset import _create_output, _same_receipt, _save_receipt, _write_shard, shuffle_dataset

from .support import artifact_tempdir, synthetic_records
from .test_hcpe3_stream import _archive, _identity
from .test_sharded import special_records


def _rows(batches):
    for batch in batches:
        for row, legal in enumerate(batch.legal_mask):
            yield (
                tuple(batch.tokens[row].tolist()),
                tuple(batch.legal_index[row, legal].tolist()),
                tuple(batch.policy_target[row, legal].tolist()),
                bool(batch.policy_present[row]),
                float(batch.value[row]),
            )


def _dataset_rows(path, *, start_record=0):
    return list(_rows(sharded.iter_batches(path, 4, start_record=start_record)))


class ShuffleDatasetTest(unittest.TestCase):
    def setUp(self):
        temporary = artifact_tempdir()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def _source(self, name, records, *, compression_level=None):
        path = self.root / name
        sharded.convert_records(
            iter(records), path, source_name=name, source_size=1, shard_records=3,
            compression_level=compression_level,
        )
        return path

    def test_shard_inputs_preserve_ragged_rows_and_duplicates_in_global_order(self):
        first_records = special_records()
        first = self._source("raw", first_records)
        second = self._source(
            "compressed", [first_records[2], *synthetic_records(seed=61, count=17)],
            compression_level=3,
        )
        first_rows = _dataset_rows(first)
        second_rows = _dataset_rows(second)
        expected = first_rows + second_rows

        outputs = []
        for name, seed, partition_bits, shard_records in (
            ("ordered", 42, 3, 5),
            ("repeated", 42, 1, 7),
            ("different-seed", 43, 3, 5),
        ):
            output = self.root / name
            result = shuffle_dataset(
                [first, second], output, seed=seed,
                partition_bits=partition_bits, shard_records=shard_records,
            )
            self.assertEqual(sharded.verify_dataset(output), result)
            self.assertEqual(result["format"], sharded.ZSTD_FORMAT)
            self.assertEqual(result["source_records"], len(expected))
            self.assertEqual(result["shard_count"], (len(expected) + shard_records - 1) // shard_records)
            rows = _dataset_rows(output)
            self.assertCountEqual(rows, expected)
            self.assertEqual(_dataset_rows(output, start_record=shard_records + 1), rows[shard_records + 1:])
            outputs.append(rows)

        ordered, repeated, different_seed = outputs
        self.assertEqual(ordered, repeated)
        self.assertNotEqual(ordered, different_seed)
        self.assertTrue(any(row in second_rows[1:] for row in ordered[:len(first_rows)]))
        self.assertTrue(any(row in first_rows for row in ordered[len(first_rows):]))

    def test_writer_failure_resumes_without_rewriting_committed_shards(self):
        source = self._source("source", special_records())
        output = self.root / "output"
        work = self.root / "work"
        options = {"work": work, "seed": 67, "partition_bits": 2, "shard_records": 3}
        writes = 0

        def interrupted(*args, **kwargs):
            nonlocal writes
            writes += 1
            if writes == 2:
                raise OSError("simulated interruption")
            return _write_shard(*args, **kwargs)

        with (
            mock.patch("rshogi_dl.shuffle_dataset._write_shard", side_effect=interrupted),
            self.assertRaisesRegex(OSError, "simulated interruption"),
        ):
            shuffle_dataset([source], output, **options)
        self.assertEqual(writes, 2)
        self.assertFalse((output / "manifest.json").exists())

        with mock.patch("rshogi_dl.shuffle_dataset._write_shard", wraps=_write_shard) as writer:
            resumed = shuffle_dataset([source], output, **options)
        self.assertEqual(writer.call_count, resumed["shard_count"] - 1)
        self.assertEqual(sharded.verify_dataset(output), resumed)
        reference = self.root / "reference"
        uninterrupted = shuffle_dataset([source], reference, **{**options, "work": None})
        self.assertEqual(resumed["dataset_content_sha256"], uninterrupted["dataset_content_sha256"])
        self.assertEqual(_dataset_rows(output), _dataset_rows(reference))

    def test_unowned_work_store_keeps_existing_files(self):
        source = self._source("source", special_records())
        output = self.root / "output"
        work = self.root / "work"
        foreign_store = work / "store"
        foreign_store.mkdir(parents=True)
        sentinel = foreign_store / "other-artifact"
        sentinel.write_bytes(b"owned by another job")

        with self.assertRaises(sharded.DatasetError):
            shuffle_dataset([source], output, work=work, seed=19, partition_bits=2)
        self.assertEqual(sentinel.read_bytes(), b"owned by another job")
        self.assertFalse((work / "input.json").exists())
        self.assertFalse((output / "manifest.json").exists())

    def test_large_work_and_output_receipts_can_be_read_after_publication(self):
        work = self.root / "large-work"
        work.mkdir()
        output = self.root / "large-output"
        identity = {"sources": [{"path": "archive-" + "x" * 320 + str(i)} for i in range(220)]}
        _save_receipt(work / "input.json", identity)
        _create_output(output, identity)
        for path in (work / "input.json", output / "shuffle.json"):
            self.assertGreater(path.stat().st_size, 65536)
            _same_receipt(path, identity)

    def test_swapped_equal_size_shards_are_rejected_before_publication(self):
        template = synthetic_records(seed=43, count=1)[0]
        source = self._source("source", [{**template, "value": row / 8} for row in range(6)])
        manifest_path = source / "manifest.json"
        original_manifest = manifest_path.read_bytes()
        first = source / "shard-00000000"
        second = source / "shard-00000001"
        temporary = self.root / "swap"
        first.rename(temporary)
        second.rename(first)
        temporary.rename(second)
        self.assertEqual(manifest_path.read_bytes(), original_manifest)
        self.assertEqual(
            sum(batch.tokens.shape[0] for batch in sharded.iter_batches(source, 4, verify_hashes=True)), 6,
        )
        output = self.root / "output"

        with self.assertRaisesRegex(sharded.DatasetError, "digest"):
            shuffle_dataset([source], output, seed=19, partition_bits=2)
        self.assertFalse((output / "manifest.json").exists())

    def test_recursive_partition_sort_preserves_order_and_cursor(self):
        original = [bytes([index % 130]) * (index % 15 + 1) for index in range(260)]
        store = self.root / "store"
        rows = partition_rows(store, (iter(original[:95]), iter(original[95:])), seed=49, partition_bits=1)
        self.assertEqual(rows, len(original))
        expected = list(iter_shuffled_rows(store))
        self.assertCountEqual(expected, original)
        self.assertNotEqual(expected, original)

        self.assertEqual(list(iter_shuffled_rows(store, max_partition_bytes=64)), expected)
        self.assertEqual(
            list(iter_shuffled_rows(store, start_row=127, max_partition_bytes=64)), expected[127:],
        )

    def test_partition_reclamation_keeps_uncommitted_rows_available_for_resume(self):
        original = [f"row-{index}".encode() for index in range(260)]
        store = self.root / "reclamation-store"
        partition_rows(store, (iter(original[:95]), iter(original[95:])), seed=49, partition_bits=2)
        expected = list(iter_shuffled_rows(store))
        fragments = set(store.glob("source-*/partition-*.zst"))
        committed = 0
        shuffled = iter_shuffled_rows(store, committed_rows=lambda: committed)
        try:
            self.assertEqual([next(shuffled) for _ in range(128)], expected[:128])
            self.assertEqual(set(store.glob("source-*/partition-*.zst")), fragments)
            committed = 128
            self.assertEqual([next(shuffled) for _ in range(80)], expected[128:208])
            self.assertLess(set(store.glob("source-*/partition-*.zst")), fragments)
        finally:
            shuffled.close()
        self.assertEqual(list(iter_shuffled_rows(store, start_row=committed)), expected[committed:])

    @unittest.skipUnless(os.name == "posix" and shutil.which("xz"), "requires an executable fixture and xz")
    def test_cli_raw_xz_and_training_manifest_preserve_filtering(self):
        encoder = self.root / "encoder"
        encoder.write_text(
            "#!/usr/bin/env python3\n"
            "from pathlib import Path\n"
            "import sys\n"
            "inputs = [arg for arg in sys.argv[1:] if arg == '-' or arg.endswith('.hcpe3')]\n"
            "for index, path in enumerate(inputs):\n"
            "    content = sys.stdin.buffer.read() if path == '-' else Path(path).read_bytes()\n"
            "    sys.stdout.buffer.write(content if index == 0 else content[8:])\n",
            encoding="utf-8",
        )
        encoder.chmod(0o755)
        raw = self.root / "direct.hcpe3"
        raw.write_bytes(_archive(13, (1, 2, 3)))
        compressed = self.root / "direct.hcpe3.xz"
        compressed.write_bytes(lzma.compress(_archive(43, (4, 5, 6))))
        protected_source = self.root / "manifest.hcpe3"
        protected_source.write_bytes(_archive(96, (10, 11, 12)))
        protected_games = self.root / "games.bin"
        protected_games.write_bytes(bytes([11]) * 32)
        manifest = self.root / "training.json"
        manifest.write_text(
            json.dumps({
                "format": FORMAT,
                "encoder": str(encoder),
                "encoder_sha256": _identity(encoder)["sha256"],
                "archives": [dict(_identity(protected_source), size=protected_source.stat().st_size)],
                "protected_tokens": None,
                "protected_games": _identity(protected_games),
                "heldout_modulus": 0,
                "seed": 17,
            }),
            encoding="utf-8",
        )
        expected_manifest_rows = list(_rows(iter_hcpe3_batches(manifest, 2)))
        self.assertCountEqual([row[1] for row in expected_manifest_rows], [(96,), (98,)])
        output = self.root / "shuffled"
        completed = subprocess.run(
            [
                sys.executable, "-m", "rshogi_dl.shuffle_dataset", str(raw), str(compressed), str(manifest),
                "--encoder", str(encoder), "--output", str(output), "--work", str(self.root / "work"),
                "--seed", "24", "--partition-bits", "2", "--shard-records", "3", "--compression-level", "1",
            ],
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        result = sharded.verify_dataset(output)
        self.assertEqual(result["source_records"], 8)
        rows = _dataset_rows(output)
        self.assertCountEqual([row[1] for row in rows], [(label,) for label in (13, 14, 15, 43, 44, 45, 96, 98)])
        self.assertCountEqual([row for row in rows if row[1][0] >= 96], expected_manifest_rows)
