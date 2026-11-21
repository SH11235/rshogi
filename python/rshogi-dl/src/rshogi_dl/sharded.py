"""検証済み JSONL を ragged 配列の shard に変換し、逐次読む。"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import stat
import subprocess
import sys
from array import array
from collections.abc import Callable, Iterable, Iterator
from io import BytesIO
from itertools import chain, repeat
from pathlib import Path

import numpy as np
import torch

from .contract import INPUT_FORMAT, MAX_LEGAL_LABELS, POLICY_FORMAT, TOKEN_COUNT, VALUE_FORMAT
from .data import Batch, Record, RecordError, iter_records, validate_record
from .hcpe3_rows import Hcpe3RowsError, PolicyFrame, iter_frames
from .paths import ArtifactPathError, shogi_data_root

FORMAT = "rshogi-dl-sharded-v1"
ZSTD_FORMAT = "rshogi-dl-sharded-zstd-v1"
MAX_SHARD_RECORDS = 65536
MAX_BATCH_SIZE = 16384
_MAX_METADATA_BYTES = 65536
_MAX_ARRAY_HEADER_BYTES = 1024
_ENCODER_EXIT_GRACE_SECONDS = 5.0
_ARRAY_NAMES = (
    "tokens", "values", "policy_present", "legal_offsets", "legal_indices", "policy_targets"
)
_CONTRACT = {
    "format": FORMAT,
    "input_format": INPUT_FORMAT,
    "policy_format": POLICY_FORMAT,
    "value_format": VALUE_FORMAT,
}


class DatasetError(ValueError):
    """shard の契約・完了状態・immutable な配置に反する。"""


def _dataset_path(path: str | Path, *, new: bool) -> Path:
    candidate = Path(path)
    if not candidate.is_absolute():
        raise ArtifactPathError("dataset path must be absolute")
    resolved = candidate.resolve()
    root = shogi_data_root()
    allowed = (root / "teachers" / "dl", root / "runs" / "rshogi-dl")
    if not any(resolved != parent and resolved.is_relative_to(parent) for parent in allowed):
        raise ArtifactPathError(
            "dataset must be below $SHOGI_DATA/teachers/dl or $SHOGI_DATA/runs/rshogi-dl"
        )
    for start in (resolved, Path(__file__).resolve(), Path.cwd().resolve()):
        for parent in (start, *start.parents):
            if (parent / ".git").exists():
                if resolved.is_relative_to(parent):
                    raise ArtifactPathError("dataset must not be inside a source repository")
                break
    if new:
        if os.path.lexists(candidate) or os.path.lexists(resolved):
            raise ArtifactPathError(f"refusing to overwrite dataset: {resolved}")
    elif not resolved.is_dir():
        raise DatasetError("dataset directory does not exist")
    return resolved


def _positive_int(value: object, name: str, maximum: int | None = None) -> int:
    if type(value) is not int or value <= 0 or (maximum is not None and value > maximum):
        limit = f" <= {maximum}" if maximum else ""
        raise DatasetError(f"{name} must be a positive integer{limit}")
    return value


def _text(value: object, name: str) -> str:
    if type(value) is not str or not value.strip() or len(value) > 256:
        raise DatasetError(f"{name} must be a non-empty string of at most 256 characters")
    return value


def _hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _write_json(path: Path, payload: dict) -> None:
    with path.open("x", encoding="utf-8") as stream:
        json.dump(payload, stream, ensure_ascii=False, allow_nan=False, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def _sync_directory(path: Path) -> None:
    if os.name == "nt":
        # Windows の標準 os.open は directory の flush 用 handle を提供しない。
        return
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _read_json(path: Path) -> dict:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > _MAX_METADATA_BYTES:
        raise DatasetError(f"missing or invalid metadata file: {path.name}")
    try:
        with path.open(encoding="utf-8") as stream:
            value = json.load(stream, object_pairs_hook=_unique_object)
    except (ValueError, OSError) as error:
        raise DatasetError(f"invalid metadata: {path.name}") from error
    if type(value) is not dict:
        raise DatasetError("metadata must be an object")
    return value


def _unique_object(pairs: list[tuple[str, object]]) -> dict:
    value = {}
    for key, item in pairs:
        if key in value:
            raise DatasetError("duplicate metadata key")
        value[key] = item
    return value


def _signature(path: Path, *, follow_symlinks: bool = True) -> tuple[int, int, int, int, int, int]:
    info = path.stat(follow_symlinks=follow_symlinks)
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns, info.st_mode


def _shard_dir(root: Path, index: int) -> Path:
    return root / f"shard-{index:08d}"


def _write_shard(
    root: Path, index: int, columns: dict[str, np.ndarray | array], count: int, compressor,
) -> str:
    directory = _shard_dir(root, index)
    directory.mkdir()
    entries = len(columns["legal_indices"])
    arrays = {
        "tokens": columns["tokens"][:count],
        "values": columns["values"][:count],
        "policy_present": columns["policy_present"][:count],
        "legal_offsets": columns["legal_offsets"][:count + 1],
        "legal_indices": np.asarray(columns["legal_indices"], dtype="<u2"),
        "policy_targets": np.asarray(columns["policy_targets"], dtype="<f4"),
    }
    files = {}
    blocks = {}
    for name, values in arrays.items():
        header_buffer = BytesIO()
        np.lib.format.write_array_header_1_0(header_buffer, np.lib.format.header_data_from_array_1_0(values))
        header = header_buffer.getvalue()
        payload = memoryview(values).cast("B")
        digest = hashlib.sha256(header)
        digest.update(payload)
        files[name] = {"size": len(header) + payload.nbytes, "sha256": digest.hexdigest()}
        blocks[name] = header, payload
    if compressor is None:
        for name, (header, payload) in blocks.items():
            path = directory / f"{name}.npy"
            with path.open("xb") as stream:
                stream.write(header)
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
    else:
        _write_compressed_arrays(
            directory / "arrays.zst", compressor,
            sum(file["size"] for file in files.values()), chain.from_iterable(blocks.values()),
        )
    _write_json(
        directory / "metadata.json",
        {"format": FORMAT, "records": count, "legal_entries": entries, "files": files},
    )
    _sync_directory(directory)
    return _hash(directory / "metadata.json")


def convert_jsonl(
    source: str | Path,
    destination: str | Path,
    *,
    shard_records: int = 4096,
    teacher_kind: str = "unknown",
    split: str = "unknown",
    training_status: str = "unknown",
    compression_level: int | None = None,
) -> dict:
    """全行を一度検証し、最大 ``shard_records`` 行ずつ保存して最後に manifest を公開する。

    失敗した directory は未完了のまま残す。再開・上書きはせず、新しい出力先を指定する。
    """
    output = _conversion_output(destination, shard_records, teacher_kind, split, training_status)
    source = Path(source)
    if not stat.S_ISREG(source.stat().st_mode):
        raise DatasetError("source must be an immutable regular file")
    source_signature = _signature(source)

    def check_source() -> None:
        try:
            source_unchanged = _signature(source) == source_signature
        except OSError as error:
            raise DatasetError("source changed during conversion") from error
        if not source_unchanged:
            raise DatasetError("source changed during conversion")

    return _convert_records(
        iter_records(source), output, source.name, source_signature[2], shard_records,
        teacher_kind, split, training_status, check_source, compression_level,
    )


def convert_records(
    records: Iterable[object],
    destination: str | Path,
    *,
    source_name: str,
    source_size: int,
    shard_records: int = 4096,
    teacher_kind: str = "unknown",
    split: str = "unknown",
    training_status: str = "unknown",
    compression_level: int | None = None,
) -> dict:
    """JSONL と同じ形式の行を逐次検証し、shard へ変換する。

    source_name と source_size は入力の由来を記録する。入力の安定性は呼び出し元が保証する。
    """
    output = _conversion_output(destination, shard_records, teacher_kind, split, training_status)
    return _convert_records(
        (validate_record(record) for record in records), output, _text(source_name, "source_name"),
        _positive_int(source_size, "source_size"), shard_records, teacher_kind, split, training_status,
        compression_level=compression_level,
    )


def convert_hcpe3(
    sources: Iterable[str | Path],
    destination: str | Path,
    *,
    encoder: str | Path,
    shard_records: int = 4096,
    teacher_kind: str = "unknown",
    split: str = "unknown",
    training_status: str = "unknown",
    compression_level: int | None = None,
) -> dict:
    """HCPE3 を順番に再生し、訪問数付き学習行から教師 shard を作る。"""
    output = _conversion_output(destination, shard_records, teacher_kind, split, training_status)
    paths = tuple(Path(path).resolve(strict=True) for path in sources)
    signatures = {path: _signature(path) for path in paths}
    if not paths or any(not stat.S_ISREG(sig[-1]) for sig in signatures.values()):
        raise DatasetError("HCPE3 sources must be immutable regular files")
    size = _positive_int(sum(signatures[path][2] for path in paths), "source_size")
    name = ", ".join(path.name for path in paths)
    if len(name) > 256:
        name = f"HCPE3 ({len(paths)} files)"

    def check_sources() -> None:
        try:
            unchanged = all(_signature(path) == signature for path, signature in signatures.items())
        except OSError as error:
            raise DatasetError("HCPE3 source changed during conversion") from error
        if not unchanged:
            raise DatasetError("HCPE3 source changed during conversion")

    with subprocess.Popen([encoder, *paths], stdout=subprocess.PIPE) as child:
        def records():
            assert child.stdout is not None
            try:
                for frame in iter_frames(child.stdout):
                    if isinstance(frame, PolicyFrame):
                        yield validate_record(frame.to_record())
            except Hcpe3RowsError as error:
                raise DatasetError(str(error)) from error
            try:
                status = child.wait(timeout=_ENCODER_EXIT_GRACE_SECONDS)
            except subprocess.TimeoutExpired as error:
                raise DatasetError("HCPE3 encoder kept running after its output closed") from error
            if status != 0:
                raise DatasetError(f"HCPE3 encoder exited with status {status}")

        try:
            return _convert_records(
                records(), output, name, size, shard_records, teacher_kind, split, training_status,
                check_sources, compression_level,
            )
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=_ENCODER_EXIT_GRACE_SECONDS)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()


def _conversion_output(
    destination: str | Path,
    shard_records: int,
    teacher_kind: str,
    split: str,
    training_status: str,
) -> Path:
    _positive_int(shard_records, "shard_records", MAX_SHARD_RECORDS)
    _text(teacher_kind, "teacher_kind")
    _text(split, "split")
    if training_status not in ("unknown", "validation-only"):
        raise DatasetError("dataset training_status must be unknown or validation-only")
    return _dataset_path(destination, new=True)


def _convert_records(
    records: Iterable[Record],
    output: Path,
    source_name: str,
    source_size: int,
    shard_records: int,
    teacher_kind: str,
    split: str,
    training_status: str,
    check_source: Callable[[], None] | None = None,
    compression_level: int | None = None,
) -> dict:
    compressor = None if compression_level is None else _compressor(compression_level)
    _initialize_output(output)
    columns = {
        "tokens": np.empty((shard_records, TOKEN_COUNT), dtype=np.uint8),
        "values": np.empty(shard_records, dtype="<f4"),
        "policy_present": np.empty(shard_records, dtype=np.bool_),
        "legal_offsets": np.zeros(shard_records + 1, dtype="<u8"),
        "legal_indices": array("H"),
        "policy_targets": array("f"),
    }
    content_digest = hashlib.sha256()
    total = 0
    shards = 0
    count = 0
    for record in records:
        columns["tokens"][count] = record.tokens
        columns["values"][count] = record.value
        columns["policy_present"][count] = record.policy is not None
        columns["legal_indices"].extend(record.legal_labels)
        targets = columns["policy_targets"]
        if record.policy is None:
            targets.extend(repeat(0.0, len(record.legal_labels)))
        else:
            weight_sum = math.fsum(weight for _, weight in record.policy)
            weights = {label: weight / weight_sum for label, weight in record.policy}
            targets.extend(map(weights.get, record.legal_labels, repeat(0.0)))
        count += 1
        total += 1
        columns["legal_offsets"][count] = len(columns["legal_indices"])
        if count == shard_records:
            content_digest.update(bytes.fromhex(_write_shard(output, shards, columns, count, compressor)))
            shards += 1
            count = 0
            columns["legal_indices"] = array("H")
            columns["policy_targets"] = array("f")
            columns["legal_offsets"][0] = 0
    if count:
        content_digest.update(bytes.fromhex(_write_shard(output, shards, columns, count, compressor)))
        shards += 1
    if not total:
        raise DatasetError("cannot convert an empty dataset")
    manifest = {
        **_CONTRACT,
        "format": FORMAT if compressor is None else ZSTD_FORMAT,
        "source_name": source_name,
        "source_size": source_size,
        "dataset_content_sha256": content_digest.hexdigest(),
        "source_records": total,
        "shard_records": shard_records,
        "shard_count": shards,
        "teacher_kind": teacher_kind,
        "split": split,
        "training_status": training_status,
    }
    _publish_manifest(output, manifest, check_source)
    return manifest


def _initialize_output(output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    output.mkdir()
    parent = output.parent
    data_root = shogi_data_root()
    while parent.is_relative_to(data_root):
        _sync_directory(parent)
        if parent == data_root:
            break
        parent = parent.parent


def _publish_manifest(output: Path, manifest: dict, check_source: Callable[[], None] | None = None) -> None:
    temporary = output / "manifest.tmp"
    _write_json(temporary, manifest)
    _sync_directory(output)
    if check_source is not None:
        check_source()
    temporary.rename(output / "manifest.json")
    _sync_directory(output)


def _manifest(root: Path) -> dict:
    manifest = _read_json(root / "manifest.json")
    expected = {
        *_CONTRACT, "source_name", "source_size", "dataset_content_sha256", "source_records", "shard_records", "shard_count",
        "teacher_kind", "split", "training_status",
    }
    if set(manifest) != expected or manifest["format"] not in (FORMAT, ZSTD_FORMAT) or any(
        manifest.get(key) != value for key, value in _CONTRACT.items() if key != "format"
    ):
        raise DatasetError("unexpected dataset manifest contract")
    digest = manifest["dataset_content_sha256"]
    if type(digest) is not str or len(digest) != 64 or any(
        char not in "0123456789abcdef" for char in digest
    ):
        raise DatasetError("invalid dataset content digest")
    _positive_int(manifest["source_records"], "source_records")
    _positive_int(manifest["source_size"], "source_size")
    _positive_int(manifest["shard_records"], "shard_records", MAX_SHARD_RECORDS)
    _positive_int(manifest["shard_count"], "shard_count")
    expected_shards = (
        manifest["source_records"] + manifest["shard_records"] - 1
    ) // manifest["shard_records"]
    if manifest["shard_count"] != expected_shards:
        raise DatasetError("shard count does not match source record count")
    for field in ("source_name", "teacher_kind", "split"):
        _text(manifest[field], field)
    if manifest["training_status"] not in ("unknown", "validation-only"):
        raise DatasetError("unexpected dataset training_status")
    return manifest


def _read_array_offset(stream, size: int, dtype: np.dtype, shape: tuple[int, ...]) -> int:
    if np.lib.format.read_magic(stream) != (1, 0):
        raise DatasetError("only converter-generated NPY 1.0 arrays are supported")
    size_bytes = stream.read(2)
    if len(size_bytes) != 2 or int.from_bytes(size_bytes, "little") > _MAX_ARRAY_HEADER_BYTES:
        raise DatasetError("array header exceeds supported size")
    stream.seek(-2, os.SEEK_CUR)
    actual_shape, fortran_order, actual_dtype = np.lib.format.read_array_header_1_0(
        stream, max_header_size=_MAX_ARRAY_HEADER_BYTES
    )
    offset = stream.tell()
    if actual_dtype != dtype or actual_shape != shape or fortran_order:
        raise DatasetError("unexpected array layout")
    if size != offset + math.prod(shape) * dtype.itemsize:
        raise DatasetError("unexpected array length")
    return offset


def _read_array(path: Path, dtype: np.dtype, shape: tuple[int, ...]) -> np.ndarray:
    try:
        with path.open("rb") as stream:
            offset = _read_array_offset(stream, os.fstat(stream.fileno()).st_size, dtype, shape)
        if not all(shape):
            return np.empty(shape, dtype=dtype)
        return np.memmap(path, dtype=dtype, mode="r", offset=offset, shape=shape, order="C")
    except (ValueError, OSError, EOFError) as error:
        raise DatasetError(f"invalid array {path.name}: {error}") from error


class _Shard:
    def __init__(self, root: Path, index: int, manifest: dict, *, verify_hashes: bool) -> None:
        directory = _shard_dir(root, index)
        if directory.is_symlink() or not directory.is_dir():
            raise DatasetError("missing or symlinked shard directory")
        metadata = _read_json(directory / "metadata.json")
        rows = min(
            manifest["shard_records"],
            manifest["source_records"] - index * manifest["shard_records"],
        )
        if (
            set(metadata) != {"format", "records", "legal_entries", "files"}
            or metadata["format"] != FORMAT
            or type(metadata["records"]) is not int
            or metadata["records"] != rows
        ):
            raise DatasetError("unexpected shard metadata")
        entries = metadata["legal_entries"]
        if type(entries) is not int or not 0 <= entries <= rows * MAX_LEGAL_LABELS:
            raise DatasetError("invalid legal entry count")
        if (
            type(metadata["files"]) is not dict
            or set(metadata["files"]) != set(_ARRAY_NAMES)
        ):
            raise DatasetError("unexpected shard files")
        layouts = {
            "tokens": (np.dtype("u1"), (rows, TOKEN_COUNT)),
            "values": (np.dtype("<f4"), (rows,)),
            "policy_present": (np.dtype("?"), (rows,)),
            "legal_offsets": (np.dtype("<u8"), (rows + 1,)),
            "legal_indices": (np.dtype("<u2"), (entries,)),
            "policy_targets": (np.dtype("<f4"), (entries,)),
        }
        self.signatures = {
            root / "manifest.json": _signature(root / "manifest.json"),
            directory / "metadata.json": _signature(directory / "metadata.json"),
        }
        files = metadata["files"]
        for name, (dtype, shape) in layouts.items():
            info = files[name]
            array_bytes = math.prod(shape) * dtype.itemsize
            if (
                type(info) is not dict
                or set(info) != {"size", "sha256"}
                or type(info["size"]) is not int
                or not array_bytes + 10 <= info["size"] <= array_bytes + 10 + _MAX_ARRAY_HEADER_BYTES
                or type(info["sha256"]) is not str
                or len(info["sha256"]) != 64
                or any(char not in "0123456789abcdef" for char in info["sha256"])
            ):
                raise DatasetError("unexpected array metadata")
        self.files = files
        if manifest["format"] == ZSTD_FORMAT:
            payload = directory / "arrays.zst"
            try:
                signature = _signature(payload, follow_symlinks=False)
            except OSError as error:
                raise DatasetError("missing compressed shard") from error
            if not stat.S_ISREG(signature[-1]):
                raise DatasetError("missing or symlinked compressed shard")
            self.signatures[payload] = signature
            self.arrays = _read_compressed_arrays(payload, signature[2], layouts, files, verify_hashes)
        else:
            self.arrays = {}
            for name, (dtype, shape) in layouts.items():
                path = directory / f"{name}.npy"
                info = files[name]
                if path.is_symlink() or not path.is_file() or path.stat().st_size != info["size"]:
                    raise DatasetError(f"missing or resized array: {name}")
                self.signatures[path] = _signature(path)
                self.arrays[name] = _read_array(path, dtype, shape)
                if verify_hashes and _hash(path) != info["sha256"]:
                    raise DatasetError(f"array checksum mismatch: {name}")
        for values in self.arrays.values():
            values.flags.writeable = False
        offsets = self.arrays["legal_offsets"]
        if offsets[0] != 0 or offsets[-1] != entries:
            raise DatasetError("invalid legal offset endpoints")
        self.rows = rows
        self.check_unchanged()

    def check_unchanged(self) -> None:
        for path, signature in self.signatures.items():
            try:
                unchanged = _signature(path, follow_symlinks=False) == signature
            except OSError as error:
                raise DatasetError("dataset changed while reading") from error
            if not unchanged:
                raise DatasetError("dataset changed while reading")


def _zstandard():
    try:
        import zstandard
    except ImportError as error:
        raise DatasetError("compressed shards require the rshogi-dl[compression] extra") from error
    return zstandard


def _compressor(level: int):
    if type(level) is not int or not 1 <= level <= 19:
        raise DatasetError("compression level must be an integer in 1..19")
    return _zstandard().ZstdCompressor(level=level, write_checksum=True)


def _write_compressed_arrays(path: Path, compressor, raw_size: int, blocks: Iterable[bytes | memoryview]) -> None:
    encoder = compressor.compressobj(size=raw_size)
    with path.open("xb") as stream:
        for block in blocks:
            stream.write(encoder.compress(block))
        stream.write(encoder.flush())
        stream.flush()
        os.fsync(stream.fileno())


def _read_compressed_arrays(path: Path, size: int, layouts: dict, files: dict, verify_hashes: bool) -> dict:
    zstandard = _zstandard()
    raw_size = sum(file["size"] for file in files.values())
    if not 0 < size <= raw_size * 2:
        raise DatasetError("unexpected compressed shard length")
    try:
        with path.open("rb") as stream:
            encoded = stream.read(size + 1)
        if len(encoded) != size:
            raise DatasetError("compressed shard changed while reading")
        frame = zstandard.get_frame_parameters(encoded)
        if frame.content_size != raw_size or frame.window_size > raw_size or not frame.has_checksum:
            raise DatasetError("unexpected compressed shard frame")
        raw = memoryview(zstandard.ZstdDecompressor().decompress(
            encoded, max_output_size=raw_size, allow_extra_data=False,
        ))
        if len(raw) != raw_size:
            raise DatasetError("unexpected decompressed shard length")
    except zstandard.ZstdError as error:
        raise DatasetError(f"invalid compressed shard: {path.name}") from error
    arrays = {}
    cursor = 0
    for name, (dtype, shape) in layouts.items():
        info = files[name]
        column = raw[cursor:cursor + info["size"]]
        cursor += len(column)
        try:
            offset = _read_array_offset(BytesIO(column[:10 + _MAX_ARRAY_HEADER_BYTES]), len(column), dtype, shape)
            if verify_hashes and hashlib.sha256(column).hexdigest() != info["sha256"]:
                raise DatasetError(f"array checksum mismatch: {name}")
            arrays[name] = np.frombuffer(column[offset:], dtype=dtype).reshape(shape)
        except (ValueError, EOFError) as error:
            raise DatasetError(f"invalid array {name}: {error}") from error
    return arrays


def compress_dataset(source: str | Path, destination: str | Path, *, level: int = 3) -> dict:
    """生成済みの NPY shard を検証し、配列と論理 digest を保った圧縮 dataset を公開する。"""
    compressor = _compressor(level)
    root = _dataset_path(source, new=False)
    output = _dataset_path(destination, new=True)
    if output.is_relative_to(root):
        raise ArtifactPathError("compressed dataset must be outside the source dataset")
    signature = _signature(root / "manifest.json")
    manifest = _manifest(root)
    if manifest["format"] != FORMAT:
        raise DatasetError("compression source must be an NPY dataset")
    _initialize_output(output)
    content_digest = hashlib.sha256()

    def check_source() -> None:
        if _signature(root / "manifest.json") != signature:
            raise DatasetError("dataset manifest changed during compression")

    for index in range(manifest["shard_count"]):
        check_source()
        shard = _Shard(root, index, manifest, verify_hashes=False)
        directory = _shard_dir(root, index)
        metadata = (directory / "metadata.json").read_bytes()
        content_digest.update(hashlib.sha256(metadata).digest())
        target = _shard_dir(output, index)
        target.mkdir()
        raw_size = sum(file["size"] for file in shard.files.values())

        def checked_blocks():
            for name in _ARRAY_NAMES:
                info = shard.files[name]
                with (directory / f"{name}.npy").open("rb") as array_file:
                    payload = array_file.read(info["size"] + 1)
                if len(payload) != info["size"] or hashlib.sha256(payload).hexdigest() != info["sha256"]:
                    raise DatasetError(f"array checksum mismatch: {name}")
                yield payload

        _write_compressed_arrays(target / "arrays.zst", compressor, raw_size, checked_blocks())
        with (target / "metadata.json").open("xb") as stream:
            stream.write(metadata)
            stream.flush()
            os.fsync(stream.fileno())
        shard.check_unchanged()
        _sync_directory(target)
    if content_digest.hexdigest() != manifest["dataset_content_sha256"]:
        raise DatasetError("dataset content digest mismatch")
    result = {**manifest, "format": ZSTD_FORMAT}
    _publish_manifest(output, result, check_source)
    return result


def _scratch(size: int) -> dict[str, np.ndarray]:
    return {
        "tokens": np.empty((size, TOKEN_COUNT), dtype=np.uint8),
        "values": np.empty(size, dtype=np.float32),
        "policy_present": np.empty(size, dtype=np.bool_),
        "legal_indices": np.zeros((size, MAX_LEGAL_LABELS), dtype=np.uint16),
        "legal_mask": np.zeros((size, MAX_LEGAL_LABELS), dtype=np.bool_),
        "policy_targets": np.zeros((size, MAX_LEGAL_LABELS), dtype=np.float32),
    }


def _append(scratch: dict, row: int, shard: _Shard, start: int, count: int) -> int:
    source = shard.arrays
    stop = start + count
    for name in ("tokens", "values", "policy_present"):
        scratch[name][row:row + count] = source[name][start:stop]
    offsets = source["legal_offsets"][start:stop + 1].astype(np.int64)
    lengths = np.diff(offsets)
    if (
        lengths.min() < 0
        or lengths.max() > MAX_LEGAL_LABELS
        or offsets[0] < 0
        or offsets[-1] > source["legal_indices"].size
    ):
        raise DatasetError("legal offsets cannot form a bounded batch")
    width = int(lengths.max())
    if width:
        mask = np.arange(width)[None, :] < lengths[:, None]
        entries = slice(offsets[0], offsets[-1])
        scratch["legal_indices"][row:row + count, :width][mask] = source["legal_indices"][entries]
        scratch["policy_targets"][row:row + count, :width][mask] = source["policy_targets"][entries]
        scratch["legal_mask"][row:row + count, :width] = mask
    return width


def _batch(scratch: dict, count: int, width: int) -> Batch:
    width = max(1, width)
    return Batch(
        tokens=torch.from_numpy(scratch["tokens"][:count].astype(np.int64)),
        legal_index=torch.from_numpy(scratch["legal_indices"][:count, :width].astype(np.int64)),
        legal_mask=torch.from_numpy(scratch["legal_mask"][:count, :width].copy()),
        policy_target=torch.from_numpy(scratch["policy_targets"][:count, :width].copy()),
        policy_present=torch.from_numpy(scratch["policy_present"][:count].copy()),
        value=torch.from_numpy(scratch["values"][:count].copy()),
        policy_rows=torch.from_numpy(
            np.flatnonzero(scratch["policy_present"][:count]).astype(np.int64, copy=False)
        ),
    )


def iter_batches(
    path: str | Path,
    batch_size: int,
    *,
    drop_last: bool = False,
    verify_hashes: bool = False,
    start_record: int = 0,
) -> Iterator[Batch]:
    """生成済みで変更されない dataset を順番どおり読み、shard 境界を跨いで batch を作る。

    既定は全データの再検証・hash scan をしない。生成後の編集や置換をせず、別 directory に新規生成する。
    ``start_record`` は先頭からの行数で、対象 shard を直接開く。既読 shard の読捨てはしない。
    """
    _positive_int(batch_size, "batch_size", MAX_BATCH_SIZE)
    root = _dataset_path(path, new=False)
    manifest = _manifest(root)
    if type(start_record) is not int or not 0 <= start_record <= manifest["source_records"]:
        raise DatasetError("start_record must be an integer within the dataset")
    if start_record == manifest["source_records"]:
        return
    manifest_signature = _signature(root / "manifest.json")
    scratch = _scratch(batch_size)
    pending = 0
    width = 0
    first_shard, first_row = divmod(start_record, manifest["shard_records"])
    for index in range(first_shard, manifest["shard_count"]):
        if _signature(root / "manifest.json") != manifest_signature:
            raise DatasetError("dataset manifest changed while reading")
        shard = _Shard(root, index, manifest, verify_hashes=verify_hashes)
        start = first_row if index == first_shard else 0
        while start < shard.rows:
            shard.check_unchanged()
            count = min(batch_size - pending, shard.rows - start)
            width = max(width, _append(scratch, pending, shard, start, count))
            shard.check_unchanged()
            pending += count
            start += count
            if pending == batch_size:
                yield _batch(scratch, pending, width)
                pending = 0
                width = 0
                for name in ("legal_indices", "legal_mask", "policy_targets"):
                    scratch[name].fill(0)
        del shard
    if pending and not drop_last:
        yield _batch(scratch, pending, width)


def dataset_info(path: str | Path) -> dict:
    """生成時の内容 root を含む manifest の指紋を返す。shard は走査しない。"""
    root = _dataset_path(path, new=False)
    signature = _signature(root / "manifest.json")
    manifest = _manifest(root)
    digest = hashlib.sha256()
    digest.update(json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode())
    if _signature(root / "manifest.json") != signature:
        raise DatasetError("dataset manifest changed while fingerprinting")
    return {"path": str(root), "fingerprint": digest.hexdigest(), "manifest": manifest}


def verify_dataset(path: str | Path) -> dict:
    """各 shard の header と全ファイルの SHA-256 を明示的に検査する。sample の再検証はしない。"""
    root = _dataset_path(path, new=False)
    manifest = _manifest(root)
    signature = _signature(root / "manifest.json")
    content_digest = hashlib.sha256()
    for index in range(manifest["shard_count"]):
        shard = _Shard(root, index, manifest, verify_hashes=True)
        content_digest.update(bytes.fromhex(_hash(_shard_dir(root, index) / "metadata.json")))
        shard.check_unchanged()
        if _signature(root / "manifest.json") != signature:
            raise DatasetError("dataset manifest changed while verifying")
        del shard
    if content_digest.hexdigest() != manifest["dataset_content_sha256"]:
        raise DatasetError("dataset content digest mismatch")
    return manifest


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="教師 shard の生成・可逆圧縮・checksum 検査。"
    )
    commands = parser.add_subparsers(dest="command", required=True)
    convert = commands.add_parser("convert")
    convert.add_argument("--source", required=True)
    hcpe3 = commands.add_parser("convert-hcpe3")
    hcpe3.add_argument("--source", action="append", required=True)
    hcpe3.add_argument("--encoder", required=True)
    for conversion in (convert, hcpe3):
        conversion.add_argument("--output", required=True)
        conversion.add_argument("--shard-records", type=int, default=4096)
        conversion.add_argument("--teacher-kind", default="unknown")
        conversion.add_argument("--split", default="unknown")
        conversion.add_argument("--compression-level", type=int)
        conversion.add_argument(
            "--training-status", choices=("unknown", "validation-only"), default="unknown"
        )
    compress = commands.add_parser("compress")
    compress.add_argument("--source", required=True)
    compress.add_argument("--output", required=True)
    compress.add_argument("--level", type=int, default=3)
    verify = commands.add_parser("verify")
    verify.add_argument("--data", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command in ("convert", "convert-hcpe3"):
            options = dict(
                shard_records=args.shard_records,
                teacher_kind=args.teacher_kind,
                split=args.split,
                training_status=args.training_status,
                compression_level=args.compression_level,
            )
            if args.command == "convert":
                result = convert_jsonl(args.source, args.output, **options)
            else:
                result = convert_hcpe3(args.source, args.output, encoder=args.encoder, **options)
        elif args.command == "compress":
            result = compress_dataset(args.source, args.output, level=args.level)
        else:
            result = verify_dataset(args.data)
        print(json.dumps(result, ensure_ascii=False, allow_nan=False, sort_keys=True))
    except (DatasetError, ArtifactPathError, RecordError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
