"""教師全体を決定的に並べ替え、順次読み込み可能な学習 shard を作る。"""

from __future__ import annotations

import argparse
from collections.abc import Iterable, Iterator
from contextlib import closing, contextmanager
from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
from struct import Struct
import sys
from tempfile import TemporaryDirectory
from typing import Callable

import numpy as np

from . import hcpe3_stream, sharded
from ._shuffle_store import iter_shuffled_rows, partition_rows
from .contract import MAX_LEGAL_LABELS, TOKEN_COUNT
from .hcpe3_rows import assemble_shards
from .paths import shogi_data_root

_SHARD_RECORDS = 32768
_INPUT_BATCH = 4096
_ROW_HEADER = Struct(f"<{TOKEN_COUNT}sf?H")
_FORMAT = "rshogi-dl-full-shuffle-v1"
_MAX_RECEIPT_BYTES = 64 << 20


@dataclass(frozen=True)
class _Source:
    identity: dict
    records: Callable[[], Iterator[bytes]]
    size: int
    teacher_kind: str
    split: str
    training_status: str


def _encode_rows(columns, count):
    offsets = columns["legal_offsets"]
    indices = columns["legal_indices"]
    targets = columns["policy_targets"]
    for row in range(count):
        start, end = int(offsets[row]), int(offsets[row + 1])
        length = end - start
        if not 0 <= length <= MAX_LEGAL_LABELS:
            raise sharded.DatasetError("invalid legal label count in shuffle input")
        yield (_ROW_HEADER.pack(columns["tokens"][row].tobytes(), columns["values"][row],
                                columns["policy_present"][row], length)
               + indices[start:end].tobytes() + targets[start:end].tobytes())


def _decode_shards(records: Iterable[bytes], shard_records: int):
    iterator = iter(records)
    while True:
        tokens = np.empty((shard_records, TOKEN_COUNT), dtype=np.uint8)
        values = np.empty(shard_records, dtype="<f4")
        present = np.empty(shard_records, dtype=np.bool_)
        offsets = np.zeros(shard_records + 1, dtype="<u8")
        labels = bytearray()
        targets = bytearray()
        count = 0
        for _ in range(shard_records):
            payload = next(iterator, None)
            if payload is None:
                break
            if len(payload) < _ROW_HEADER.size:
                raise sharded.DatasetError("truncated shuffle row")
            token, value, has_policy, length = _ROW_HEADER.unpack_from(payload)
            if length > MAX_LEGAL_LABELS or len(payload) != _ROW_HEADER.size + 6 * length:
                raise sharded.DatasetError("invalid shuffle row length")
            tokens[count] = np.frombuffer(token, dtype=np.uint8)
            values[count] = value
            present[count] = has_policy
            boundary = _ROW_HEADER.size + 2 * length
            labels.extend(payload[_ROW_HEADER.size:boundary])
            targets.extend(payload[boundary:])
            count += 1
            offsets[count] = offsets[count - 1] + length
        if not count:
            return
        yield {
            "tokens": tokens[:count], "values": values[:count], "policy_present": present[:count],
            "legal_offsets": offsets[:count + 1], "legal_indices": np.frombuffer(labels, dtype="<u2"),
            "policy_targets": np.frombuffer(targets, dtype="<f4"),
        }, count
        if count < shard_records:
            return


def _shard_source(path: Path) -> _Source:
    path = sharded._dataset_path(path, new=False)
    manifest_path = path / "manifest.json"
    signature = sharded._signature(manifest_path)
    manifest = sharded._manifest(path)
    identity = {"type": "shard", "path": str(path), "manifest": sharded._hash(manifest_path)}
    if sharded._signature(manifest_path) != signature:
        raise sharded.DatasetError(f"shard manifest changed: {path}")

    def records():
        digest = hashlib.sha256()
        for index in range(manifest["shard_count"]):
            if sharded._signature(manifest_path) != signature:
                raise sharded.DatasetError(f"shard manifest changed: {path}")
            shard = sharded._Shard(path, index, manifest, verify_hashes=True)
            metadata = sharded._shard_dir(path, index) / "metadata.json"
            digest.update(bytes.fromhex(sharded._hash(metadata)))
            yield from _encode_rows(shard.arrays, shard.rows)
            shard.check_unchanged()
        if digest.hexdigest() != manifest["dataset_content_sha256"]:
            raise sharded.DatasetError(f"dataset content digest mismatch: {path}")

    return _Source(identity, records, manifest["source_size"], manifest["teacher_kind"], manifest["split"],
                   manifest["training_status"])


