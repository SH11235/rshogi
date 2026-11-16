"""checkpoint の保存と読み込み。"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import torch

from .contract import ARCHITECTURE, INPUT_FORMAT, TRAINING_STATUS
from .model import ModelConfig, SquareTransformer
from .paths import resolve_new_artifact

CHECKPOINT_FORMAT = "rshogi-dl-checkpoint-v1"

_KEYS = frozenset(
    {
        "format",
        "architecture",
        "input_format",
        "training_status",
        "model_config",
        "model_state",
        "optimizer_state",
        "step",
        "seed",
    }
)


class CheckpointError(ValueError):
    """checkpoint が期待した形式でない。"""


@dataclass(frozen=True)
class Checkpoint:
    """読み込んだ checkpoint。"""

    model: SquareTransformer
    optimizer_state: dict
    step: int
    seed: int
    training_status: str


def save_checkpoint(
    path: str | Path,
    model: SquareTransformer,
    optimizer: torch.optim.Optimizer,
    *,
    step: int,
    seed: int,
) -> Path:
    """model と optimizer の状態を新規 file に保存する。既存 file は上書きしない。"""
    resolved = resolve_new_artifact(path)
    payload = {
        "format": CHECKPOINT_FORMAT,
        "architecture": ARCHITECTURE,
        "input_format": INPUT_FORMAT,
        "training_status": TRAINING_STATUS,
        "model_config": model.config.to_dict(),
        "model_state": model.state_dict(),
        "optimizer_state": optimizer.state_dict(),
        "step": step,
        "seed": seed,
    }
    with open(resolved, "xb") as stream:
        torch.save(payload, stream)
    return resolved


def load_checkpoint(path: str | Path) -> Checkpoint:
    """checkpoint を CPU に読み込み、model を復元する。"""
    payload = torch.load(path, map_location="cpu", weights_only=True)
    if type(payload) is not dict or set(payload) != _KEYS:
        raise CheckpointError("unexpected checkpoint layout")
    expected = {
        "format": CHECKPOINT_FORMAT,
        "architecture": ARCHITECTURE,
        "input_format": INPUT_FORMAT,
        "training_status": TRAINING_STATUS,
    }
    for key, value in expected.items():
        if payload[key] != value:
            raise CheckpointError(f"checkpoint {key} must be {value!r}, got {payload[key]!r}")
    try:
        config = ModelConfig.from_dict(payload["model_config"])
    except (TypeError, ValueError) as error:
        raise CheckpointError(f"invalid model config: {error}") from error
    model = SquareTransformer(config)
    model.load_state_dict(payload["model_state"])
    model.eval()
    return Checkpoint(
        model=model,
        optimizer_state=payload["optimizer_state"],
        step=payload["step"],
        seed=payload["seed"],
        training_status=payload["training_status"],
    )
