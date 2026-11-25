"""policy / value の loss。"""

from __future__ import annotations

from dataclasses import dataclass

import torch
from torch.nn import functional

from .data import Batch


@dataclass(frozen=True)
class LossParts:
    """``total = policy + value``。"""

    total: torch.Tensor
    policy: torch.Tensor
    value: torch.Tensor
    policy_entropy: torch.Tensor | None = None


def policy_value_loss(
    policy_logits: torch.Tensor, value_logits: torch.Tensor, batch: Batch, *, compute_policy_entropy: bool = False,
) -> LossParts:
    """合法手内の soft-target cross entropy と、value logit への BCE を足す。

    policy は ``policy_present`` の行だけで平均する。該当行がなければ policy loss は 0。
    """
    value_loss = functional.binary_cross_entropy_with_logits(value_logits.squeeze(1), batch.value)
    rows = batch.policy_rows
    if rows is None:
        rows = batch.policy_present.nonzero(as_tuple=True)[0]
    entropy = None
    if rows.numel() == 0:
        policy_loss = policy_logits.new_zeros(())
        if compute_policy_entropy:
            entropy = policy_loss
    else:
        legal_logits = policy_logits.index_select(0, rows).gather(
            1, batch.legal_index.index_select(0, rows)
        )
        mask = batch.legal_mask.index_select(0, rows)
        target = batch.policy_target.index_select(0, rows)
        # 2187 全体へ -inf の mask を掛けて log_softmax すると、重み 0 との積が NaN になる。
        # 正規化項だけを mask 付きで求め、引かれる側は gather した有限の logit のままにする。
        log_normalizer = torch.logsumexp(legal_logits.masked_fill(~mask, float("-inf")), dim=1)
        log_probabilities = legal_logits - log_normalizer.unsqueeze(1)
        policy_loss = -(target * log_probabilities).sum(dim=1).mean()
        if compute_policy_entropy:
            legal_log_prob = functional.log_softmax(
                legal_logits.float().masked_fill(~mask, float("-inf")), dim=1,
            ).masked_fill(~mask, 0.0)
            entropy = -(legal_log_prob.exp() * legal_log_prob).sum(dim=1).mean()
    return LossParts(total=policy_loss + value_loss, policy=policy_loss, value=value_loss, policy_entropy=entropy)
