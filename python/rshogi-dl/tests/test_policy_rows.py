from __future__ import annotations

from dataclasses import fields
from pathlib import Path
import unittest
from unittest.mock import patch

import torch

from rshogi_dl.data import Batch, collate, iter_batches, validate_record
from rshogi_dl.loss import policy_value_loss
from rshogi_dl.model import TEST_CONFIG, SquareTransformer
from .support import artifact_tempdir, synthetic_records, write_jsonl


def records_with_policy(present: list[bool]) -> list[dict]:
    records = synthetic_records(seed=71, count=len(present))
    for record, has_policy in zip(records, present, strict=True):
        record["policy"] = [[record["legal_labels"][0], 1.0]] if has_policy else None
        if not has_policy:
            record["legal_labels"] = []
    return records


def prepared_batch(present: list[bool]) -> Batch:
    return collate([validate_record(record) for record in records_with_policy(present)])


def legacy_batch(batch: Batch) -> Batch:
    return Batch(*(getattr(batch, field.name) for field in fields(Batch) if field.name != "policy_rows"))


class PolicyRowsTest(unittest.TestCase):
    def test_rows_are_cpu_int64_local_indices_including_partial_batch(self) -> None:
        for present in ([True] * 5, [False] * 5, [False, True, False, False, True]):
            with self.subTest(present=present), artifact_tempdir() as root:
                path = Path(root) / "fixture.jsonl"
                write_jsonl(path, records_with_policy(present))
                batches = list(iter_batches(path, 3))
                self.assertEqual([batch.tokens.shape[0] for batch in batches], [3, 2])
                for index, batch in enumerate(batches):
                    expected = [i for i, enabled in enumerate(present[index * 3:(index + 1) * 3]) if enabled]
                    self.assertEqual(batch.policy_rows.dtype, torch.int64)
                    self.assertEqual(batch.policy_rows.device.type, "cpu")
                    self.assertEqual(batch.policy_rows.dim(), 1)
                    self.assertEqual(batch.policy_rows.tolist(), expected)

    def test_prepared_and_legacy_loss_and_full_logit_gradients_are_identical(self) -> None:
        for present in ([True] * 3, [False] * 3, [False, True, False]):
            with self.subTest(present=present):
                prepared = prepared_batch(present)
                legacy = legacy_batch(prepared)
                self.assertIsNone(legacy.policy_rows)
                generator = torch.Generator().manual_seed(83)
                policy = torch.randn(len(present), 2187, generator=generator)
                value = torch.randn(len(present), 1, generator=generator)
                outputs = []
                gradients = []
                for batch in (prepared, legacy):
                    logits = policy.clone().requires_grad_()
                    values = value.clone().requires_grad_()
                    loss = policy_value_loss(logits, values, batch)
                    loss.total.backward()
                    outputs.append(loss)
                    gradients.append((logits.grad, values.grad))
                for name in ("total", "policy", "value"):
                    self.assertTrue(torch.equal(getattr(outputs[0], name), getattr(outputs[1], name)), name)
                for expected, actual in zip(*gradients, strict=True):
                    if expected is None:
                        self.assertIsNone(actual)
                    else:
                        self.assertTrue(torch.equal(expected, actual))
                with patch.object(torch.Tensor, "nonzero", side_effect=AssertionError("prepared rows")):
                    policy_value_loss(policy, value, prepared)

    def test_adamw_keeps_weights_steps_and_moments_for_missing_policy(self) -> None:
        torch.manual_seed(89)
        prepared = SquareTransformer(TEST_CONFIG)
        legacy = SquareTransformer(TEST_CONFIG)
        legacy.load_state_dict(prepared.state_dict())
        models = (prepared, legacy)
        optimizers = [torch.optim.AdamW(model.parameters(), lr=1e-3, weight_decay=0.1) for model in models]
        for flags in ([False] * 3, [True] * 3, [False, True, False], [False] * 3, [True] * 3):
            batch = prepared_batch(flags)
            before = [parameter.detach().clone() for parameter in prepared.policy_head.parameters()]
            old_steps = [
                optimizer.state.get(model.policy_head.weight, {}).get("step", torch.tensor(0)).clone()
                for model, optimizer in zip(models, optimizers, strict=True)
            ]
            for model, optimizer, current in zip(models, optimizers, (batch, legacy_batch(batch)), strict=True):
                optimizer.zero_grad(set_to_none=True)
                loss = policy_value_loss(*model(current.tokens), current)
                loss.total.backward()
                if not any(flags):
                    self.assertTrue(all(parameter.grad is None for parameter in model.policy_head.parameters()))
                optimizer.step()
            if not any(flags):
                for old, parameter in zip(before, prepared.policy_head.parameters(), strict=True):
                    self.assertTrue(torch.equal(old, parameter))
                for model, optimizer, old in zip(models, optimizers, old_steps, strict=True):
                    state = optimizer.state.get(model.policy_head.weight, {})
                    self.assertTrue(torch.equal(state.get("step", torch.tensor(0)), old))
                    if old.item() == 0:
                        self.assertEqual(state, {})
            for (_, expected), (_, actual) in zip(prepared.named_parameters(), legacy.named_parameters(), strict=True):
                self.assertTrue(torch.equal(expected, actual))
                if expected.grad is None:
                    self.assertIsNone(actual.grad)
                else:
                    self.assertTrue(torch.equal(expected.grad, actual.grad))
                expected_state = optimizers[0].state.get(expected, {})
                actual_state = optimizers[1].state.get(actual, {})
                self.assertEqual(expected_state.keys(), actual_state.keys())
                for key, value in expected_state.items():
                    self.assertTrue(torch.equal(value, actual_state[key]), key)


if __name__ == "__main__":
    unittest.main()
