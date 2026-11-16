"""小さな fixture で有限回だけ更新し、学習経路の接続を確かめる。

``python -m rshogi_dl.smoke --data FIXTURE.jsonl --run-dir $SHOGI_DATA/runs/rshogi-dl/<run>``

16 step × batch 8 固定の接続確認であり、学習器ではない。保存する checkpoint は検証専用。
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from collections.abc import Iterator
from pathlib import Path

import torch

from .checkpoint import CheckpointError, load_checkpoint, save_checkpoint
from .contract import TRAINING_STATUS
from .data import Batch, RecordError, iter_batches
from .loss import policy_value_loss
from .model import NAMED_CONFIGS, ModelConfig, SquareTransformer
from .paths import ArtifactPathError, resolve_new_artifact

SMOKE_STEPS = 16
SMOKE_BATCH_SIZE = 8
SMOKE_LEARNING_RATE = 1e-3

CHECKPOINT_NAME = "checkpoint.pt"
REPORT_NAME = "report.json"


class SmokeError(RuntimeError):
    """接続確認の条件を満たさなかった。"""


def _cycle_batches(data_path: str | Path) -> Iterator[Batch]:
    while True:
        produced = False
        for batch in iter_batches(data_path, SMOKE_BATCH_SIZE, drop_last=True):
            produced = True
            yield batch
        if not produced:
            raise SmokeError(f"fixture needs at least {SMOKE_BATCH_SIZE} records")


def _evaluate(model: SquareTransformer, batch: Batch) -> float:
    with torch.no_grad():
        policy, value = model(batch.tokens)
        return policy_value_loss(policy, value, batch).total.item()


def _group_gradient_norm(name: str, parameters: list[torch.nn.Parameter]) -> float:
    squared = 0.0
    for parameter in parameters:
        if parameter.grad is None:
            continue
        if not torch.isfinite(parameter.grad).all():
            raise SmokeError(f"non-finite gradient in group {name}")
        squared += parameter.grad.double().pow(2).sum().item()
    return math.sqrt(squared)


def run_smoke(data_path: str | Path, run_dir: str | Path, config: ModelConfig, seed: int) -> dict:
    """16 step の更新・checkpoint の保存と再読込を行い、report を返す。"""
    run_dir = resolve_new_artifact(run_dir)
    torch.manual_seed(seed)
    torch.set_num_threads(1)
    torch.use_deterministic_algorithms(True)

    model = SquareTransformer(config)
    model.train()
    optimizer = torch.optim.AdamW(model.parameters(), lr=SMOKE_LEARNING_RATE, weight_decay=0.0)
    groups = model.parameter_groups()
    initial = {
        name: [parameter.detach().clone() for parameter in parameters]
        for name, parameters in groups.items()
    }
    max_gradient_norm = dict.fromkeys(groups, 0.0)

    batches = _cycle_batches(data_path)
    first_batch = next(batches)
    loss_before = _evaluate(model, first_batch)
    losses: list[float] = []
    batch = first_batch
    for step in range(SMOKE_STEPS):
        if step > 0:
            batch = next(batches)
        optimizer.zero_grad(set_to_none=True)
        policy, value = model(batch.tokens)
        loss = policy_value_loss(policy, value, batch)
        if not torch.isfinite(loss.total):
            raise SmokeError(f"non-finite loss at step {step + 1}")
        loss.total.backward()
        for name, parameters in groups.items():
            norm = _group_gradient_norm(name, parameters)
            max_gradient_norm[name] = max(max_gradient_norm[name], norm)
        optimizer.step()
        losses.append(loss.total.item())
    loss_after = _evaluate(model, first_batch)

    for name, parameters in groups.items():
        if not all(torch.isfinite(parameter).all() for parameter in parameters):
            raise SmokeError(f"non-finite parameter in group {name}")
        if max_gradient_norm[name] == 0.0:
            raise SmokeError(f"group {name} never received a non-zero gradient")
        if all(torch.equal(now, before) for now, before in zip(parameters, initial[name], strict=True)):
            raise SmokeError(f"group {name} was not updated")
    if not math.isfinite(loss_after) or not loss_after < loss_before:
        raise SmokeError(
            f"loss on the first batch did not decrease: {loss_before} -> {loss_after}"
        )

    run_dir.parent.mkdir(parents=True, exist_ok=True)
    run_dir.mkdir()
    checkpoint_path = save_checkpoint(
        run_dir / CHECKPOINT_NAME, model, optimizer, step=SMOKE_STEPS, seed=seed
    )

    reloaded = load_checkpoint(checkpoint_path)
    model.eval()
    with torch.no_grad():
        expected = model(first_batch.tokens)
        actual = reloaded.model(first_batch.tokens)
    if not all(torch.equal(want, got) for want, got in zip(expected, actual, strict=True)):
        raise SmokeError("reloaded checkpoint does not reproduce the model outputs")
    torch.optim.AdamW(reloaded.model.parameters()).load_state_dict(reloaded.optimizer_state)

    report = {
        "training_status": TRAINING_STATUS,
        "model_config": config.to_dict(),
        "seed": seed,
        "steps": SMOKE_STEPS,
        "batch_size": SMOKE_BATCH_SIZE,
        "losses": losses,
        "first_batch_loss_before": loss_before,
        "first_batch_loss_after": loss_after,
        "max_gradient_norm": max_gradient_norm,
        "checkpoint": CHECKPOINT_NAME,
    }
    with open(resolve_new_artifact(run_dir / REPORT_NAME), "x", encoding="utf-8") as stream:
        json.dump(report, stream, indent=2, sort_keys=True, allow_nan=False)
        stream.write("\n")
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m rshogi_dl.smoke",
        description=f"{SMOKE_STEPS} step × batch {SMOKE_BATCH_SIZE} の接続確認 (CPU, 1 thread)。",
    )
    parser.add_argument("--data", required=True, help="encode 済み sample の JSONL")
    parser.add_argument(
        "--run-dir",
        required=True,
        help="新規に作る directory の絶対 path。$SHOGI_DATA/runs/rshogi-dl/ の下",
    )
    parser.add_argument("--config", choices=sorted(NAMED_CONFIGS), default="initial")
    parser.add_argument("--seed", type=int, default=0)
    args = parser.parse_args(argv)
    try:
        report = run_smoke(args.data, args.run_dir, NAMED_CONFIGS[args.config], args.seed)
    except (ArtifactPathError, CheckpointError, RecordError, SmokeError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    print(
        f"first-batch loss {report['first_batch_loss_before']:.6f} -> "
        f"{report['first_batch_loss_after']:.6f}; wrote {args.run_dir}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
