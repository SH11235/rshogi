from __future__ import annotations

import copy
from dataclasses import replace
import unittest
from unittest.mock import patch

import torch

from rshogi_dl._policy_grad import clear_unused_policy_head_gradients, has_policy_targets
from rshogi_dl.data import collate, validate_record
from rshogi_dl.loss import policy_value_loss
from rshogi_dl.model import SquareTransformer, TEST_CONFIG

from .support import empty_board_tokens


def batch_with_policy(present: bool):
    return collate([
        validate_record({
            "tokens": empty_board_tokens(), "legal_labels": [3, 7],
            "policy": [[3, 1], [7, 2]] if present else None, "value": value,
        })
        for value in (0.25, 0.75)
    ])


def head_snapshot(model, optimizer):
    return [
        (parameter.detach().clone(), copy.deepcopy(optimizer.state.get(parameter)))
        for parameter in model.policy_head.parameters()
    ]


class PolicyGradTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.previous_threads = torch.get_num_threads()
        cls.previous_rng = torch.random.get_rng_state()
        torch.set_num_threads(1)

    @classmethod
    def tearDownClass(cls) -> None:
        torch.set_num_threads(cls.previous_threads)
        torch.random.set_rng_state(cls.previous_rng)

    def test_presence_uses_prepared_shape_and_cpu_only_legacy_fallback(self) -> None:
        for present in (False, True):
            batch = batch_with_policy(present)
            with (
                patch.object(torch.Tensor, "any", side_effect=AssertionError("no reduction")),
                patch.object(torch.Tensor, "item", side_effect=AssertionError("no scalar transfer")),
            ):
                self.assertEqual(has_policy_targets(batch), present)
            self.assertEqual(has_policy_targets(replace(batch, policy_rows=None)), present)
        batch = batch_with_policy(False)
        not_cpu = replace(batch, policy_rows=None, policy_present=torch.empty(2, device="meta", dtype=torch.bool))
        with patch.object(torch.Tensor, "any", side_effect=AssertionError("must reject before reduction")):
            with self.assertRaisesRegex(ValueError, "CPU"):
                has_policy_targets(not_cpu)

    def test_actual_aot_materializes_zero_policy_grad_and_weight_decay_updates_it(self) -> None:
        torch.random.default_generator.manual_seed(71)
        model = SquareTransformer(TEST_CONFIG)
        execution = torch.compile(model, backend="aot_eager")
        optimizer = torch.optim.AdamW(model.parameters(), lr=1e-3, weight_decay=0.2)
        batch = batch_with_policy(False)
        before = model.policy_head.weight.detach().clone()
        policy, value = execution(batch.tokens)
        policy_value_loss(policy, value, batch).total.backward()
        for parameter in model.policy_head.parameters():
            self.assertIsNotNone(parameter.grad)
            self.assertTrue(torch.equal(parameter.grad, torch.zeros_like(parameter)))
        optimizer.step()
        self.assertFalse(torch.equal(before, model.policy_head.weight))
        self.assertEqual(optimizer.state[model.policy_head.weight]["step"].item(), 1)

    def test_null_present_null_present_preserves_head_state_and_eager_semantics(self) -> None:
        torch.random.default_generator.manual_seed(72)
        eager = SquareTransformer(TEST_CONFIG)
        model = copy.deepcopy(eager)
        execution = torch.compile(model, backend="aot_eager")
        eager_optimizer = torch.optim.AdamW(eager.parameters(), lr=1e-3, weight_decay=0.2)
        optimizer = torch.optim.AdamW(model.parameters(), lr=1e-3, weight_decay=0.2)
        policy_steps = 0
        for step, present in enumerate((False, True, False, True), 1):
            batch = batch_with_policy(present)
            eager_optimizer.zero_grad(set_to_none=True)
            policy, value = eager(batch.tokens)
            policy_value_loss(policy, value, batch).total.backward()
            if not present:
                self.assertIsNone(eager.policy_head.weight.grad)
            eager_optimizer.step()

            before = head_snapshot(model, optimizer)
            optimizer.zero_grad(set_to_none=True)
            policy, value = execution(batch.tokens)
            policy_value_loss(policy, value, batch).total.backward()
            backbone_grad = model.position_embedding.grad
            backbone_before = backbone_grad.clone()
            clear_unused_policy_head_gradients(model, has_policy_targets(batch))
            self.assertIs(model.position_embedding.grad, backbone_grad)
            self.assertTrue(torch.equal(model.position_embedding.grad, backbone_before))
            for parameter in model.policy_head.parameters():
                self.assertEqual(parameter.grad is None, not present)
            optimizer.step()
            if present:
                policy_steps += 1
            else:
                after = head_snapshot(model, optimizer)
                for (old_weight, old_state), (new_weight, new_state) in zip(before, after, strict=True):
                    self.assertTrue(torch.equal(old_weight, new_weight))
                    if old_state is None:
                        self.assertIsNone(new_state)
                    else:
                        self.assertEqual(old_state.keys(), new_state.keys())
                        for key in old_state:
                            self.assertTrue(torch.equal(old_state[key], new_state[key]), key)
            self.assertEqual(optimizer.state[model.position_embedding]["step"].item(), step)
            for parameter in model.policy_head.parameters():
                state = optimizer.state.get(parameter)
                self.assertEqual(state["step"].item() if state else 0, policy_steps)
            for (name, expected), (actual_name, actual) in zip(eager.named_parameters(), model.named_parameters(), strict=True):
                self.assertEqual(name, actual_name)
                torch.testing.assert_close(actual, expected, rtol=1e-5, atol=1e-6)
                expected_state = eager_optimizer.state.get(expected)
                actual_state = optimizer.state.get(actual)
                if expected_state is None:
                    self.assertIsNone(actual_state)
                else:
                    self.assertEqual(expected_state.keys(), actual_state.keys())
                    for key in expected_state:
                        if key == "step":
                            self.assertTrue(torch.equal(expected_state[key], actual_state[key]))
                        else:
                            torch.testing.assert_close(actual_state[key], expected_state[key], rtol=1e-5, atol=1e-6)

    def test_single_legal_policy_zero_loss_still_advances_head_optimizer(self) -> None:
        torch.random.default_generator.manual_seed(73)
        model = SquareTransformer(TEST_CONFIG)
        execution = torch.compile(model, backend="aot_eager")
        optimizer = torch.optim.AdamW(model.parameters(), lr=1e-3, weight_decay=0.2)
        first = batch_with_policy(True)
        policy, value = execution(first.tokens)
        policy_value_loss(policy, value, first).total.backward()
        clear_unused_policy_head_gradients(model, has_policy_targets(first))
        optimizer.step()
        self.assertTrue(bool((optimizer.state[model.policy_head.weight]["exp_avg"] != 0).any()))

        single = collate([
            validate_record({
                "tokens": empty_board_tokens(), "legal_labels": [3],
                "policy": [[3, 1]], "value": value,
            })
            for value in (0.25, 0.75)
        ])
        before = head_snapshot(model, optimizer)
        optimizer.zero_grad(set_to_none=True)
        policy, value = execution(single.tokens)
        loss = policy_value_loss(policy, value, single)
        self.assertEqual(loss.policy.item(), 0.0)
        self.assertTrue(has_policy_targets(single))
        loss.total.backward()
        clear_unused_policy_head_gradients(model, has_policy_targets(single))
        for parameter in model.policy_head.parameters():
            self.assertIsNotNone(parameter.grad)
            self.assertTrue(torch.equal(parameter.grad, torch.zeros_like(parameter)))
        optimizer.step()
        after_single = head_snapshot(model, optimizer)
        for (old_weight, old_state), (new_weight, new_state) in zip(before, after_single, strict=True):
            self.assertFalse(torch.equal(old_weight, new_weight))
            self.assertEqual(new_state["step"].item(), old_state["step"].item() + 1)
        self.assertFalse(torch.equal(before[0][1]["exp_avg"], after_single[0][1]["exp_avg"]))

        null = batch_with_policy(False)
        optimizer.zero_grad(set_to_none=True)
        policy, value = execution(null.tokens)
        policy_value_loss(policy, value, null).total.backward()
        clear_unused_policy_head_gradients(model, has_policy_targets(null))
        optimizer.step()
        after_null = head_snapshot(model, optimizer)
        for (old_weight, old_state), (new_weight, new_state) in zip(after_single, after_null, strict=True):
            self.assertTrue(torch.equal(old_weight, new_weight))
            for key in old_state:
                self.assertTrue(torch.equal(old_state[key], new_state[key]), key)
        self.assertEqual(optimizer.state[model.position_embedding]["step"].item(), 3)


if __name__ == "__main__":
    unittest.main()
