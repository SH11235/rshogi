from __future__ import annotations

import json
import os
import stat
import unittest
from dataclasses import fields
from pathlib import Path
from unittest import mock

import numpy as np
import torch

from rshogi_dl import data, sharded
from rshogi_dl.contract import MAX_LEGAL_LABELS
from rshogi_dl.loss import policy_value_loss
from rshogi_dl.paths import ArtifactPathError, shogi_data_root
from rshogi_dl.sharded import DatasetError, convert_jsonl, iter_batches, verify_dataset

from .support import artifact_tempdir, empty_board_tokens, synthetic_records, write_jsonl


def special_records() -> list[dict]:
    records = synthetic_records(seed=13, count=11)
    records[0].update(legal_labels=[], policy=None)
    records[1].update(legal_labels=[9, 7, 4], policy=None)
    records[2].update(legal_labels=[8, 3, 17], policy=[[17, 5], [8, 0]])
    records[3].update(legal_labels=list(range(MAX_LEGAL_LABELS)), policy=[[592, 3], [0, 1]])
    return records


class ShardedTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = artifact_tempdir()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source.jsonl"
        self.output = self.root / "dataset"

    def convert(self, records: list[dict], *, shard_records: int = 3) -> dict:
        write_jsonl(self.source, records)
        return convert_jsonl(self.source, self.output, shard_records=shard_records)

    def test_jsonl_binary_batch_and_loss_parity_across_shards(self) -> None:
        manifest = self.convert(special_records())
        self.assertEqual(manifest["shard_count"], 4)
        self.assertEqual(manifest["source_records"], 11)
        self.assertEqual(manifest["source_size"], self.source.stat().st_size)
        self.assertEqual(manifest["teacher_kind"], "unknown")
        self.assertEqual(manifest["split"], "unknown")
        self.assertEqual(manifest["training_status"], "unknown")
        self.assertNotIn("shards", manifest)
        for size in (1, 2, 4, 12):
            for drop_last in (False, True):
                with self.subTest(batch=size, drop_last=drop_last):
                    expected = list(data.iter_batches(self.source, size, drop_last=drop_last))
                    actual = list(iter_batches(self.output, size, drop_last=drop_last))
                    self.assertEqual(len(expected), len(actual))
                    for want, got in zip(expected, actual, strict=True):
                        for field in fields(data.Batch):
                            self.assertTrue(
                                torch.equal(getattr(want, field.name), getattr(got, field.name)),
                                field.name,
                            )
                        logits = torch.randn(want.tokens.shape[0], 2187)
                        value = torch.randn(want.tokens.shape[0], 1)
                        self.assertTrue(torch.equal(
                            policy_value_loss(logits, value, want).total,
                            policy_value_loss(logits, value, got).total,
                        ))
        self.assertEqual(verify_dataset(self.output), manifest)

    def test_empty_legal_arrays_and_repeatable_owned_batches(self) -> None:
        records = [
            {"tokens": empty_board_tokens(), "legal_labels": [], "policy": None, "value": value}
            for value in (0.0, 0.5, 1.0)
        ]
        self.convert(records, shard_records=1)
        batches = iter_batches(self.output, 2)
        first = next(batches)
        snapshot = first.value.clone()
        second = next(batches)
        self.assertEqual(first.legal_index.shape, (2, 1))
        self.assertFalse(first.legal_mask.any())
        self.assertEqual(second.value.tolist(), [1.0])
        self.assertTrue(torch.equal(first.value, snapshot))
        self.assertEqual([batch.value.tolist() for batch in iter_batches(self.output, 2)], [[0.0, 0.5], [1.0]])
        self.assertEqual(verify_dataset(self.output)["source_records"], 3)

    def test_ragged_storage_and_bounded_conversion(self) -> None:
        write_jsonl(self.source, synthetic_records(seed=0, count=1))
        record = data.validate_record({
            "tokens": empty_board_tokens(), "legal_labels": [3, 9],
            "policy": [[9, 1]], "value": 0.5,
        })
        def records():
            for index in range(7):
                if index == 2:
                    self.assertTrue((self.output / "shard-00000000" / "metadata.json").is_file())
                    self.assertFalse((self.output / "manifest.json").exists())
                yield record
        with mock.patch.object(sharded, "iter_records", side_effect=lambda _: records()):
            manifest = convert_jsonl(self.source, self.output, shard_records=2)
        self.assertEqual(manifest["shard_count"], 4)
        directory = self.output / "shard-00000000"
        self.assertEqual(np.load(directory / "tokens.npy").dtype, np.uint8)
        self.assertEqual(np.load(directory / "legal_offsets.npy").tolist(), [0, 2, 4])
        self.assertEqual(np.load(directory / "legal_indices.npy").shape, (4,))
        self.assertEqual(np.load(directory / "policy_targets.npy").shape, (4,))
        self.assertEqual(np.load(directory / "policy_targets.npy").tolist(), [0.0, 1.0, 0.0, 1.0])

    def test_records_are_validated_once_and_not_on_each_epoch(self) -> None:
        records = synthetic_records(seed=8, count=7)
        write_jsonl(self.source, records)
        with mock.patch.object(data, "validate_record", wraps=data.validate_record) as validate:
            convert_jsonl(self.source, self.output, shard_records=2)
            self.assertEqual(validate.call_count, len(records))
        with (
            mock.patch.object(data, "validate_record", side_effect=AssertionError("no record scan")),
            mock.patch.object(sharded, "_hash", side_effect=AssertionError("no default hash scan")),
        ):
            for _ in range(2):
                self.assertEqual(sum(batch.tokens.shape[0] for batch in iter_batches(self.output, 3)), 7)

    def test_failed_conversion_is_not_a_readable_dataset(self) -> None:
        with self.source.open("x", encoding="utf-8") as stream:
            for record in synthetic_records(seed=2, count=2):
                stream.write(json.dumps(record) + "\n")
            stream.write("{broken\n")
        with self.assertRaises(data.RecordError):
            convert_jsonl(self.source, self.output, shard_records=2)
        self.assertTrue((self.output / "shard-00000000" / "metadata.json").is_file())
        self.assertFalse((self.output / "manifest.json").exists())
        with self.assertRaises(DatasetError):
            next(iter_batches(self.output, 1))
        with self.assertRaises(ArtifactPathError):
            convert_jsonl(self.source, self.output)

    def test_limits_and_empty_source(self) -> None:
        for value in (0, -1, True, 65537):
            with self.subTest(shard_records=value), self.assertRaises(DatasetError):
                convert_jsonl(self.source, self.output, shard_records=value)
        for value in (0, -1, True, 16385):
            with self.subTest(batch_size=value), self.assertRaises(DatasetError):
                next(iter_batches(self.output, value))
        write_jsonl(self.source, [])
        with self.assertRaisesRegex(DatasetError, "empty"):
            convert_jsonl(self.source, self.output)
        self.assertFalse((self.output / "manifest.json").exists())

    def test_explicit_provenance_does_not_change_model_training_status(self) -> None:
        write_jsonl(self.source, synthetic_records(seed=0, count=1))
        manifest = convert_jsonl(
            self.source, self.output, teacher_kind="synthetic-validation-targets",
            split="fixture-only", training_status="validation-only",
        )
        self.assertEqual(manifest["training_status"], "validation-only")
        self.assertEqual(manifest["teacher_kind"], "synthetic-validation-targets")
        self.assertEqual(manifest["split"], "fixture-only")
        from rshogi_dl.contract import TRAINING_STATUS
        self.assertEqual(TRAINING_STATUS, "validation-only")

    def test_output_path_rules_and_overwrite(self) -> None:
        write_jsonl(self.source, synthetic_records(seed=0, count=1))
        root = shogi_data_root()
        for path in (
            root / "teachers" / "dl",
            root / "teachers" / "other" / "dataset",
            root / "dl" / "transformer-v1-validation" / "dataset",
            Path("relative"),
        ):
            with self.subTest(path=path), self.assertRaises(ArtifactPathError):
                convert_jsonl(self.source, path)
        convert_jsonl(self.source, self.output)
        with self.assertRaises(ArtifactPathError):
            convert_jsonl(self.source, self.output)
        link = self.root / "escaped"
        link.symlink_to(root / "nnue", target_is_directory=True)
        with self.assertRaises(ArtifactPathError):
            convert_jsonl(self.source, link / "dataset")
        package = Path(__file__).resolve().parents[1]
        inside = package / "shogi-data"
        with (
            mock.patch.dict(os.environ, {"SHOGI_DATA": str(inside)}),
            self.assertRaisesRegex(ArtifactPathError, "repository"),
        ):
            convert_jsonl(self.source, inside / "teachers" / "dl" / "dataset")

    def test_invalid_manifest_contract_counts_and_names(self) -> None:
        manifest = self.convert(synthetic_records(seed=0, count=3))
        path = self.output / "manifest.json"
        cases = (
            {**manifest, "format": "unknown"}, {**manifest, "shard_count": 99},
            {**manifest, "source_records": True}, {**manifest, "extra": 0},
            {**manifest, "training_status": "trained"},
        )
        for value in cases:
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.subTest(value=value), self.assertRaises(DatasetError):
                next(iter_batches(self.output, 1))
        path.write_text('{"format": "a", "format": "b"}', encoding="utf-8")
        with self.assertRaises(DatasetError):
            next(iter_batches(self.output, 1))

    def test_resized_array_wrong_dtype_and_hash_verification(self) -> None:
        self.convert(synthetic_records(seed=0, count=3))
        directory = self.output / "shard-00000000"
        path = directory / "tokens.npy"
        original = path.read_bytes()
        path.write_bytes(original[:-1])
        with self.assertRaises(DatasetError):
            next(iter_batches(self.output, 1))
        path.write_bytes(original)
        mapped = np.load(path, mmap_mode="r+")
        mapped[0, 0] = (int(mapped[0, 0]) + 1) % 29
        mapped.flush()
        del mapped
        with self.assertRaisesRegex(DatasetError, "checksum"):
            verify_dataset(self.output)
        with self.assertRaisesRegex(DatasetError, "checksum"):
            next(iter_batches(self.output, 1, verify_hashes=True))
        values = np.zeros((3, 96), dtype=np.int8)
        with path.open("wb") as stream:
            np.save(stream, values)
        metadata_path = directory / "metadata.json"
        metadata = json.loads(metadata_path.read_text())
        metadata["files"]["tokens"]["size"] = path.stat().st_size
        metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
        with self.assertRaisesRegex(DatasetError, "layout"):
            next(iter_batches(self.output, 1))

    def test_array_and_manifest_mutation_during_iteration_are_rejected(self) -> None:
        self.convert(synthetic_records(seed=0, count=3))
        batches = iter_batches(self.output, 1)
        next(batches)
        path = self.output / "shard-00000000" / "values.npy"
        with path.open("ab") as stream:
            stream.write(b"x")
        with self.assertRaisesRegex(DatasetError, "changed"):
            next(batches)

    def test_symlinked_shard_and_array_are_rejected(self) -> None:
        self.convert(synthetic_records(seed=0, count=3))
        directory = self.output / "shard-00000000"
        path = directory / "values.npy"
        moved = directory / "values-original.npy"
        path.rename(moved)
        path.symlink_to(moved)
        with self.assertRaises(DatasetError):
            next(iter_batches(self.output, 1))

    def test_later_shards_are_opened_lazily(self) -> None:
        self.convert(synthetic_records(seed=1, count=5), shard_records=2)
        (self.output / "shard-00000002" / "values.npy").unlink()
        batches = iter_batches(self.output, 1)
        for _ in range(4):
            self.assertEqual(next(batches).tokens.shape, (1, 96))
        with self.assertRaises(DatasetError):
            next(batches)

    def test_teacher_dataset_subtree_is_accepted(self) -> None:
        write_jsonl(self.source, synthetic_records(seed=1, count=1))
        with mock.patch.dict(os.environ, {"SHOGI_DATA": str(self.root)}):
            output = self.root / "teachers" / "dl" / "dataset"
            convert_jsonl(self.source, output)
            self.assertEqual(next(iter_batches(output, 1)).tokens.shape, (1, 96))

    def test_content_digest_identifies_data_without_a_shard_list(self) -> None:
        write_jsonl(self.source, special_records())
        first = self.root / "digest-first"
        second = self.root / "digest-second"
        a = convert_jsonl(self.source, first, shard_records=3)
        b = convert_jsonl(self.source, second, shard_records=3)
        self.assertEqual(a["dataset_content_sha256"], b["dataset_content_sha256"])
        self.assertEqual(verify_dataset(first), a)
        metadata = first / "shard-00000000" / "metadata.json"
        metadata.write_text(metadata.read_text() + " ", encoding="utf-8")
        with self.assertRaisesRegex(DatasetError, "content digest"):
            verify_dataset(first)

    def test_source_changes_prevent_manifest_publication(self) -> None:
        records = synthetic_records(seed=3, count=3)
        original_reader = sharded.iter_records
        for change in ("append", "truncate", "replace"):
            with self.subTest(change=change):
                source = self.root / f"{change}.jsonl"
                output = self.root / f"{change}-dataset"
                write_jsonl(source, records)
                def changing_reader(path):
                    yield from original_reader(path)
                    if change == "append":
                        with path.open("a", encoding="utf-8") as stream:
                            stream.write(json.dumps(records[0]) + "\n")
                    elif change == "truncate":
                        path.write_bytes(b"")
                    else:
                        replacement = source.with_suffix(".replacement")
                        replacement.write_bytes(source.read_bytes())
                        replacement.replace(source)
                with mock.patch.object(sharded, "iter_records", side_effect=changing_reader):
                    with self.assertRaisesRegex(DatasetError, "source changed"):
                        convert_jsonl(source, output, shard_records=2)
                self.assertFalse((output / "manifest.json").exists())

    def test_bad_empty_array_header_is_rejected_before_loading_or_mapping(self) -> None:
        path = self.root / "bad.npy"
        for header in (
            {"descr": "<u2", "fortran_order": False, "shape": (1 << 50,)},
            {"descr": "<u2", "fortran_order": True, "shape": (0,)},
        ):
            with path.open("wb") as stream:
                np.lib.format.write_array_header_1_0(stream, header)
            with (
                mock.patch.object(np, "load", side_effect=AssertionError("no allocation")),
                mock.patch.object(np, "memmap", side_effect=AssertionError("no mapping")),
                mock.patch.object(np, "empty", side_effect=AssertionError("no allocation")),
            ):
                with self.assertRaisesRegex(DatasetError, "layout"):
                    sharded._read_array(path, np.dtype("<u2"), (0,))
        path.write_bytes(b"\x93NUMPY\x01\x00\xff\xff")
        with self.assertRaisesRegex(DatasetError, "header"):
            sharded._read_array(path, np.dtype("<u2"), (0,))

    def test_file_sync_failure_prevents_manifest_publication(self) -> None:
        write_jsonl(self.source, synthetic_records(seed=0, count=1))
        original_fsync = os.fsync
        def failing_file_sync(descriptor):
            if stat.S_ISREG(os.fstat(descriptor).st_mode):
                raise OSError("simulated file sync failure")
            original_fsync(descriptor)
        with mock.patch.object(os, "fsync", side_effect=failing_file_sync):
            with self.assertRaisesRegex(OSError, "sync failure"):
                convert_jsonl(self.source, self.output)
        self.assertFalse((self.output / "manifest.json").exists())


if __name__ == "__main__":
    unittest.main()
