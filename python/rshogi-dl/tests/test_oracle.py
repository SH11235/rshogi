"""token と policy label を独立した参照実装・engine と照合する。"""

from __future__ import annotations

import json
import os
import subprocess
import unittest
from pathlib import Path

import numpy as np
import torch

from rshogi_dl import export, parity
from rshogi_dl.contract import (
    CPU_PARITY_ATOL,
    CPU_PARITY_RTOL,
    POLICY_SIZE,
    TOKEN_COUNT,
)
from rshogi_dl.data import validate_record
from rshogi_dl.model import TEST_CONFIG, SquareTransformer

from .support import CORPUS, HIRATE, artifact_tempdir, env_binary, label_from_usi, skip_or_fail

PROBE_MAX_BATCH = 4
SUBPROCESS_TIMEOUT_SECONDS = 300


def load_oracle(test: unittest.TestCase):
    try:
        from . import cshogi_oracle
    except ImportError as error:
        skip_or_fail(test, f"cshogi is not available: {error}")
    return cshogi_oracle


def run_lines(binary: str, arguments: list[str], sfens: list[str]) -> str:
    completed = subprocess.run(
        [binary, *arguments],
        input="".join(f"{sfen}\n" for sfen in sfens),
        capture_output=True,
        text=True,
        timeout=SUBPROCESS_TIMEOUT_SECONDS,
        check=False,
    )
    if completed.returncode != 0:
        raise AssertionError(f"{binary} exited with {completed.returncode}: {completed.stderr}")
    return completed.stdout


class LabelReferenceTest(unittest.TestCase):
    """手で求めた label との一致。cshogi も engine も使わない。"""

    def test_known_labels(self) -> None:
        cases = (
            # (USI, 後手番か, channel, 正規化後の移動先 = 9 * (筋 - 1) + (段 - 1))
            ("7g7f", False, 0, 59),
            ("3c3d", True, 0, 59),
            ("2h7h", False, 3, 61),
            ("7h2h", False, 4, 16),
            ("8h7g", False, 2, 60),
            ("2h3g", False, 1, 24),
            ("5e5f", False, 5, 41),
            ("5e6f", False, 6, 50),
            ("5e4f", False, 7, 32),
            ("7e8c", False, 8, 65),
            ("7e6c", False, 9, 47),
            ("3e2g", True, 8, 65),
            ("3e4g", True, 9, 47),
            ("5c5b+", False, 10, 37),
            ("8h2b+", False, 12, 10),
            ("7e6c+", False, 19, 47),
            ("5g5h+", True, 10, 37),
            ("P*5e", False, 20, 40),
            ("L*1a", False, 21, 0),
            ("N*9i", False, 22, 80),
            ("S*5e", True, 23, 40),
            ("B*1a", True, 24, 80),
            ("R*9i", True, 25, 0),
            ("G*7f", False, 26, 59),
        )
        for usi, white_to_move, channel, destination in cases:
            with self.subTest(usi=usi, white_to_move=white_to_move):
                self.assertEqual(label_from_usi(usi, white_to_move), 81 * channel + destination)