def _archive_source(encoder, archive, config, protected, protected_games, identity):
    def records():
        frames = hcpe3_stream._archive_rows(encoder, archive, config, protected, protected_games)
        try:
            for columns, count in assemble_shards(
                frames, _INPUT_BATCH, policy_temperature=config.get("policy_temperature", 1.0),
            ):
                yield from _encode_rows(columns, count)
        finally:
            frames.close()

    return _Source(identity, records, archive[2][2], "train-visits", "train", "unknown")


def _manifest_sources(path: Path):
    _, encoder, config, archives, tokens, games, _, fingerprint = hcpe3_stream._manifest(path)
    for archive in archives:
        identity = {"type": "hcpe3-manifest", "manifest": fingerprint, "path": str(archive[0])}
        yield _archive_source(encoder, archive, config, tokens, games, identity)


def _sources(paths, encoder, value_eval_weight, value_eval_scale, policy_temperature, prior):
    raw_config = {"heldout_modulus": 0, "value_eval_weight": value_eval_weight,
                  "value_eval_scale": value_eval_scale, "policy_temperature": policy_temperature}
    prior_archives = {row["path"]: row for row in prior if row["type"] == "hcpe3"}
    encoder_identity = None
    on_disk = set()
    for path in paths:
        path = Path(path).resolve(strict=True)
        if path.is_dir():
            sources = (_shard_source(path),)
        elif path.name.endswith((".hcpe3", ".hcpe3.xz")):
            if not path.is_file() or not path.is_relative_to(shogi_data_root()):
                raise sharded.DatasetError("HCPE3 input must be a file below SHOGI_DATA")
            if encoder is None:
                raise sharded.DatasetError("raw HCPE3 input requires --encoder")
            if encoder_identity is None:
                encoder = hcpe3_stream._regular_file(encoder)
                encoder_identity = (str(encoder), sharded._hash(encoder))
            signature = sharded._signature(path)
            saved = prior_archives.get(str(path))
            if saved is not None and saved["signature"] != list(signature):
                raise sharded.DatasetError(f"HCPE3 input changed: {path}")
            digest = saved["sha256"] if saved is not None else sharded._hash(path)
            identity = {"type": "hcpe3", "path": str(path), "sha256": digest,
                        "signature": list(signature), "encoder": list(encoder_identity), "settings": raw_config}
            sources = (_archive_source(encoder, (path, digest, signature), raw_config, set(), set(), identity),)
        elif path.suffix == ".json":
            sources = _manifest_sources(path)
        else:
            raise sharded.DatasetError(f"unsupported shuffle input: {path}")
        for source in sources:
            key = source.identity["path"]
            if key in on_disk:
                raise sharded.DatasetError(f"duplicate shuffle input: {key}")
            on_disk.add(key)
            yield source


@contextmanager
def _work_lock(work):
    with (work / "lock").open("a+b") as lock:
        if sys.platform == "win32":
            import msvcrt

            lock.seek(0)
            msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl

            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def _open_work(path):
    if path.is_symlink():
        raise sharded.DatasetError("shuffle work directory must not be a symlink")
    if path.exists():
        return sharded._dataset_path(path, new=False)
    work = sharded._dataset_path(path, new=True)
    try:
        sharded._initialize_output(work)
    except FileExistsError:
        if not work.is_dir():
            raise
    return work


def _same_receipt(path, identity):
    if _read_receipt(path) != identity:
        raise sharded.DatasetError(f"shuffle configuration changed: {path}")


def _read_receipt(path):
    return sharded._read_json(path, max_bytes=_MAX_RECEIPT_BYTES)


