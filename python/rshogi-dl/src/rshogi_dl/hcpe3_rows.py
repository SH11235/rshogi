"""HCPE3 の対局境界付き学習行を逐次読む。"""

from collections.abc import Iterator
from dataclasses import dataclass
from struct import Struct, unpack_from
from typing import BinaryIO

from .contract import MAX_LEGAL_LABELS, POLICY_SIZE, TOKEN_COUNT


_MAGIC = b"RSDLHC01"
_GAME = Struct("<32sHH")
_POSITION = Struct("<HHHB")
_PLY = Struct("<H")


class Hcpe3RowsError(ValueError):
    """対局または学習行の構造が不正。"""


@dataclass(frozen=True, slots=True)
class GameFrame:
    """対局の識別子、着手数、直後に続く学習行数。"""

    key: bytes
    move_count: int
    row_count: int


@dataclass(frozen=True, slots=True)
class PolicyFrame:
    """実着手と訪問数を保持する、手番基準の学習行。"""

    ply: int
    played: int
    tokens: bytes
    value: float
    legal_labels: tuple[int, ...]
    candidate_visits: tuple[int, ...]

    def to_record(self) -> dict:
        """教師 shard が受け付ける行へ変換する。"""
        visits = self.candidate_visits
        policy = [[visits[index], visits[index + 1]] for index in range(0, len(visits), 2)]
        return {
            "tokens": list(self.tokens), "legal_labels": list(self.legal_labels), "policy": policy, "value": self.value,
        }


def _complete(stream: BinaryIO, data: bytes, length: int) -> bytes:
    if len(data) == length:
        return data
    result = bytearray(data)
    while len(result) < length:
        chunk = stream.read(length - len(result))
        if not chunk:
            raise Hcpe3RowsError("truncated HCPE3 policy stream")
        result.extend(chunk)
    return bytes(result)


def _read(stream: BinaryIO, length: int) -> bytes:
    return _complete(stream, stream.read(length), length)


def iter_frames(stream: BinaryIO) -> Iterator[GameFrame | PolicyFrame]:
    """対局ヘッダーに続いて、その対局の学習行を順番に返す。"""
    if _read(stream, len(_MAGIC)) != _MAGIC:
        raise Hcpe3RowsError("unsupported HCPE3 policy stream")
    while raw := stream.read(_GAME.size):
        key, move_count, row_count = _GAME.unpack(_complete(stream, raw, _GAME.size))
        if row_count > move_count:
            raise Hcpe3RowsError("more policy rows than HCPE3 moves")
        yield GameFrame(key, move_count, row_count)
        previous_ply = -1
        for _ in range(row_count):
            ply, = _PLY.unpack(_read(stream, _PLY.size))
            tokens = _read(stream, TOKEN_COUNT)
            played, legal_count, visit_count, result = _POSITION.unpack(_read(stream, _POSITION.size))
            if (
                not previous_ply < ply < move_count or played >= POLICY_SIZE or result > 2
                or not 0 < visit_count <= legal_count <= MAX_LEGAL_LABELS
            ):
                raise Hcpe3RowsError("invalid HCPE3 policy row")
            previous_ply = ply
            payload = _read(stream, 2 * legal_count + 4 * visit_count)
            legal = unpack_from(f"<{legal_count}H", payload)
            visits = unpack_from(f"<{2 * visit_count}H", payload, 2 * legal_count)
            if played not in legal:
                raise Hcpe3RowsError("HCPE3 played label is not legal")
            if 0 in visits[1::2]:
                raise Hcpe3RowsError("HCPE3 policy visit counts must be positive")
            yield PolicyFrame(ply, played, tokens, result / 2, legal, visits)