class CshogiOracleTest(unittest.TestCase):
    def setUp(self) -> None:
        self.oracle = load_oracle(self)
        self.tokens = {name: self.oracle.tokens_from_sfen(sfen) for name, sfen in CORPUS}
        self.labels = {name: self.oracle.legal_labels_from_sfen(sfen) for name, sfen in CORPUS}

    def test_hirate_tokens(self) -> None:
        tokens = self.tokens["hirate-black"]
        self.assertEqual(len(tokens), TOKEN_COUNT)
        # 1 筋: 相手の香 (14+2)・空・相手の歩 (14+1)・空 3 つ・自分の歩・空・自分の香。
        self.assertEqual(tokens[0:9], [16, 0, 15, 0, 0, 0, 1, 0, 2])
        # 2 筋: 相手の桂・相手の角・相手の歩・...・自分の歩・自分の飛・自分の桂。
        self.assertEqual(tokens[9:18], [17, 19, 15, 0, 0, 0, 1, 6, 3])
        # 5 筋: 相手の玉 ... 自分の玉。
        self.assertEqual(tokens[36:45], [22, 0, 15, 0, 0, 0, 1, 0, 8])
        # 8 筋: 相手の桂・相手の飛・相手の歩・...・自分の歩・自分の角・自分の桂。
        self.assertEqual(tokens[63:72], [17, 20, 15, 0, 0, 0, 1, 5, 3])
        self.assertEqual(tokens[81:88], [29, 48, 53, 58, 63, 66, 69])
        self.assertEqual(tokens[88:95], [74, 93, 98, 103, 108, 111, 114])
        self.assertEqual(tokens[95], 119)

    def test_ply_does_not_change_tokens(self) -> None:
        self.assertEqual(self.tokens["hirate-black-ply37"], self.tokens["hirate-black"])
        self.assertEqual(self.labels["hirate-black-ply37"], self.labels["hirate-black"])

    def test_rotated_positions_differ_only_in_context(self) -> None:
        pairs = (
            ("hirate-black", "hirate-board-white"),
            ("promotions-black", "promotions-white"),
            ("hand-max-black", "hand-max-white"),
        )
        for black, white in pairs:
            with self.subTest(black=black, white=white):
                self.assertEqual(self.tokens[black][:95], self.tokens[white][:95])
                self.assertEqual((self.tokens[black][95], self.tokens[white][95]), (119, 120))
                self.assertEqual(
                    sorted(self.labels[black].values()), sorted(self.labels[white].values())
                )

    def test_hand_tokens(self) -> None:
        own_max = [47, 52, 57, 62, 65, 68, 73]
        opponent_empty = [74, 93, 98, 103, 108, 111, 114]
        self.assertEqual(self.tokens["hand-max-black"][81:95], own_max + opponent_empty)
        # 先手: 歩 8 と各 1 枚、後手: 歩 9 と各 1 枚。
        black_view = self.tokens["drops-black-pawn8"]
        self.assertEqual(black_view[81:88], [37, 49, 54, 59, 64, 67, 70])
        self.assertEqual(black_view[88:95], [83, 94, 99, 104, 109, 112, 115])
        white_view = self.tokens["drops-white-pawn9"]
        self.assertEqual(white_view[81:88], [38, 49, 54, 59, 64, 67, 70])
        self.assertEqual(white_view[88:95], [82, 94, 99, 104, 109, 112, 115])
        self.assertEqual(self.tokens["pawn9-each"][81], 38)
        self.assertEqual(self.tokens["pawn9-each"][88], 83)

    def test_corpus_covers_required_move_kinds(self) -> None:
        channels = {name: {label // 81 for label in labels.values()} for name, labels in self.labels.items()}
        for name in ("drops-black-pawn8", "drops-white-pawn9"):
            self.assertTrue(set(range(20, 27)) <= channels[name], name)
        for name in ("promotions-black", "promotions-white"):
            moves = self.labels[name]
            promoted = {usi[:-1] for usi in moves if usi.endswith("+")}
            self.assertTrue(promoted & set(moves), "promotion and non-promotion of the same move")
            self.assertTrue({8, 9, 18, 19} <= channels[name], name)
        self.assertEqual(len(self.labels["hirate-black"]), 30)
        self.assertEqual(self.labels["hirate-black"]["7g7f"], 59)
        self.assertEqual(self.labels["after-7g7f-white"]["3c3d"], 59)

    def test_oracle_output_is_a_valid_record(self) -> None:
        for name, _ in CORPUS:
            with self.subTest(name=name):
                labels = sorted(self.labels[name].values())
                self.assertEqual(len(labels), len(set(labels)))
                validate_record(
                    {
                        "tokens": self.tokens[name],
                        "legal_labels": labels,
                        "policy": [[labels[0], 1.0]],
                        "value": 0.5,
                    }
                )


class EngineEncodingTest(unittest.TestCase):
    def test_engine_tokens_and_labels_match_oracle(self) -> None:
        binary = env_binary(self, "RSHOGI_ENCODE_BIN")
        oracle = load_oracle(self)
        sfens = [sfen for _, sfen in CORPUS]
        lines = run_lines(binary, [], sfens).splitlines()
        self.assertEqual(len(lines), len(CORPUS))
        for (name, sfen), line in zip(CORPUS, lines, strict=True):
            with self.subTest(name=name):
                encoded = json.loads(line)
                self.assertEqual(set(encoded), {"sfen", "tokens", "labels"})
                self.assertEqual(encoded["sfen"].split()[1], sfen.split()[1])
                self.assertEqual(encoded["tokens"], oracle.tokens_from_sfen(sfen))
                engine_labels = {usi: label for usi, label in encoded["labels"]}
                self.assertEqual(len(engine_labels), len(encoded["labels"]))
                self.assertEqual(engine_labels, oracle.legal_labels_from_sfen(sfen))

    def test_engine_output_is_a_valid_record(self) -> None:
        binary = env_binary(self, "RSHOGI_ENCODE_BIN")
        for line in run_lines(binary, [], [HIRATE]).splitlines():
            encoded = json.loads(line)
            labels = [label for _, label in encoded["labels"]]
            validate_record(
                {"tokens": encoded["tokens"], "legal_labels": labels, "policy": None, "value": 0.5}
            )


class EngineModelProbeTest(unittest.TestCase):
    def test_engine_inference_matches_torch(self) -> None:
        binary = env_binary(self, "RSHOGI_MODEL_PROBE_BIN")
        oracle = load_oracle(self)
        torch.manual_seed(21)
        model = SquareTransformer(TEST_CONFIG).eval()
        sfens = [sfen for _, sfen in CORPUS]
        tokens = np.array([oracle.tokens_from_sfen(sfen) for sfen in sfens], dtype=np.int64)
        expected_policy, expected_value = parity.torch_outputs(model, tokens)
        tolerance = {"rtol": CPU_PARITY_RTOL, "atol": CPU_PARITY_ATOL}

        with artifact_tempdir(1) as directory:
            onnx_path = export.export_onnx(model, Path(directory) / "probe.onnx")
            for start in range(0, len(sfens), PROBE_MAX_BATCH):
                chunk = slice(start, start + PROBE_MAX_BATCH)
                with self.subTest(start=start):
                    # 負の device は CPU 推論。
                    stdout = run_lines(
                        binary, [os.fspath(onnx_path), "-1", str(PROBE_MAX_BATCH)], sfens[chunk]
                    )
                    result = json.loads(stdout)
                    policy = np.array(result["policy"], dtype=np.float32)
                    values = np.array(result["values"], dtype=np.float32)
                    rows = len(sfens[chunk])
                    self.assertEqual(policy.shape, (rows, POLICY_SIZE))
                    self.assertEqual(values.shape, (rows,))
                    np.testing.assert_allclose(policy, expected_policy[chunk], **tolerance)
                    np.testing.assert_allclose(values, expected_value[chunk, 0], **tolerance)


if __name__ == "__main__":
    unittest.main()