def _write_receipt(path, identity):
    sharded._write_json(path, identity)
    if path.stat().st_size > _MAX_RECEIPT_BYTES:
        raise sharded.DatasetError("shuffle receipt exceeds the supported size")


def _resume_output(output, manifest):
    content = hashlib.sha256()
    rows = 0
    start = 0
    for index in range(manifest["shard_count"]):
        directory = sharded._shard_dir(output, index)
        if not directory.exists():
            break
        shard = sharded._Shard(output, index, manifest, verify_hashes=True,
                                manifest_name="shuffle.json")
        content.update(bytes.fromhex(sharded._hash(directory / "metadata.json")))
        rows += shard.rows
        start += 1
    if any(p.name.startswith("shard-") and p.name >= f"shard-{start:08d}"
           for p in output.iterdir()):
        raise sharded.DatasetError("shuffle output has noncontiguous shards")
    return start, rows, content


def _write_shard(output, index, columns, count, compressor):
    with TemporaryDirectory(prefix=".pending-", dir=output) as temporary:
        temporary = Path(temporary)
        digest = sharded._write_shard(temporary, index, columns, count, compressor)
        sharded._shard_dir(temporary, index).rename(sharded._shard_dir(output, index))
        sharded._sync_directory(output)
        return digest


def _create_output(output, identity):
    output.parent.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(prefix=".shuffle-init-", dir=output.parent) as temporary:
        staged = Path(temporary) / "output"
        staged.mkdir()
        _write_receipt(staged / "shuffle.json", identity)
        sharded._sync_directory(staged)
        staged.rename(output)
    root = shogi_data_root()
    for parent in output.parents:
        sharded._sync_directory(parent)
        if parent == root:
            break


def _write_output(output, store, rows, identity, sources, shard_records, compressor):
    kinds = {s.teacher_kind for s in sources}
    manifest = {
        **sharded._CONTRACT,
        "format": sharded.ZSTD_FORMAT,
        "source_name": f"full-shuffle:{identity['seed']}",
        "source_size": sum(s.size for s in sources),
        "dataset_content_sha256": "",
        "source_records": rows,
        "shard_records": shard_records,
        "shard_count": (rows + shard_records - 1) // shard_records,
        "teacher_kind": kinds.pop() if len(kinds) == 1 else "mixed",
        "split": sources[0].split,
        "training_status": ("validation-only" if any(s.training_status == "validation-only" for s in sources)
                            else "unknown"),
    }
    start, written, content = _resume_output(output, manifest)
    # Windows では directory の fsync ができないため、途中の一時区画を保持する。
    committed_rows = None if os.name == "nt" else lambda: written
    with closing(iter_shuffled_rows(store, start_row=written, committed_rows=committed_rows)) as records:
        for index, (columns, count) in enumerate(_decode_shards(records, shard_records), start):
            digest = _write_shard(output, index, columns, count, compressor)
            content.update(bytes.fromhex(digest))
            written += count
    if written != rows:
        raise sharded.DatasetError("shuffle output row count differs from partitioned input")
    manifest["dataset_content_sha256"] = content.hexdigest()
    (output / "manifest.tmp").unlink(missing_ok=True)
    sharded._publish_manifest(output, manifest)
    return manifest


