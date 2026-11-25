"""Read ordered HCPE3 archives as training batches without staging positions on disk."""

from contextlib import ExitStack, contextmanager
from hashlib import file_digest, sha256
from itertools import islice
import json
import os
from pathlib import Path
import random
import re
import stat
import subprocess
import tempfile

from .contract import INPUT_FORMAT, POLICY_FORMAT, TOKEN_COUNT, VALUE_FORMAT
from .hcpe3_rows import GameFrame, assemble_shards, iter_packed_frames
from .paths import shogi_data_root
from .sharded import MAX_BATCH_SIZE, DatasetError, _append, _batch, _scratch, _signature


FORMAT = "rshogi-dl-hcpe3-stream-v1"
_SHUFFLE_ROWS = 8192
_EXIT_TIMEOUT = 10


def is_hcpe3_manifest(path):
    return Path(path).is_file()


def _regular_file(path):
    if not isinstance(path, (str, Path)):
        raise DatasetError("expected an HCPE3 input path")
    resolved = Path(path).resolve(strict=True)
    if not stat.S_ISREG(resolved.stat().st_mode):
        raise DatasetError(f"expected a regular file: {resolved}")
    return resolved


def _manifest(path):
    path = _regular_file(path)
    if not path.is_relative_to(shogi_data_root()):
        raise DatasetError("HCPE3 input manifest must be below SHOGI_DATA")
    signature = _signature(path)
    if signature[2] > 1 << 20:
        raise DatasetError("HCPE3 input manifest is too large")
    raw = path.read_bytes()
    config = json.loads(raw)
    if (
        type(config) is not dict
        or set(config) != {"format", "encoder", "encoder_sha256", "archives", "protected_tokens", "heldout_modulus", "seed"}
        or config["format"] != FORMAT
        or type(config["seed"]) is not int
        or type(config["heldout_modulus"]) is not int
        or config["heldout_modulus"] < 0 or config["heldout_modulus"] == 1
    ):
        raise DatasetError("invalid HCPE3 input manifest")
    encoder = _regular_file(config["encoder"])
    if not os.access(encoder, os.X_OK) or _hash(encoder) != config["encoder_sha256"]:
        raise DatasetError("HCPE3 encoder does not match the manifest")
    if type(config["archives"]) is not list or not config["archives"]:
        raise DatasetError("HCPE3 input manifest requires archives")
    archives = []
    for entry in config["archives"]:
        if type(entry) is not dict or set(entry) != {"path", "sha256", "size"}:
            raise DatasetError("invalid HCPE3 archive entry")
        source = _regular_file(entry["path"])
        source_signature = _signature(source)
        if (
            not source.is_relative_to(shogi_data_root())
            or type(entry["size"]) is not int or entry["size"] != source_signature[2]
            or type(entry["sha256"]) is not str or not re.fullmatch(r"[a-f0-9]{64}", entry["sha256"])
            or not source.name.endswith((".hcpe3", ".hcpe3.xz"))
        ):
            raise DatasetError(f"invalid HCPE3 archive identity: {source}")
        archives.append((source, entry["sha256"], source_signature))
    if len({entry[1] for entry in archives}) != len(archives):
        raise DatasetError("duplicate HCPE3 archives")
    protected = set()
    if config["protected_tokens"] is not None:
        entry = config["protected_tokens"]
        if type(entry) is not dict or set(entry) != {"path", "sha256"}:
            raise DatasetError("invalid protected-token entry")
        exclude = _regular_file(entry["path"])
        if not exclude.is_relative_to(shogi_data_root()) or exclude.stat().st_size > 128 << 20:
            raise DatasetError("invalid protected-token file")
        data = exclude.read_bytes()
        if len(data) % TOKEN_COUNT or sha256(data).hexdigest() != entry["sha256"]:
            raise DatasetError("protected-token file does not match the manifest")
        protected.update(data[i:i + TOKEN_COUNT] for i in range(0, len(data), TOKEN_COUNT))
    if _signature(path) != signature:
        raise DatasetError("HCPE3 input manifest changed while reading")
    content = sha256(raw).hexdigest()
    fingerprint = sha256(json.dumps([content, [list(entry[2]) for entry in archives]]).encode()).hexdigest()
    return path, encoder, config, archives, protected, content, fingerprint


def _hash(path):
    with path.open("rb") as stream:
        return file_digest(stream, "sha256").hexdigest()


