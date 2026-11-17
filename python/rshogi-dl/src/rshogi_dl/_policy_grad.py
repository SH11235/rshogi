"""policy 教師なしの更新で、eager autograd と同じ head の勾配状態を保つ。"""

from __future__ import annotations

from .data import Batch
from .model import SquareTransformer


def has_policy_targets(batch: Batch) -> bool:
    """転送前の batch から policy 教師の有無を得る。prepared index は形状だけで判定する。"""
    if batch.policy_rows is not None:
        return batch.policy_rows.numel() != 0
    if batch.policy_present.device.type != "cpu":
        raise ValueError("legacy policy presence must be determined from CPU batch metadata")
    return bool(batch.policy_present.any().item())


def clear_unused_policy_head_gradients(model: SquareTransformer, has_policy: bool) -> None:
    """AOT backward が作る未使用出力の zero grad を、教師なしの policy head だけ None に戻す。"""
    if not has_policy:
        for parameter in model.policy_head.parameters():
            parameter.grad = None
