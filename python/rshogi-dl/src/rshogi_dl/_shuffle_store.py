"""固定 seed の全行シャッフルを、ディスク上の分割を使って実行する。"""

import hashlib
import json
import os
from collections import deque
from collections.abc import Callable, Iterable, Iterator
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass
from io import BufferedReader, BufferedWriter
from itertools import islice
from pathlib import Path
from shutil import rmtree
from struct import Struct
from tempfile import TemporaryDirectory

_HEADER = Struct(">QH")
_MASK64 = (1 << 64) - 1
_IO_BUFFER = 256 << 10
_VERSION = 1
_COMPRESSED = "zstd"


@dataclass(frozen=True, slots=True)
class _Fragment:
    path: Path
    rows: int
    size: int
    compressed_size: int
    digest: str | None = None


class _Writer:
    def __init__(self, path: Path, stack: ExitStack, *, checksum: bool):
        self.path = path
        output = stack.enter_context(path.open("xb"))
        if checksum:
            # ExitStack は圧縮ストリームを閉じてから出力ファイルを同期する。
            stack.callback(_sync_file, output)
        compressor = _zstandard().ZstdCompressor(level=1, write_checksum=True)
        encoded = compressor.stream_writer(output, closefd=False)
        self.stream = stack.enter_context(BufferedWriter(encoded, buffer_size=_IO_BUFFER))
        self.hash = hashlib.sha256() if checksum else None
        self.rows = 0
        self.size = 0

    def write(self, record: bytes) -> None:
        self.stream.write(record)
        if self.hash is not None:
            self.hash.update(record)
        self.rows += 1
        self.size += len(record)

    def fragment(self) -> _Fragment:
        return _Fragment(
            self.path, self.rows, self.size, self.path.stat().st_size,
            self.hash.hexdigest() if self.hash is not None else None,
        )


def _zstandard():
    try:
        import zstandard
    except ImportError as error:
        raise RuntimeError("teacher shuffling requires the rshogi-dl compression extra") from error
    return zstandard


def _mix(value: int) -> int:
    # SplitMix64 の全単射を使い、同じ内容の行にも別々のキーを割り当てる。
    value = ((value ^ (value >> 30)) * 0xBF58476D1CE4E5B9) & _MASK64
    value = ((value ^ (value >> 27)) * 0x94D049BB133111EB) & _MASK64
    return value ^ (value >> 31)


def _source_dir(work: Path, index: int) -> Path:
    return work / f"source-{index:08d}"


def _partition_name(index: int) -> str:
    return f"partition-{index:03d}.zst"


def _config(seed: int, bits: int) -> dict:
    return {"version": _VERSION, "seed": seed, "partition_bits": bits, "compression": _COMPRESSED}


def _sync_file(stream) -> None:
    stream.flush()
    os.fsync(stream.fileno())


def _sync_directory(path: Path) -> None:
    if os.name == "nt":
        return
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _reject_symlink(path: Path) -> None:
    if path.is_symlink():
        raise ValueError(f"shuffle store path must not be a symbolic link: {path}")


def _json(path: Path) -> dict:
    _reject_symlink(path)
    if not path.is_file():
        raise ValueError(f"shuffle receipt must be a regular file: {path}")
    with path.open("r", encoding="utf-8") as stream:
        return json.load(stream)


def _write_json(path: Path, value: dict) -> None:
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True)
        _sync_file(stream)


@contextmanager
def _scratch(work: Path) -> Iterator[Path]:
    # 呼び出し側の排他ロックの内側で、強制終了時の未公開データを回収する。
    path = work / ".scratch"
    _reject_symlink(path)
    if path.exists():
        rmtree(path)
    path.mkdir()
    try:
        yield path
    finally:
        rmtree(path)


def _write_source(
    target: Path, rows: Iterable[bytes], start: int, seed: int, partition_bits: int, scratch: Path,
) -> None:
    with TemporaryDirectory(prefix="source-", dir=scratch) as temporary:
        directory = Path(temporary)
        writers = {}
        salt = _mix((seed + 0x9E3779B97F4A7C15) & _MASK64)
        with ExitStack() as files:
            count = 0
            for ordinal, row in enumerate(rows, start):
                if ordinal > _MASK64:
                    raise ValueError("shuffle supports at most 2**64 rows")
                if not isinstance(row, bytes) or not 0 < len(row) <= 65535:
                    raise ValueError("shuffle rows must contain 1 to 65535 bytes")
                key = _mix(ordinal ^ salt)
                index = key >> (64 - partition_bits)
                writer = writers.get(index)
                if writer is None:
                    writer = _Writer(directory / _partition_name(index), files, checksum=True)
                    writers[index] = writer
                writer.write(_HEADER.pack(key, len(row)) + row)
                count += 1

        partitions = {}
        for index, writer in writers.items():
            fragment = writer.fragment()
            partitions[str(index)] = [
                fragment.rows, fragment.size, fragment.compressed_size, fragment.digest,
            ]
        _write_json(directory / "receipt.json", {
            **_config(seed, partition_bits),
            "start": start,
            "rows": count,
            "partitions": partitions,
        })
        _sync_directory(directory)
        _sync_directory(scratch)
        directory.rename(target)
        _sync_directory(target.parent)
        _sync_directory(scratch)


