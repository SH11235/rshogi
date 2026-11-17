"""encode 済み sample の JSONL を検証しながら逐次読む。

1 行 1 sample:
``{"tokens": [96 個], "legal_labels": [...], "policy": null | [[label, weight], ...], "value": 0..1}``
"""

from __future__ import annotations

import json
import math
from collections.abc import Iterator, Sequence
from dataclasses import dataclass
from pathlib import Path

import torch

from .contract import MAX_LEGAL_LABELS, POLICY_SIZE, TOKEN_COUNT, TOKEN_ID_RANGES

# 合法手 593 個に policy の重みを全て付けた行でも収まる長さ。
# 改行のない巨大入力を 1 行として丸ごと読み込まないための上限でもある。
MAX_LINE_CHARS = 65536

_RECORD_KEYS = frozenset({"tokens", "legal_labels", "policy", "value"})


class RecordError(ValueError):
    """sample が形式に合わない。"""


@dataclass(frozen=True)
class Record:
    """検証済みの 1 sample。

    ``policy`` が ``None`` の行は policy loss の対象外というだけで、終局を意味しない。
    """

    tokens: tuple[int, ...]
    legal_labels: tuple[int, ...]
    policy: tuple[tuple[int, float], ...] | None
    value: float


@dataclass(frozen=True)
class Batch:
    """loss 計算用に詰めた batch。

    ``legal_index`` は行ごとの合法 label を左詰めにしたもので、余りは ``legal_mask`` が False。
    ``policy_target`` は ``legal_index`` と同じ並びの正規化済み重み。
    ``policy_rows`` は ``policy_present`` が True の行番号を昇順に並べた int64 Tensor。
    loader は CPU で作り、他の Tensor と同じ device へ転送する。None は未準備の互換入力。
    外部で構築するときも行番号と mask の一致を保ち、mask を変更したら行番号も作り直す。
    """

    tokens: torch.Tensor
    legal_index: torch.Tensor
    legal_mask: torch.Tensor
    policy_target: torch.Tensor
    policy_present: torch.Tensor
    value: torch.Tensor
    policy_rows: torch.Tensor | None = None


def _is_int(value: object) -> bool:
    # bool は int の subclass なので型を厳密に比べる。
    return type(value) is int


def _is_finite_number(value: object) -> bool:
    return type(value) in (int, float) and math.isfinite(value)


def _validate_tokens(raw: object) -> tuple[int, ...]:
    if type(raw) is not list or len(raw) != TOKEN_COUNT:
        raise RecordError(f"tokens must be a list of {TOKEN_COUNT} integers")
    for position, token in enumerate(raw):
        if not _is_int(token):
            raise RecordError(f"tokens[{position}] is not an integer")
        low, high = TOKEN_ID_RANGES[position]
        if not low <= token <= high:
            raise RecordError(
                f"tokens[{position}]={token} is outside the allowed range {low}..{high}"
            )
    return tuple(raw)


def _validate_legal_labels(raw: object) -> tuple[int, ...]:
    if type(raw) is not list:
        raise RecordError("legal_labels must be a list")
    if len(raw) > MAX_LEGAL_LABELS:
        raise RecordError(f"legal_labels has more than {MAX_LEGAL_LABELS} entries")
    for index, label in enumerate(raw):
        if not _is_int(label):
            raise RecordError(f"legal_labels[{index}] is not an integer")
        if not 0 <= label < POLICY_SIZE:
            raise RecordError(f"legal_labels[{index}]={label} is out of range")
    if len(set(raw)) != len(raw):
        raise RecordError("legal_labels contains duplicates")
    return tuple(raw)


def _validate_policy(
    raw: object, legal_labels: tuple[int, ...]
) -> tuple[tuple[int, float], ...] | None:
    if raw is None:
        return None
    if type(raw) is not list or not raw:
        raise RecordError("policy must be null or a non-empty list of [label, weight]")
    legal = set(legal_labels)
    seen: set[int] = set()
    entries: list[tuple[int, float]] = []
    for index, entry in enumerate(raw):
        if type(entry) is not list or len(entry) != 2:
            raise RecordError(f"policy[{index}] must be [label, weight]")
        label, weight = entry
        if not _is_int(label):
            raise RecordError(f"policy[{index}] label is not an integer")
        if label not in legal:
            raise RecordError(f"policy[{index}] label {label} is not a legal label")
        if label in seen:
            raise RecordError(f"policy[{index}] label {label} is duplicated")
        if not _is_finite_number(weight) or weight < 0:
            raise RecordError(f"policy[{index}] weight must be a finite non-negative number")
        seen.add(label)
        entries.append((label, float(weight)))
    try:
        total = math.fsum(weight for _, weight in entries)
    except OverflowError:
        total = math.inf
    if not math.isfinite(total) or total <= 0.0:
        raise RecordError("policy weights must have a finite positive sum")
    return tuple(entries)


