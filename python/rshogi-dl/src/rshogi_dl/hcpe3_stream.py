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
from sys import float_info
import tempfile

from .contract import INPUT_FORMAT, POLICY_FORMAT, TOKEN_COUNT, VALUE_FORMAT
from .hcpe3_rows import GameFrame, assemble_shards, iter_packed_frames
from .paths import shogi_data_root
from .sharded import MAX_BATCH_SIZE, DatasetError, _append, _batch, _scratch, _signature


FORMAT = "rshogi-dl-hcpe3-stream-v1"
_SHUFFLE_ROWS = 8192
_MAX_INTERLEAVE_ARCHIVES = 8
_GAME_KEY_BYTES = 32
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
    required = {"format", "encoder", "encoder_sha256", "archives", "protected_tokens", "heldout_modulus", "seed"}
    optional = {"interleave_archives", "protected_games", "value_eval_weight", "value_eval_scale", "policy_temperature"}
    if (
        type(config) is not dict
        or not required <= config.keys() <= required | optional
        or config["format"] != FORMAT
        or type(config["seed"]) is not int
        or type(config["heldout_modulus"]) is not int
        or config["heldout_modulus"] < 0 or config["heldout_modulus"] == 1
    ):
        raise DatasetError("invalid HCPE3 input manifest")
    weight = config.get("value_eval_weight", 0.0)
    scale = config.get("value_eval_scale", 0.0013226)
    temperature = config.get("policy_temperature", 1.0)
    if type(weight) not in (int, float) or not 0 <= weight <= 1:
        raise DatasetError("value_eval_weight must be in 0..1")
    if type(scale) not in (int, float) or not 0 < scale <= float_info.max:
        raise DatasetError("value_eval_scale must be positive and finite")
    if type(temperature) not in (int, float) or not 0 <= temperature <= float_info.max:
        raise DatasetError("policy_temperature must be finite and nonnegative")
    interleave = config.get("interleave_archives", 1)
    if type(interleave) is not int or not 1 <= interleave <= _MAX_INTERLEAVE_ARCHIVES:
        raise DatasetError(f"interleave_archives must be in 1..{_MAX_INTERLEAVE_ARCHIVES}")
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
    protected = _protection(config["protected_tokens"], TOKEN_COUNT, "protected-token")
    protected_games = _protection(config.get("protected_games"), _GAME_KEY_BYTES, "protected-game")
    if _signature(path) != signature:
        raise DatasetError("HCPE3 input manifest changed while reading")
    content = sha256(raw).hexdigest()
    fingerprint = sha256(json.dumps([content, [list(entry[2]) for entry in archives]]).encode()).hexdigest()
    return path, encoder, config, archives, protected, protected_games, content, fingerprint


def _protection(entry, width, label):
    if entry is None:
        return set()
    if type(entry) is not dict or set(entry) != {"path", "sha256"}:
        raise DatasetError(f"invalid {label} entry")
    path = _regular_file(entry["path"])
    if not path.is_relative_to(shogi_data_root()) or path.stat().st_size > 128 << 20:
        raise DatasetError(f"invalid {label} file")
    data = path.read_bytes()
    if len(data) % width or sha256(data).hexdigest() != entry["sha256"]:
        raise DatasetError(f"{label} file does not match the manifest")
    return {data[i:i + width] for i in range(0, len(data), width)}


def _hash(path):
    with path.open("rb") as stream:
        return file_digest(stream, "sha256").hexdigest()


def hcpe3_dataset_info(path):
    root, _, _, _, _, _, content, fingerprint = _manifest(path)
    return {"path": str(root), "fingerprint": fingerprint, "manifest": {
        "format": FORMAT, "input_format": INPUT_FORMAT, "policy_format": POLICY_FORMAT,
        "value_format": VALUE_FORMAT, "source_records": None, "dataset_content_sha256": content,
        "teacher_kind": "train-visits", "split": "train", "training_status": "unknown",
    }}


@contextmanager
def _replay(encoder, path, arguments):
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
                [encoder, "--skip-invalid-games", *arguments, source], stdin=input_pipe, stdout=subprocess.PIPE,
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