def _source_receipt(directory: Path, config: dict, start: int) -> tuple[int, dict]:
    _reject_symlink(directory)
    receipt = _json(directory / "receipt.json")
    if any(receipt.get(name) != value for name, value in config.items()):
        raise ValueError(f"shuffle source configuration mismatch in {directory}")
    if receipt["start"] != start:
        raise ValueError(f"shuffle source ordinal mismatch in {directory}")
    rows = receipt["rows"]
    partitions = receipt["partitions"]
    if type(rows) is not int or rows < 0 or start + rows > 1 << 64:
        raise ValueError(f"invalid shuffle row count in {directory}")
    if any(
        str(int(index)) != index or not 0 <= int(index) < 1 << config["partition_bits"]
        or type(count) is not int or count <= 0 or type(size) is not int
        or not count * (_HEADER.size + 1) <= size <= count * (_HEADER.size + 65535)
        or type(compressed_size) is not int or compressed_size <= 0
        or not isinstance(digest, str) or len(digest) != 64
        for index, (count, size, compressed_size, digest) in partitions.items()
    ) or sum(partition[0] for partition in partitions.values()) != rows:
        raise ValueError(f"invalid shuffle partition receipt in {directory}")
    return rows, partitions


def _prepared_receipt(manifest: Path) -> dict:
    prepared = _json(manifest)
    bits = prepared["partition_bits"]
    seed = prepared["seed"]
    if (
        type(bits) is not int or not 0 <= bits <= 8
        or type(seed) is not int or not 0 <= seed <= _MASK64
        or type(prepared["sources"]) is not int or prepared["sources"] < 0
        or type(prepared["rows"]) is not int or not 0 <= prepared["rows"] <= 1 << 64
        or any(prepared.get(name) != value for name, value in _config(seed, bits).items())
    ):
        raise ValueError("invalid shuffle store receipt")
    return prepared


def partition_rows(
    work: Path, sources: Iterable[Iterable[bytes]], *, seed: int, partition_bits: int = 8,
) -> int:
    """入力を順番に分割し、再実行時は完了済み入力を再利用する。

    呼び出し側は作業ディレクトリの排他ロックを保持し、入力の同一性を確認する。
    """
    if type(seed) is not int or not 0 <= seed <= _MASK64:
        raise ValueError("shuffle seed must be an unsigned 64-bit integer")
    if type(partition_bits) is not int or not 0 <= partition_bits <= 8:
        raise ValueError("shuffle partition_bits must be between 0 and 8")
    config = _config(seed, partition_bits)
    _reject_symlink(work)
    work.mkdir(parents=True, exist_ok=True)
    _sync_directory(work.parent)
    manifest = work / "partitioned.json"
    _reject_symlink(manifest)
    prepared = _prepared_receipt(manifest) if manifest.exists() else None
    if prepared is not None and any(prepared.get(name) != value for name, value in config.items()):
        raise ValueError("shuffle store configuration mismatch")

    with _scratch(work) as scratch:
        total = source_count = 0
        for index, rows in enumerate(sources):
            source_count = index + 1
            directory = _source_dir(work, index)
            _reject_symlink(directory)
            if prepared is not None and (index >= prepared["sources"] or not directory.is_dir()):
                raise ValueError("shuffle source count or completed source directory mismatch")
            if not directory.is_dir():
                _write_source(directory, rows, total, seed, partition_bits, scratch)
            count, _ = _source_receipt(directory, config, total)
            total += count
        next_directory = _source_dir(work, source_count)
        _reject_symlink(next_directory)
        if next_directory.exists():
            raise ValueError("shuffle store contains a source absent from the input")
        completed = {**config, "sources": source_count, "rows": total}
        if prepared is None:
            receipt = scratch / "partitioned.json"
            _write_json(receipt, completed)
            _sync_directory(scratch)
            receipt.rename(manifest)
            _sync_directory(work)
            _sync_directory(scratch)
        elif prepared != completed:
            raise ValueError("shuffle store receipt totals do not match the sources")
    return total


def _read_records(fragment: _Fragment, prefix: int, bits: int) -> Iterator[bytes]:
    _reject_symlink(fragment.path)
    if not fragment.path.is_file():
        raise ValueError(f"shuffle fragment must be a regular file: {fragment.path}")
    if fragment.path.stat().st_size != fragment.compressed_size:
        raise ValueError(f"compressed shuffle fragment size mismatch in {fragment.path}")
    with fragment.path.open("rb") as encoded:
        decoder = _zstandard().ZstdDecompressor()
        with BufferedReader(decoder.stream_reader(encoded, closefd=False), buffer_size=_IO_BUFFER) as stream:
            yield from _decode_records(stream, fragment, prefix, bits)


