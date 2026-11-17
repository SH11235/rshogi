"""有限 trainer の再開用状態を、上書きしない checkpoint に保存する。"""

from __future__ import annotations

import os
import math
import pickle
import random
from pathlib import Path

import numpy as np
import torch

from .checkpoint import CHECKPOINT_FORMAT
from .contract import ARCHITECTURE, INPUT_FORMAT, POLICY_FORMAT, TRAINING_STATUS, VALUE_FORMAT
from .paths import resolve_new_artifact

FORMAT = "rshogi-dl-trainer-v1"
SCALER_GROWTH_FACTOR = 2.0
SCALER_BACKOFF_FACTOR = 0.5
SCALER_GROWTH_INTERVAL = 2000
CONTRACT = {
    "format": FORMAT,
    "architecture": ARCHITECTURE,
    "input_format": INPUT_FORMAT,
    "policy_format": POLICY_FORMAT,
    "value_format": VALUE_FORMAT,
    "training_status": TRAINING_STATUS,
}
_KEYS = {
    *CONTRACT, "model_config", "trainer_config", "datasets", "model_state", "optimizer_state",
    "scaler_state", "scheduler_state", "progress", "rng", "runtime",
}


class ResumeError(ValueError):
    """checkpoint が再開する run の条件に一致しない。"""


def validate_scaler_state(state: object, *, enabled: bool) -> None:
    """GradScaler の再開状態を GPU 初期化・更新前に検証する。"""
    if type(state) is not dict:
        raise ResumeError("unexpected scaler state")
    if not enabled:
        if state:
            raise ResumeError("disabled AMP scaler must have an empty state")
        return
    if set(state) != {"scale", "growth_factor", "backoff_factor", "growth_interval", "_growth_tracker"}:
        raise ResumeError("unexpected FP16 scaler state fields")
    for key in ("scale", "growth_factor", "backoff_factor"):
        if type(state[key]) is not float or not math.isfinite(state[key]):
            raise ResumeError("scaler floating values must be finite floats")
    scale = state["scale"]
    if (
        not 0 < scale <= float(np.finfo(np.float32).max)
        or scale < float(np.nextafter(np.float32(0), np.float32(1)))
        or float(np.float32(scale)) != scale
    ):
        raise ResumeError("scaler scale must be a finite positive float32 value")
    if state["growth_factor"] <= 1 or not 0 < state["backoff_factor"] < 1:
        raise ResumeError("invalid scaler growth/backoff factors")
    interval = state["growth_interval"]
    tracker = state["_growth_tracker"]
    if (
        type(interval) is not int or interval <= 0
        or type(tracker) is not int or not 0 <= tracker < interval
    ):
        raise ResumeError("invalid scaler growth counters")
    if (
        state["growth_factor"] != SCALER_GROWTH_FACTOR
        or state["backoff_factor"] != SCALER_BACKOFF_FACTOR
        or interval != SCALER_GROWTH_INTERVAL
    ):
        raise ResumeError("scaler hyperparameters differ from trainer configuration")


def sync_directory(path: Path) -> None:
    """POSIX の directory entry を永続化する。Windows は file fsync と rename の範囲に限る。"""
    if os.name != "nt":
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


def atomic_save(path: str | Path, payload: dict) -> Path:
    """新規 checkpoint を fsync してから atomic rename する。既存 file は上書きしない。"""
    output = resolve_new_artifact(path)
    temporary = output.with_name(f".{output.name}.tmp")
    with temporary.open("xb") as stream:
        torch.save(payload, stream)
        stream.flush()
        os.fsync(stream.fileno())
    if output.exists():
        raise ResumeError("checkpoint output already exists")
    temporary.rename(output)
    sync_directory(output.parent)
    return output


def capture_rng(*, cuda: bool) -> dict:
    """weights_only 読込で扱える basic types と tensor に RNG を詰める。"""
    state = np.random.get_state()
    return {
        "python": random.getstate(),
        "numpy": {
            "algorithm": state[0],
            "keys": torch.from_numpy(state[1].astype(np.int64)),
            "position": state[2],
            "has_gauss": state[3],
            "cached_gaussian": state[4],
        },
        "torch": torch.random.get_rng_state(),
        "cuda": torch.cuda.get_rng_state_all() if cuda else [],
    }


def restore_rng(state: dict, *, cuda: bool) -> None:
    """Python・NumPy・Torch と、CUDA run の GPU RNG を復元する。"""
    try:
        if type(state) is not dict or set(state) != {"python", "numpy", "torch", "cuda"}:
            raise ResumeError("unexpected RNG state")
        numpy = state["numpy"]
        if type(numpy) is not dict or set(numpy) != {
            "algorithm", "keys", "position", "has_gauss", "cached_gaussian"
        }:
            raise ResumeError("unexpected NumPy RNG state")
        keys = numpy["keys"]
        if (
            numpy["algorithm"] != "MT19937"
            or not isinstance(keys, torch.Tensor)
            or keys.dtype != torch.int64
            or tuple(keys.shape) != (624,)
            or keys.device.type != "cpu"
            or bool(((keys < 0) | (keys >= 2**32)).any())
            or type(numpy["position"]) is not int
            or not 0 <= numpy["position"] <= 624
            or type(numpy["has_gauss"]) is not int
            or numpy["has_gauss"] not in (0, 1)
            or type(numpy["cached_gaussian"]) is not float
            or not np.isfinite(numpy["cached_gaussian"])
        ):
            raise ResumeError("invalid NumPy RNG state")
        if type(state["cuda"]) is not list or (not cuda and state["cuda"]):
            raise ResumeError("unexpected CUDA RNG state")
        if cuda and len(state["cuda"]) != torch.cuda.device_count():
            raise ResumeError("CUDA device count differs from checkpoint")
        random.setstate(state["python"])
        np.random.set_state((
            numpy["algorithm"], keys.numpy().astype(np.uint32), numpy["position"],
            numpy["has_gauss"], numpy["cached_gaussian"],
        ))
        torch.random.set_rng_state(state["torch"])
        if cuda:
            torch.cuda.set_rng_state_all(state["cuda"])
    except (TypeError, ValueError, RuntimeError, KeyError) as error:
        raise ResumeError(f"invalid RNG state: {error}") from error


def load_resume(path: str | Path) -> dict:
    """再開専用 format を CPU に安全に読む。旧 checkpoint reader の契約は変えない。"""
    try:
        payload = torch.load(path, map_location="cpu", weights_only=True)
    except (OSError, RuntimeError, ValueError, EOFError, pickle.UnpicklingError, TypeError) as error:
        raise ResumeError(f"cannot read resume checkpoint: {error}") from error
    if type(payload) is not dict or set(payload) != _KEYS:
        raise ResumeError("unexpected resume checkpoint layout")
    if any(type(payload[key]) is not str or payload[key] != value for key, value in CONTRACT.items()):
        raise ResumeError("unexpected resume checkpoint contract")
    if payload["scheduler_state"] is not None:
        raise ResumeError("constant learning rate requires a null scheduler state")
    return payload


def export_checkpoint(path, model, optimizer, *, step: int, seed: int) -> Path:
    """既存 export/parity CLI が読める v1 の検証用 checkpoint を別に保存する。"""
    return atomic_save(path, {
        "format": CHECKPOINT_FORMAT,
        "architecture": ARCHITECTURE,
        "input_format": INPUT_FORMAT,
        "training_status": TRAINING_STATUS,
        "model_config": model.config.to_dict(),
        "model_state": model.state_dict(),
        "optimizer_state": optimizer.state_dict(),
        "step": step,
        "seed": seed,
    })