def shuffle_dataset(sources, output, *, seed: int, work=None, encoder=None, partition_bits: int = 8,
                    shard_records: int = _SHARD_RECORDS, compression_level: int = 3,
                    value_eval_weight: float = 0.0, value_eval_scale: float = 0.0013226,
                    policy_temperature: float = 1.0) -> dict:
    """入力全体を一度並べ替える。同じ入力・seed・出力先で中断した作成を再開できる。"""
    sharded._positive_int(shard_records, "shard_records", sharded.MAX_SHARD_RECORDS)
    if type(seed) is not int or not 0 <= seed < 2**64:
        raise sharded.DatasetError("seed must be in 0..2**64-1")
    if type(partition_bits) is not int or not 0 <= partition_bits <= 8:
        raise sharded.DatasetError("partition bits must be in 0..8")
    if not (0 <= value_eval_weight <= 1 and 0 < value_eval_scale < math.inf and 0 <= policy_temperature < math.inf):
        raise sharded.DatasetError("invalid raw HCPE3 teacher parameters")
    compressor = sharded._compressor(compression_level)
    requested = Path(output)
    if requested.is_symlink():
        raise sharded.DatasetError("shuffle output must not be a symlink")
    output = sharded._dataset_path(requested, new=not requested.exists())
    work = Path(work) if work is not None else output.with_name(f"{output.name}.shuffle-work")
    if work.resolve().is_relative_to(output) or output.is_relative_to(work.resolve()):
        raise sharded.DatasetError("shuffle work and output must be separate directories")
    work = _open_work(work)

    with _work_lock(work):
        receipt = work / "input.json"
        previous = _read_receipt(receipt) if receipt.exists() else {}
        if not previous and any(
            path.name != "lock" and
            (not path.name.startswith(".shuffle-receipt-") or path.is_symlink() or not path.is_dir())
            for path in work.iterdir()
        ):
            raise sharded.DatasetError("shuffle work directory is not empty and has no input receipt")
        inputs = list(_sources(sources, encoder, value_eval_weight, value_eval_scale, policy_temperature,
                               previous.get("sources", ())))
        if not inputs:
            raise sharded.DatasetError("shuffle requires at least one input")
        if len({entry.split for entry in inputs}) != 1:
            raise sharded.DatasetError("shuffle inputs must use the same training or validation split")
        identity = {
            "format": _FORMAT, "output": str(output), "work": str(work), "seed": seed,
            "partition_bits": partition_bits, "shard_records": shard_records,
            "compression_level": compression_level, "sources": [entry.identity for entry in inputs],
        }
        if previous:
            _same_receipt(receipt, identity)
        else:
            _save_receipt(receipt, identity)

        output_receipt = output / "shuffle.json"
        if output.exists():
            _same_receipt(output_receipt, identity)
        else:
            _create_output(output, identity)
        store = work / "store"
        if store.is_symlink():
            raise sharded.DatasetError("shuffle store must not be a symlink")
        if (output / "manifest.json").is_file():
            result = sharded._manifest(output)
        else:
            for pending in output.glob(".pending-*"):
                if pending.is_dir() and not pending.is_symlink():
                    shutil.rmtree(pending)
            store.mkdir(exist_ok=True)
            rows = partition_rows(store, (source.records() for source in inputs), seed=seed,
                                  partition_bits=partition_bits)
            if not rows:
                raise sharded.DatasetError("shuffle requires at least one retained row")
            result = _write_output(output, store, rows, identity, inputs, shard_records, compressor)
        if store.is_dir() and not store.is_symlink():
            shutil.rmtree(store)
        return result


def _save_receipt(path, identity):
    with TemporaryDirectory(prefix=".shuffle-receipt-", dir=path.parent) as temporary:
        staged = Path(temporary) / path.name
        _write_receipt(staged, identity)
        staged.replace(path)
    sharded._sync_directory(path.parent)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sources", metavar="INPUT", nargs="+",
                        help="HCPE3、HCPE3.XZ、HCPE3学習manifest、または学習shardディレクトリ")
    parser.add_argument("--output", required=True)
    parser.add_argument("--work", help="途中ファイルの置き場。既定は出力と同じ階層に置く")
    parser.add_argument("--seed", type=int, required=True)
    parser.add_argument("--encoder", help="直接指定したHCPE3原本を読む hcpe3_to_dl_rows")
    parser.add_argument("--partition-bits", type=int, default=8)
    parser.add_argument("--shard-records", type=int, default=_SHARD_RECORDS)
    parser.add_argument("--compression-level", type=int, default=3)
    parser.add_argument("--value-eval-weight", type=float, default=0.0)
    parser.add_argument("--value-eval-scale", type=float, default=0.0013226)
    parser.add_argument("--policy-temperature", type=float, default=1.0)
    args = vars(parser.parse_args(argv))
    try:
        result = shuffle_dataset(**args)
    except (OSError, ValueError, RuntimeError) as error:
        parser.exit(2, f"{parser.prog}: {error}\n")
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
