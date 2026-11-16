from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path

import torch

from rshogi_dl.contract import MAX_LEGAL_LABELS, TOKEN_ID_RANGES, VOCAB_SIZE
from rshogi_dl.data import (
    MAX_LINE_CHARS,
    RecordError,
    collate,
    iter_batches,
    iter_records,
    parse_record,
    validate_record,
)

from .support import empty_board_tokens, synthetic_records, write_jsonl


def valid_record() -> dict:
    return {
        "tokens": empty_board_tokens(),
        "legal_labels": [59, 1660, 7],
        "policy": [[59, 3.0], [1660, 1]],
        "value": 0.5,
    }


def with_token(position: int, token: object) -> dict:
    record = valid_record()
    record["tokens"][position] = token
    return record


def with_field(key: str, value: object) -> dict:
    record = valid_record()
    record[key] = value
    return record


class TokenRangeTest(unittest.TestCase):
    def test_ranges_cover_vocabulary_exactly(self) -> None:
        self.assertEqual(len(TOKEN_ID_RANGES), 96)
        self.assertEqual(TOKEN_ID_RANGES[0], (0, 28))
        self.assertEqual(TOKEN_ID_RANGES[80], (0, 28))
        self.assertEqual(
            TOKEN_ID_RANGES[81:88],
            ((29, 47), (48, 52), (53, 57), (58, 62), (63, 65), (66, 68), (69, 73)),
        )
        self.assertEqual(
            TOKEN_ID_RANGES[88:95],
            ((74, 92), (93, 97), (98, 102), (103, 107), (108, 110), (111, 113), (114, 118)),
        )
        self.assertEqual(TOKEN_ID_RANGES[95], (119, 120))
        covered = set()
        for low, high in TOKEN_ID_RANGES:
            covered.update(range(low, high + 1))
        self.assertEqual(covered, set(range(VOCAB_SIZE)))

    def test_range_boundaries(self) -> None:
        for position, (low, high) in enumerate(TOKEN_ID_RANGES):
            validate_record(with_token(position, low))
            validate_record(with_token(position, high))
            for outside in (low - 1, high + 1):
                with self.subTest(position=position, token=outside):
                    with self.assertRaises(RecordError):
                        validate_record(with_token(position, outside))


class RecordValidationTest(unittest.TestCase):
    def test_valid_records(self) -> None:
        record = validate_record(valid_record())
        self.assertEqual(record.legal_labels, (59, 1660, 7))
        self.assertEqual(record.policy, ((59, 3.0), (1660, 1.0)))
        # 合法だが重み 0 の手は許す。
        validate_record(with_field("policy", [[59, 1], [7, 0]]))
        # policy が null の行は合法手が空でもよい。
        value_only = with_field("policy", None)
        value_only["legal_labels"] = []
        self.assertIsNone(validate_record(value_only).policy)
        for value in (0, 1, 0.0, 1.0):
            validate_record(with_field("value", value))

    def test_invalid_records(self) -> None:
        too_many_labels = with_field("legal_labels", list(range(MAX_LEGAL_LABELS + 1)))
        too_many_labels["policy"] = None
        missing = valid_record()
        del missing["value"]
        extra = valid_record()
        extra["sfen"] = "x"
        no_legal_with_policy = with_field("legal_labels", [])
        cases = {
            "not an object": [],
            "missing key": missing,
            "extra key": extra,
            "tokens too short": with_field("tokens", empty_board_tokens()[:-1]),
            "tokens too long": with_field("tokens", empty_board_tokens() + [119]),
            "tokens not a list": with_field("tokens", "x"),
            "token bool": with_token(0, True),
            "token float": with_token(0, 1.0),
            "token string": with_token(0, "1"),
            "legal not a list": with_field("legal_labels", None),
            "legal duplicate": with_field("legal_labels", [59, 1660, 59]),
            "legal too large": with_field("legal_labels", [59, 1660, 2187]),
            "legal negative": with_field("legal_labels", [59, 1660, -1]),
            "legal bool": with_field("legal_labels", [59, 1660, True]),
            "legal float": with_field("legal_labels", [59, 1660, 7.0]),
            "too many legal labels": too_many_labels,
            "empty policy list": with_field("policy", []),
            "policy without legal labels": no_legal_with_policy,
            "policy illegal label": with_field("policy", [[60, 1.0]]),
            "policy duplicate label": with_field("policy", [[59, 1.0], [59, 1.0]]),
            "policy negative weight": with_field("policy", [[59, 2.0], [7, -1.0]]),
            "policy bool weight": with_field("policy", [[59, True]]),
            "policy bool label": with_field("policy", [[True, 1.0]]),
            "policy all zero": with_field("policy", [[59, 0.0], [7, 0]]),
            "policy infinite weight": with_field("policy", [[59, float("inf")]]),
            "policy nan weight": with_field("policy", [[59, float("nan")]]),
            "policy sum overflow": with_field("policy", [[59, 1.7e308], [7, 1.7e308]]),
            "policy entry shape": with_field("policy", [[59, 1.0, 2.0]]),
            "policy entry not a list": with_field("policy", [59]),
            "value above one": with_field("value", 1.5),
            "value negative": with_field("value", -0.1),
            "value bool": with_field("value", True),
            "value null": with_field("value", None),
            "value nan": with_field("value", float("nan")),
            "value string": with_field("value", "0.5"),
        }
        for name, raw in cases.items():
            with self.subTest(name), self.assertRaises(RecordError):
                validate_record(copy.deepcopy(raw))

    def test_json_text_rejections(self) -> None:
        good = json.dumps(valid_record())
        parse_record(good)
        cases = {
            "nan constant": good.replace("0.5", "NaN"),
            "infinity constant": good.replace("0.5", "Infinity"),
            "overflowing literal": good.replace("0.5", "1e999"),
            "duplicate key": good[:-1] + ', "value": 0.25}',
            "truncated": good[:-1],
            "blank": "\n",
        }
        for name, text in cases.items():
            with self.subTest(name), self.assertRaises(RecordError):
                parse_record(text)