def hcpe3_dataset_info(path):
    root, _, _, _, _, content, fingerprint = _manifest(path)
    return {"path": str(root), "fingerprint": fingerprint, "manifest": {
        "format": FORMAT, "input_format": INPUT_FORMAT, "policy_format": POLICY_FORMAT,
        "value_format": VALUE_FORMAT, "source_records": None, "dataset_content_sha256": content,
        "teacher_kind": "train-visits", "split": "train", "training_status": "unknown",
    }}


@contextmanager
def _replay(encoder, path):
    with ExitStack() as stack:
        processes = []
        try:
            if path.suffix == ".xz":
                decoder = stack.enter_context(subprocess.Popen(
                    ["xz", "--decompress", "--stdout", "--threads=1", "--", path], stdout=subprocess.PIPE,
                ))
                processes.append(decoder)
                input_pipe, source = decoder.stdout, "-"
            else:
                input_pipe, source = None, path
            child = stack.enter_context(subprocess.Popen(
                [encoder, "--skip-invalid-games", source], stdin=input_pipe, stdout=subprocess.PIPE,
            ))
            processes.insert(0, child)
            if input_pipe is not None:
                input_pipe.close()
            yield child.stdout
            for process in processes:
                if process.wait(timeout=_EXIT_TIMEOUT):
                    raise DatasetError(f"HCPE3 replay or decompression failed: {path}")
        except GeneratorExit:
            if any(process.poll() not in (None, 0) for process in processes):
                raise DatasetError(f"HCPE3 replay or decompression failed: {path}") from None
            raise
        except subprocess.TimeoutExpired as error:
            raise DatasetError(f"timed out closing HCPE3 replay: {path}") from error
        finally:
            for process in processes:
                if process.poll() is None:
                    process.terminate()
            for process in processes:
                try:
                    process.wait(timeout=_EXIT_TIMEOUT)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def _archive_rows(encoder, archive, config, protected):
    source, digest, signature = archive
    if _signature(source) != signature or _hash(source) != digest or _signature(source) != signature:
        raise DatasetError(f"HCPE3 archive contents changed: {source}")
    modulus = config["heldout_modulus"]
    try:
        with _replay(encoder, source) as stream:
            retained = True
            for frame in iter_packed_frames(stream):
                if isinstance(frame, GameFrame):
                    retained = not modulus or int.from_bytes(frame.key[:8], "big") % modulus != 0
                elif retained and frame.tokens not in protected:
                    yield frame
    finally:
        if _signature(source) != signature:
            raise DatasetError(f"HCPE3 archive changed during replay: {source}")


def _count_record(path, count):
    return f"{count} {sha256(f'{path.name}:{count}'.encode()).hexdigest()}\n"


def _remember_count(path, count):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(_count_record(path, count))
            stream.close()
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def _ordered_rows(path, start_row, expected_fingerprint):
    root, encoder, config, archives, protected, _, fingerprint = _manifest(path)
    if expected_fingerprint is not None and expected_fingerprint != fingerprint:
        raise DatasetError("HCPE3 input changed before opening the training stream")
    index = root.with_name(f"{root.name}.counts") / fingerprint
    index.mkdir(parents=True, exist_ok=True)
    for archive in archives:
        source, digest, signature = archive
        count_path = index / digest
        if start_row and count_path.is_file():
            record = count_path.read_text()
            count = int(record.split(" ", 1)[0])
            if count < 0 or record != _count_record(count_path, count) or _signature(source) != signature:
                raise DatasetError(f"invalid HCPE3 resume index: {source}")
            if start_row >= count:
                start_row -= count
                continue
        rng = random.Random(config["seed"] ^ int(digest, 16))
        count = 0
        rows = _archive_rows(encoder, archive, config, protected)
        try:
            while block := list(islice(rows, _SHUFFLE_ROWS)):
                count += len(block)
                rng.shuffle(block)
                skip = min(start_row, len(block))
                start_row -= skip
                yield from islice(block, skip, None)
        finally:
            rows.close()
        _remember_count(count_path, count)
    if start_row:
        raise DatasetError("HCPE3 resume cursor exceeds the dataset")


def iter_hcpe3_batches(path, batch_size, *, start_row=0, expected_fingerprint=None):
    if type(batch_size) is not int or not 1 <= batch_size <= MAX_BATCH_SIZE:
        raise DatasetError("invalid HCPE3 batch size")
    if type(start_row) is not int or start_row < 0:
        raise DatasetError("invalid HCPE3 resume cursor")
    rows = _ordered_rows(path, start_row, expected_fingerprint)
    try:
        for columns, count in assemble_shards(rows, batch_size):
            scratch = _scratch(count)
            width = _append(scratch, 0, columns, 0, count)
            yield _batch(scratch, count, width)
    finally:
        rows.close()
