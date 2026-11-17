from __future__ import annotations

from dataclasses import replace
import unittest

import torch

from rshogi_dl import benchmark
from rshogi_dl.data import Batch, collate, validate_record
from rshogi_dl.loss import policy_value_loss
from rshogi_dl.model import TEST_CONFIG, SquareTransformer
from .support import synthetic_records


def make_batch(seed: int, present: list[bool], legal_width: int) -> Batch:
    records = synthetic_records(seed=seed, count=len(present))
    for index, (record, enabled) in enumerate(zip(records, present, strict=True)):
        legal = list(range(seed * 7 + index, seed * 7 + index + legal_width))
        record["legal_labels"] = legal if enabled else []
        record["policy"] = [[legal[(seed + index) % legal_width], 1.0]] if enabled else None
        record["value"] = ((seed + index) % 5) / 4
    return collate([validate_record(record) for record in records])


def copy_state(optimizer, parameters):
    return [
        (parameter.detach().clone(), {key: value.clone() for key, value in optimizer.state.get(parameter, {}).items()})
        for parameter in parameters
    ]


class ModelLossCompileTest(unittest.TestCase):
    def test_requires_prepared_rows(self) -> None:
        model = benchmark._ModelLoss(SquareTransformer(TEST_CONFIG))
        with self.assertRaisesRegex(benchmark.BenchmarkError, "prepared policy rows"):
            model(replace(make_batch(1, [True], 1), policy_rows=None))

    def test_aot_graph_tracks_changed_inputs_shapes_and_missing_policy_adamw_state(self) -> None:
        torch.manual_seed(97)
        expected = SquareTransformer(TEST_CONFIG).double()
        actual = SquareTransformer(TEST_CONFIG).double()
        actual.load_state_dict(expected.state_dict())
        original_names = tuple(actual.state_dict())
        compiled = torch.compile(benchmark._ModelLoss(actual), backend="aot_eager", fullgraph=True)
        models = (expected, actual)
        optimizers = [torch.optim.AdamW(model.parameters(), lr=1e-3, weight_decay=0.1) for model in models]
        batches = [
            make_batch(1, [False, False], 3),
            make_batch(2, [True, True], 3),
            make_batch(3, [False, False], 3),
            make_batch(4, [True, False, True], 7),
            make_batch(5, [True], 1),
            make_batch(6, [True, False, True], 7),
        ]
        losses = []
        for index, batch in enumerate(batches):
            with self.subTest(index=index, rows=batch.tokens.shape[0], legal_width=batch.legal_index.shape[1]):
                snapshots = [copy_state(optimizer, list(model.policy_head.parameters()))
                             for model, optimizer in zip(models, optimizers, strict=True)]
                for optimizer in optimizers:
                    optimizer.zero_grad(set_to_none=True)
                expected_loss = policy_value_loss(*expected(batch.tokens), batch).total
                actual_loss = compiled(batch)
                torch.testing.assert_close(actual_loss, expected_loss, rtol=1e-8, atol=1e-9)
                losses.append(actual_loss.item())
                expected_loss.backward()
                actual_loss.backward()
                for (name, want), (_, got) in zip(expected.named_parameters(), actual.named_parameters(), strict=True):
                    if want.grad is None:
                        self.assertIsNone(got.grad, name)
                    else:
                        torch.testing.assert_close(got.grad, want.grad, rtol=1e-7, atol=1e-9, msg=name)
                for optimizer in optimizers:
                    optimizer.step()
                if batch.policy_rows.numel() == 0:
                    for model, optimizer, snapshot in zip(models, optimizers, snapshots, strict=True):
                        for parameter, (old_weight, old_state) in zip(model.policy_head.parameters(), snapshot, strict=True):
                            self.assertIsNone(parameter.grad)
                            self.assertTrue(torch.equal(parameter, old_weight))
                            current = optimizer.state.get(parameter, {})
                            self.assertEqual(current.keys(), old_state.keys())
                            for key, old_value in old_state.items():
                                self.assertTrue(torch.equal(current[key], old_value), key)
                for (name, want), (_, got) in zip(expected.named_parameters(), actual.named_parameters(), strict=True):
                    torch.testing.assert_close(got, want, rtol=1e-7, atol=1e-9, msg=name)
                    want_state, got_state = optimizers[0].state.get(want, {}), optimizers[1].state.get(got, {})
                    self.assertEqual(want_state.keys(), got_state.keys())
                    for key, value in want_state.items():
                        torch.testing.assert_close(got_state[key], value, rtol=1e-7, atol=1e-9, msg=f"{name}.{key}")
        self.assertNotEqual(losses[0], losses[2])
        self.assertNotEqual(losses[3], losses[5])
        self.assertEqual(tuple(actual.state_dict()), original_names)

        batch = batches[-1]
        before = compiled(batch).item()
        batch.tokens[:, 0] = (batch.tokens[:, 0] + 1) % 29
        batch.value.fill_(0.125)
        batch.policy_target.copy_(batch.policy_target.flip(1))
        with torch.no_grad():
            changed = compiled(batch)
            reference = policy_value_loss(*actual(batch.tokens), batch).total
        torch.testing.assert_close(changed, reference, rtol=1e-8, atol=1e-9)
        self.assertNotEqual(before, changed.item())


if __name__ == "__main__":
    unittest.main()
