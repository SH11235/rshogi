"""HCPE3 の対局境界付き学習行を逐次読む。"""

from collections.abc import Iterator
from dataclasses import dataclass
from itertools import chain, islice
from struct import Struct, unpack_from
from typing import BinaryIO

import numpy as np

from .contract import MAX_LEGAL_LABELS, POLICY_SIZE, TOKEN_COUNT, TOKEN_ID_RANGES


_MAGIC = b"RSDLHC01"
_GAME = Struct("<32sHH")
_POSITION = Struct("<HHHB")
_PLY = Struct("<H")
_TOKEN_MINIMUM, _TOKEN_MAXIMUM = np.asarray(TOKEN_ID_RANGES, dtype=np.uint8).T


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


def _offsets(lengths):
    return np.concatenate((np.zeros(1, dtype="<u8"), np.cumsum(lengths, dtype="<u8")))


def _assemble(rows):
    tokens = np.frombuffer(b"".join(row.tokens for row in rows), dtype=np.uint8).reshape(-1, TOKEN_COUNT)
    if np.any(tokens < _TOKEN_MINIMUM) or np.any(tokens > _TOKEN_MAXIMUM):
        raise Hcpe3RowsError("HCPE3 tokens are outside the allowed ranges")
    legal_lengths = [len(row.legal_labels) for row in rows]
    visit_lengths = [len(row.candidate_visits) // 2 for row in rows]
    legal = np.fromiter(chain.from_iterable(row.legal_labels for row in rows), dtype="<u2", count=sum(legal_lengths))
    flat_visits = np.fromiter(
        chain.from_iterable(row.candidate_visits for row in rows), dtype="<u2", count=2 * sum(visit_lengths),
    )
    visits = flat_visits.reshape(-1, 2)
    if np.any(legal >= POLICY_SIZE) or np.any(visits[:, 0] >= POLICY_SIZE):
        raise Hcpe3RowsError("HCPE3 label is out of range")
    row_keys = np.arange(len(rows), dtype=np.int32) * POLICY_SIZE
    legal_keys = np.repeat(row_keys, legal_lengths) + legal
    visit_keys = np.repeat(row_keys, visit_lengths) + visits[:, 0]
    order = np.argsort(legal_keys)
    legal_sorted = legal_keys[order]
    found = np.searchsorted(legal_sorted, visit_keys)
    if np.any(found == legal.size) or np.any(legal_sorted[found] != visit_keys):
        raise Hcpe3RowsError("HCPE3 candidate is not legal")
    if np.any(legal_sorted[1:] == legal_sorted[:-1]) or np.unique(visit_keys).size != visit_keys.size:
        raise Hcpe3RowsError("HCPE3 labels contain duplicates")
    weights = visits[:, 1]
    totals = np.add.reduceat(weights.astype(np.uint64), _offsets(visit_lengths)[:-1].astype(np.intp))
    target = np.zeros(legal.size, dtype="<f4")
    target[order[found]] = weights.astype(np.float64) / np.repeat(totals, visit_lengths)
    return {
        "tokens": tokens,
        "values": np.fromiter((row.value for row in rows), dtype="<f4", count=len(rows)),
        "policy_present": np.ones(len(rows), dtype=np.bool_),
        "legal_offsets": _offsets(legal_lengths),
        "legal_indices": legal,
        "policy_targets": target,
    }


def assemble_shards(frames: Iterator[PolicyFrame], shard_records: int):
    """学習行を検証し、shardごとの配列にまとめる。"""
    while True:
        blocks, offset, size = [], 0, 0
        while size < shard_records:
            rows = list(islice(frames, min(4096, shard_records - size)))
            if not rows:
                break
            block = _assemble(rows)
            block["legal_offsets"] += offset
            if blocks:
                block["legal_offsets"] = block["legal_offsets"][1:]
            size += len(rows)
            offset += block["legal_indices"].size
            blocks.append(block)
        if not blocks:
            return
        columns = blocks[0] if len(blocks) == 1 else {
            key: np.concatenate([block[key] for block in blocks]) for key in blocks[0]
        }
        yield columns, size
        if size < shard_records:
            return