def _archive_rows(encoder, archive, config, protected, protected_games):
    source, digest, signature = archive
    if _signature(source) != signature or _hash(source) != digest or _signature(source) != signature:
        raise DatasetError(f"HCPE3 archive contents changed: {source}")
    modulus = config["heldout_modulus"]
    weight = config.get("value_eval_weight", 0.0)
    arguments = ["--value-eval-weight", str(weight), "--value-eval-scale", str(config.get("value_eval_scale", 0.0013226))] if weight else []
    try:
        with _replay(encoder, source, arguments) as stream:
            retained = True
            for frame in iter_packed_frames(stream, mixed_values=bool(weight)):
                if isinstance(frame, GameFrame):
                    retained = frame.key not in protected_games and (
                        not modulus or int.from_bytes(frame.key[:8], "big") % modulus != 0
                    )
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


def _cached_count(index, archive):
    source, digest, signature = archive
    count_path = index / digest
    if not count_path.is_file():
        return None
    record = count_path.read_text()
    count = int(record.split(" ", 1)[0])
    if count < 0 or record != _count_record(count_path, count) or _signature(source) != signature:
        raise DatasetError(f"invalid HCPE3 resume index: {source}")
    return count


def _mixed_blocks(encoder, group, config, protected, protected_games, index):
    seed = config["seed"]
    if len(group) == 1:
        seed ^= int(group[0][1], 16)
    else:
        identity = b"".join(bytes.fromhex(archive[1]) for archive in group)
        seed ^= int.from_bytes(sha256(identity).digest(), "big")
    rng = random.Random(seed)
    with ExitStack() as stack:
        active = {}
        counts = {}
        for archive in group:
            rows = _archive_rows(encoder, archive, config, protected, protected_games)
            stack.callback(rows.close)
            active[archive[1]] = rows
            counts[archive[1]] = 0
        while active:
            block = []
            share = _SHUFFLE_ROWS // len(active)
            for digest, rows in list(active.items()):
                start = len(block)
                block.extend(islice(rows, share))
                added = len(block) - start
                counts[digest] += added
                if added < share:
                    _remember_count(index / digest, counts[digest])
                    del active[digest]
            if block:
                rng.shuffle(block)
                yield block


def _ordered_rows(root, encoder, config, archives, protected, protected_games, fingerprint, start_row):
    index = root.with_name(f"{root.name}.counts") / fingerprint
    index.mkdir(parents=True, exist_ok=True)
    interleave = config.get("interleave_archives", 1)
    for offset in range(0, len(archives), interleave):
        group = archives[offset:offset + interleave]
        if start_row:
            counts = [_cached_count(index, archive) for archive in group]
            if None not in counts and start_row >= sum(counts):
                start_row -= sum(counts)
                continue
        blocks = _mixed_blocks(encoder, group, config, protected, protected_games, index)
        try:
            for block in blocks:
                skip = min(start_row, len(block))
                start_row -= skip
                yield from islice(block, skip, None)
        finally:
            blocks.close()
    if start_row:
        raise DatasetError("HCPE3 resume cursor exceeds the dataset")


def iter_hcpe3_batches(path, batch_size, *, start_row=0, expected_fingerprint=None):
    if type(batch_size) is not int or not 1 <= batch_size <= MAX_BATCH_SIZE:
        raise DatasetError("invalid HCPE3 batch size")
    if type(start_row) is not int or start_row < 0:
        raise DatasetError("invalid HCPE3 resume cursor")
    root, encoder, config, archives, protected, protected_games, _, fingerprint = _manifest(path)
    if expected_fingerprint is not None and expected_fingerprint != fingerprint:
        raise DatasetError("HCPE3 input changed before opening the training stream")
    rows = _ordered_rows(root, encoder, config, archives, protected, protected_games, fingerprint, start_row)
    try:
        for columns, count in assemble_shards(rows, batch_size, policy_temperature=config.get("policy_temperature", 1.0)):
            scratch = _scratch(count)
            width = _append(scratch, 0, columns, 0, count)
            yield _batch(scratch, count, width)
    finally:
        rows.close()