class CollateTest(unittest.TestCase):
    def test_targets_are_normalized_in_legal_order(self) -> None:
        value_only = with_field("policy", None)
        value_only["legal_labels"] = []
        batch = collate([validate_record(valid_record()), validate_record(value_only)])
        self.assertEqual(batch.tokens.shape, (2, 96))
        self.assertEqual(batch.tokens.dtype, torch.int64)
        self.assertEqual(batch.legal_index.tolist(), [[59, 1660, 7], [0, 0, 0]])
        self.assertEqual(batch.legal_mask.tolist(), [[True, True, True], [False, False, False]])
        self.assertEqual(batch.policy_target.tolist(), [[0.75, 0.25, 0.0], [0.0, 0.0, 0.0]])
        self.assertEqual(batch.policy_present.tolist(), [True, False])
        self.assertEqual(batch.value.tolist(), [0.5, 0.5])


class StreamingTest(unittest.TestCase):
    def test_batches_and_drop_last(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.jsonl"
            write_jsonl(path, synthetic_records(seed=1, count=20))
            self.assertEqual(len(list(iter_records(path))), 20)
            sizes = [batch.tokens.shape[0] for batch in iter_batches(path, 8)]
            self.assertEqual(sizes, [8, 8, 4])
            dropped = [batch.tokens.shape[0] for batch in iter_batches(path, 8, drop_last=True)]
            self.assertEqual(dropped, [8, 8])
            for batch in iter_batches(path, 8):
                sums = batch.policy_target.sum(dim=1)
                expected = batch.policy_present.to(torch.float32)
                self.assertTrue(torch.allclose(sums, expected, atol=1e-6))

    def test_reader_does_not_read_ahead(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.jsonl"
            with open(path, "x", encoding="utf-8") as stream:
                for record in synthetic_records(seed=2, count=8):
                    stream.write(json.dumps(record) + "\n")
                stream.write("{broken\n")
            batches = iter_batches(path, 8)
            self.assertEqual(next(batches).tokens.shape[0], 8)
            with self.assertRaisesRegex(RecordError, r":9:"):
                next(batches)

    def test_overlong_line_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.jsonl"
            path.write_text(" " * (MAX_LINE_CHARS + 10) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(RecordError, "exceeds"):
                next(iter_records(path))

    def test_invalid_batch_size(self) -> None:
        with self.assertRaises(ValueError):
            next(iter_batches("unused.jsonl", 0))


if __name__ == "__main__":
    unittest.main()
