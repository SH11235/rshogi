from __future__ import annotations

import math
import unittest

import torch

from rshogi_dl.contract import POLICY_SIZE
from rshogi_dl.data import collate, validate_record
from rshogi_dl.loss import policy_value_loss

from .support import empty_board_tokens


def record(legal: list[int], policy: list[list] | None, value: float) -> dict:
    return {"tokens": empty_board_tokens(), "legal_labels": legal, "policy": policy, "value": value}


def batch_of(*records: dict):
    return collate([validate_record(raw) for raw in records])


def logits_for(rows: int) -> tuple[torch.Tensor, torch.Tensor]:
    generator = torch.Generator().manual_seed(11)
    policy = torch.randn(rows, POLICY_SIZE, generator=generator, requires_grad=True)
    value = torch.randn(rows, 1, generator=generator, requires_grad=True)
    return policy, value


def bce(logit: float, target: float) -> float:
    probability = 1.0 / (1.0 + math.exp(-logit))
    return -(target * math.log(probability) + (1.0 - target) * math.log(1.0 - probability))


class PolicyValueLossTest(unittest.TestCase):
    def test_matches_manual_computation(self) -> None:
        batch = batch_of(
            record([10, 500, 2000], [[10, 3], [2000, 1]], 0.25),
            record([7, 8], [[8, 2.0]], 1.0),
        )
        policy, value = logits_for(2)
        loss = policy_value_loss(policy, value, batch)

        first = [policy[0, label].item() for label in (10, 500, 2000)]
        first_norm = math.log(sum(math.exp(x) for x in first))
        first_loss = -(0.75 * (first[0] - first_norm) + 0.25 * (first[2] - first_norm))
        second = [policy[1, label].item() for label in (7, 8)]
        second_loss = -(second[1] - math.log(sum(math.exp(x) for x in second)))
        expected_policy = (first_loss + second_loss) / 2
        expected_value = (bce(value[0, 0].item(), 0.25) + bce(value[1, 0].item(), 1.0)) / 2

        self.assertAlmostEqual(loss.policy.item(), expected_policy, places=5)
        self.assertAlmostEqual(loss.value.item(), expected_value, places=5)
        self.assertAlmostEqual(loss.total.item(), expected_policy + expected_value, places=5)

    def test_illegal_logits_do_not_affect_loss_or_receive_gradient(self) -> None:
        batch = batch_of(record([10, 500], [[10, 1]], 0.5))
        policy, value = logits_for(1)
        baseline = policy_value_loss(policy, value, batch)
        baseline.total.backward()
        illegal = torch.ones(POLICY_SIZE, dtype=torch.bool)
        illegal[[10, 500]] = False
        self.assertEqual(policy.grad[0, illegal].abs().sum().item(), 0.0)
        self.assertGreater(policy.grad[0, ~illegal].abs().sum().item(), 0.0)

        shifted = policy.detach().clone()
        shifted[0, illegal] = 1e4
        moved = policy_value_loss(shifted, value.detach(), batch)
        self.assertEqual(moved.policy.item(), baseline.policy.item())

    def test_zero_weight_legal_move_with_extreme_logit_stays_finite(self) -> None:
        batch = batch_of(record([10, 500], [[10, 1], [500, 0]], 0.5))
        policy, value = logits_for(1)
        with torch.no_grad():
            policy[0, 500] = -1e30
        loss = policy_value_loss(policy, value, batch)
        loss.total.backward()
        self.assertTrue(torch.isfinite(loss.total))
        self.assertTrue(torch.isfinite(policy.grad).all())

    def test_all_policy_null_gives_zero_policy_loss_and_finite_value_gradient(self) -> None:
        batch = batch_of(record([10, 500], None, 0.0), record([], None, 1.0))
        policy, value = logits_for(2)
        loss = policy_value_loss(policy, value, batch)
        self.assertEqual(loss.policy.item(), 0.0)
        self.assertTrue(torch.isfinite(loss.total))
        self.assertEqual(loss.total.item(), loss.value.item())
        loss.total.backward()
        self.assertIsNone(policy.grad)
        self.assertTrue(torch.isfinite(value.grad).all())
        self.assertGreater(value.grad.abs().sum().item(), 0.0)

    def test_null_rows_are_excluded_from_policy_mean(self) -> None:
        with_policy = record([10, 500, 2000], [[10, 3], [2000, 1]], 0.25)
        # 合法手のない行と、合法手はあるが policy のない行。どちらも policy には寄与しない。
        mixed = batch_of(with_policy, record([], None, 0.5), record([3, 4], None, 0.75))
        alone = batch_of(with_policy)
        policy, value = logits_for(3)
        mixed_loss = policy_value_loss(policy, value, mixed)
        alone_loss = policy_value_loss(policy[:1], value[:1], alone)
        self.assertAlmostEqual(mixed_loss.policy.item(), alone_loss.policy.item(), places=6)
        mixed_loss.total.backward()
        self.assertTrue(torch.isfinite(policy.grad).all())
        self.assertTrue(torch.isfinite(value.grad).all())
        self.assertEqual(policy.grad[1:].abs().sum().item(), 0.0)
        self.assertGreater(value.grad[1:].abs().min().item(), 0.0)


if __name__ == "__main__":
    unittest.main()