def validate_record(raw: object) -> Record:
    """JSON を decode した object を検証して :class:`Record` にする。"""
    if type(raw) is not dict:
        raise RecordError("record must be a JSON object")
    if set(raw) != _RECORD_KEYS:
        raise RecordError(f"record keys must be exactly {sorted(_RECORD_KEYS)}")
    tokens = _validate_tokens(raw["tokens"])
    legal_labels = _validate_legal_labels(raw["legal_labels"])
    policy = _validate_policy(raw["policy"], legal_labels)
    value = raw["value"]
    if not _is_finite_number(value) or not 0.0 <= value <= 1.0:
        raise RecordError("value must be a finite number in [0, 1]")
    return Record(tokens=tokens, legal_labels=legal_labels, policy=policy, value=float(value))


def _reject_constant(name: str) -> object:
    raise RecordError(f"non-finite JSON constant is not allowed: {name}")


def _unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise RecordError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def parse_record(line: str) -> Record:
    """JSONL の 1 行を検証して :class:`Record` にする。"""
    try:
        raw = json.loads(line, parse_constant=_reject_constant, object_pairs_hook=_unique_object)
    except json.JSONDecodeError as error:
        raise RecordError(f"invalid JSON: {error}") from error
    return validate_record(raw)


def iter_records(path: str | Path) -> Iterator[Record]:
    """JSONL を 1 行ずつ読む。保持するのは常に 1 行だけ。"""
    with open(path, encoding="utf-8", newline="\n") as stream:
        line_number = 0
        while True:
            line = stream.readline(MAX_LINE_CHARS + 1)
            if not line:
                return
            line_number += 1
            if len(line) > MAX_LINE_CHARS:
                raise RecordError(f"{path}:{line_number}: line exceeds {MAX_LINE_CHARS} characters")
            try:
                yield parse_record(line)
            except RecordError as error:
                raise RecordError(f"{path}:{line_number}: {error}") from error


def collate(records: Sequence[Record]) -> Batch:
    """sample 列を tensor の batch に詰める。"""
    if not records:
        raise RecordError("cannot collate an empty batch")
    batch_size = len(records)
    width = max(1, max(len(record.legal_labels) for record in records))
    legal_index = torch.zeros((batch_size, width), dtype=torch.int64)
    legal_mask = torch.zeros((batch_size, width), dtype=torch.bool)
    policy_target = torch.zeros((batch_size, width), dtype=torch.float32)
    policy_present = torch.zeros(batch_size, dtype=torch.bool)
    policy_rows: list[int] = []
    for row, record in enumerate(records):
        count = len(record.legal_labels)
        if count:
            legal_index[row, :count] = torch.tensor(record.legal_labels, dtype=torch.int64)
            legal_mask[row, :count] = True
        if record.policy is not None:
            column = {label: index for index, label in enumerate(record.legal_labels)}
            total = math.fsum(weight for _, weight in record.policy)
            for label, weight in record.policy:
                policy_target[row, column[label]] = weight / total
            policy_present[row] = True
            policy_rows.append(row)
    return Batch(
        tokens=torch.tensor([record.tokens for record in records], dtype=torch.int64),
        legal_index=legal_index,
        legal_mask=legal_mask,
        policy_target=policy_target,
        policy_present=policy_present,
        value=torch.tensor([record.value for record in records], dtype=torch.float32),
        policy_rows=torch.tensor(policy_rows, dtype=torch.int64, device="cpu"),
    )


def iter_batches(path: str | Path, batch_size: int, *, drop_last: bool = False) -> Iterator[Batch]:
    """JSONL を batch 単位で逐次読む。保持する sample は最大 ``batch_size`` 個。"""
    if batch_size <= 0:
        raise ValueError("batch_size must be positive")
    pending: list[Record] = []
    for record in iter_records(path):
        pending.append(record)
        if len(pending) == batch_size:
            yield collate(pending)
            pending = []
    if pending and not drop_last:
        yield collate(pending)
