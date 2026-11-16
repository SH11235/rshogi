"""テスト共通の補助。"""

from __future__ import annotations

import json
import os
import random
import tempfile
import unittest
from pathlib import Path

from rshogi_dl import paths
from rshogi_dl.contract import (
    CONTEXT_BLACK_TO_MOVE,
    POLICY_SIZE,
    SQUARE_COUNT,
    TOKEN_ID_RANGES,
)

REQUIRE_INTEGRATION_ENV = "RSHOGI_DL_REQUIRE_INTEGRATION"

HIRATE = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
BARE_KINGS = "4k4/9/9/9/9/9/9/9/4K4"

# (名前, SFEN)。両手番、成り / 不成、7 種の打ち、持ち歩 8 / 9 / 18、手数だけが違う同一盤面を含む。
CORPUS = (
    ("hirate-black", HIRATE),
    ("hirate-black-ply37", HIRATE.replace(" 1", " 37")),
    ("hirate-board-white", HIRATE.replace(" b ", " w ")),
    ("after-7g7f-white", "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2"),
    ("promotions-black", "4k4/9/4P4/L5S2/2N6/9/9/1B5R1/4K4 b - 1"),
    ("promotions-white", "4k4/1r5b1/9/9/6n2/2s5l/4p4/9/4K4 w - 1"),
    ("drops-black-pawn8", f"{BARE_KINGS} b RBGSNL8Prbgsnl9p 1"),
    ("drops-white-pawn9", f"{BARE_KINGS} w RBGSNL8Prbgsnl9p 1"),
    ("hand-max-black", f"{BARE_KINGS} b 2R2B4G4S4N4L18P 1"),
    ("hand-max-white", f"{BARE_KINGS} w 2r2b4g4s4n4l18p 1"),
    ("pawn9-each", f"{BARE_KINGS} b 9P9p 1"),
    (
        "promoted-pieces-white",
        "4k4/8+P/+B+R+S+N+L4/9/9/9/+p+l+n+s+b+r3/9/4K4 w - 1",
    ),
    (
        "middlegame-black",
        "ln1g1g1nl/1ks2r3/1pppp1bpp/p4pp2/9/P1P1P1P2/1P1PSP1PP/1BK1GR3/LN1G3NL b Ss 30",
    ),
    ("in-check-black", "4k4/9/9/9/9/9/9/4r4/4K4 b G 1"),
)

_DROP_PIECES = "PLNSBRG"
_DIRECTION_CHANNELS = {
    (0, -1): 0,
    (1, -1): 1,
    (-1, -1): 2,
    (1, 0): 3,
    (-1, 0): 4,
    (0, 1): 5,
    (1, 1): 6,
    (-1, 1): 7,
}


def _sign(value: int) -> int:
    return (value > 0) - (value < 0)


def label_from_usi(usi: str, white_to_move: bool) -> int:
    """USI の指し手文字列だけから policy label を求める、engine と独立な参照実装。"""

    def square(text: str) -> int:
        index = 9 * (int(text[0]) - 1) + (ord(text[1]) - ord("a"))
        return 80 - index if white_to_move else index

    destination = square(usi[2:4])
    if usi[1] == "*":
        return SQUARE_COUNT * (20 + _DROP_PIECES.index(usi[0])) + destination
    origin = square(usi[0:2])
    # 筋の番号が増える向きが、手番側から見た左。段が減る向きが前。
    left = destination // 9 - origin // 9
    down = destination % 9 - origin % 9
    if down == -2 and abs(left) == 1:
        channel = 8 if left == 1 else 9
    else:
        channel = _DIRECTION_CHANNELS[(_sign(left), _sign(down))]
    if usi.endswith("+"):
        channel += 10
    return SQUARE_COUNT * channel + destination


def empty_board_tokens() -> list[int]:
    """全マスが空・持駒なし・先手番の token 列 (範囲の検査だけを通る最小の入力)。"""
    tokens = [low for low, _ in TOKEN_ID_RANGES]
    assert tokens[-1] == CONTEXT_BLACK_TO_MOVE
    return tokens


def random_tokens(rng: random.Random) -> list[int]:
    return [rng.randint(low, high) for low, high in TOKEN_ID_RANGES]


def synthetic_records(seed: int, count: int) -> list[dict]:
    """位置ごとの ID 範囲だけを満たす乱数 sample。4 行に 1 行は policy が null。"""
    rng = random.Random(seed)
    records = []
    for index in range(count):
        legal = rng.sample(range(POLICY_SIZE), rng.randint(1, 40))
        if index % 4 == 3:
            policy = None
        else:
            chosen = rng.sample(legal, rng.randint(1, len(legal)))
            policy = [[label, rng.randint(1, 100)] for label in chosen]
        records.append(
            {
                "tokens": random_tokens(rng),
                "legal_labels": legal,
                "policy": policy,
                "value": rng.randint(0, 1000) / 1000,
            }
        )
    return records


def write_jsonl(path: Path, records: list[dict]) -> None:
    with open(path, "x", encoding="utf-8") as stream:
        for record in records:
            stream.write(json.dumps(record) + "\n")


def artifact_tempdir(root_index: int = 0) -> tempfile.TemporaryDirectory:
    """成果物を置ける subtree の下に一時 directory を作る。"""
    root = paths.allowed_roots()[root_index]
    root.mkdir(parents=True, exist_ok=True)
    return tempfile.TemporaryDirectory(dir=root, prefix="unittest-")


def integration_required() -> bool:
    return os.environ.get(REQUIRE_INTEGRATION_ENV) == "1"


def skip_or_fail(test: unittest.TestCase, reason: str) -> None:
    """統合テストの前提が欠けたら skip する。必須指定の環境では失敗にする。"""
    if integration_required():
        test.fail(f"{reason} ({REQUIRE_INTEGRATION_ENV}=1)")
    test.skipTest(reason)


def env_binary(test: unittest.TestCase, name: str) -> str:
    value = os.environ.get(name)
    if not value:
        skip_or_fail(test, f"{name} is not set")
    if not os.path.isfile(value):
        test.fail(f"{name} does not point to a file: {value}")
    return value