def _decode_records(stream: BufferedReader, fragment: _Fragment, prefix: int, bits: int) -> Iterator[bytes]:
    hasher = hashlib.sha256() if fragment.digest is not None else None
    size = 0
    for _ in range(fragment.rows):
        header = stream.read(_HEADER.size)
        if len(header) != _HEADER.size:
            raise ValueError(f"truncated shuffle header in {fragment.path}")
        key, length = _HEADER.unpack(header)
        if not length or key >> (64 - bits) != prefix:
            raise ValueError(f"invalid shuffle record header in {fragment.path}")
        row = stream.read(length)
        if len(row) != length:
            raise ValueError(f"truncated shuffle row in {fragment.path}")
        record = header + row
        size += len(record)
        if hasher is not None:
            hasher.update(record)
        yield record
    if size != fragment.size or stream.read(1):
        raise ValueError(f"shuffle fragment size mismatch in {fragment.path}")
    if hasher is not None and hasher.hexdigest() != fragment.digest:
        raise ValueError(f"shuffle fragment checksum mismatch in {fragment.path}")


def _records(fragments: list[_Fragment], prefix: int, bits: int) -> Iterator[bytes]:
    for fragment in fragments:
        yield from _read_records(fragment, prefix, bits)


def _sort_partition(
    fragments: list[_Fragment], prefix: int, bits: int, budget: int, work: Path, start_row: int,
) -> Iterator[bytes]:
    # bytes のヘッダーは big endian のため、そのまま比較してキー順に並べられる。
    size = sum(fragment.size + 56 * fragment.rows for fragment in fragments)
    count = sum(fragment.rows for fragment in fragments)
    if start_row >= count:
        return
    if size <= budget or count <= 1:
        previous = None
        for record in islice(sorted(_records(fragments, prefix, bits)), start_row, None):
            key = record[:8]
            if key == previous:
                raise ValueError("duplicate shuffle key")
            previous = key
            yield record[_HEADER.size:]
        return
    if bits == 64:
        raise ValueError("duplicate shuffle key")

    step = min(4, 64 - bits)
    next_bits = bits + step
    with TemporaryDirectory(prefix=".radix-", dir=work) as temporary:
        directory = Path(temporary)
        writers = {}
        with ExitStack() as files:
            for record in _records(fragments, prefix, bits):
                key, _ = _HEADER.unpack_from(record)
                index = (key >> (64 - next_bits)) & ((1 << step) - 1)
                writer = writers.get(index)
                if writer is None:
                    writer = _Writer(directory / _partition_name(index), files, checksum=False)
                    writers[index] = writer
                writer.write(record)
        for index, writer in sorted(writers.items()):
            yield from _sort_partition(
                [writer.fragment()], (prefix << step) | index, next_bits, budget, directory, start_row,
            )
            start_row = max(0, start_row - writer.rows)
            writer.path.unlink()


def iter_shuffled_rows(
    work: Path, *, start_row: int = 0, max_partition_bytes: int = 128 << 20,
    committed_rows: Callable[[], int] | None = None,
) -> Iterator[bytes]:
    """完了済みの入力から、指定位置以降を分割境界によらないキー順で返す。

    呼び出し側はイテレータを閉じるまで作業ディレクトリの排他ロックを保持する。
    committed_rows は検証可能な状態で公開済みの出力行数を返す。区画の末尾まで公開されたら一時データを解放する。
    """
    if type(max_partition_bytes) is not int or max_partition_bytes <= 0:
        raise ValueError("max_partition_bytes must be a positive integer")
    _reject_symlink(work)
    prepared = _prepared_receipt(work / "partitioned.json")
    if type(start_row) is not int or not 0 <= start_row <= prepared["rows"]:
        raise ValueError("start_row lies outside the shuffle store")
    bits = prepared["partition_bits"]
    config = _config(prepared["seed"], bits)
    by_partition: list[list[_Fragment]] = [[] for _ in range(1 << bits)]
    total = 0
    for index in range(prepared["sources"]):
        directory = _source_dir(work, index)
        rows, metadata = _source_receipt(directory, config, total)
        for partition, (count, size, compressed_size, digest) in metadata.items():
            bucket = int(partition)
            by_partition[bucket].append(
                _Fragment(directory / _partition_name(bucket), count, size, compressed_size, digest)
            )
        total += rows
    if total != prepared["rows"]:
        raise ValueError("shuffle store receipt total does not match the sources")
    pending = deque()
    through = 0
    with _scratch(work) as scratch:
        for index, fragments in enumerate(by_partition):
            yield from _sort_partition(fragments, index, bits, max_partition_bytes, scratch, start_row)
            rows = sum(fragment.rows for fragment in fragments)
            through += rows
            start_row = max(0, start_row - rows)
            if committed_rows is not None:
                pending.append((through, fragments))
                committed = committed_rows()
                while pending and pending[0][0] <= committed:
                    _, completed = pending.popleft()
                    for fragment in completed:
                        fragment.path.unlink(missing_ok=True)
