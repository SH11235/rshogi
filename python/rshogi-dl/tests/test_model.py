from __future__ import annotations

import random
import unittest

import torch

from rshogi_dl.contract import CONTEXT_POSITION, POLICY_SIZE, SQUARE_COUNT
from rshogi_dl.data import collate, validate_record
from rshogi_dl.loss import policy_value_loss
from rshogi_dl.model import (
    ATTENTION_BACKENDS,
    INITIAL_CONFIG,
    TEST_CONFIG,
    ExportModule,
    ModelConfig,
    SquareTransformer,
    flatten_policy,
)

from .support import random_tokens, synthetic_records


def token_batch(seed: int, rows: int) -> torch.Tensor:
    rng = random.Random(seed)
    return torch.tensor([random_tokens(rng) for _ in range(rows)], dtype=torch.int64)


def build(config: ModelConfig = TEST_CONFIG, seed: int = 0) -> SquareTransformer:
    torch.manual_seed(seed)
    return SquareTransformer(config)


class ConfigTest(unittest.TestCase):
    def test_named_configs(self) -> None:
        self.assertEqual(
            INITIAL_CONFIG.to_dict(), {"layers": 4, "width": 128, "heads": 4, "ffn_width": 512}
        )
        self.assertEqual(
            TEST_CONFIG.to_dict(), {"layers": 2, "width": 32, "heads": 4, "ffn_width": 128}
        )
        self.assertEqual(ModelConfig.from_dict(TEST_CONFIG.to_dict()), TEST_CONFIG)

    def test_invalid_configs(self) -> None:
        with self.assertRaises(ValueError):
            ModelConfig(layers=1, width=30, heads=4, ffn_width=8)
        with self.assertRaises(ValueError):
            ModelConfig(layers=0, width=32, heads=4, ffn_width=8)
        with self.assertRaises(ValueError):
            ModelConfig(layers=True, width=32, heads=4, ffn_width=8)
        with self.assertRaises(ValueError):
            ModelConfig.from_dict({"layers": 1, "width": 32, "heads": 4})
        with self.assertRaises(ValueError):
            ModelConfig.from_dict({"layers": 1, "width": 32, "heads": 4, "ffn_width": 8, "x": 1})


class ForwardTest(unittest.TestCase):
    def test_output_shapes(self) -> None:
        for config in (TEST_CONFIG, INITIAL_CONFIG):
            with self.subTest(config=config):
                model = build(config).eval()
                with torch.no_grad():
                    policy, value = model(token_batch(1, 3))
                self.assertEqual(policy.shape, (3, POLICY_SIZE))
                self.assertEqual(value.shape, (3, 1))
                self.assertTrue(torch.isfinite(policy).all())
                self.assertTrue(torch.isfinite(value).all())

    def test_rejects_wrong_input(self) -> None:
        model = build()
        with self.assertRaises(ValueError):
            model(token_batch(1, 2)[:, :95])
        with self.assertRaises(ValueError):
            model(token_batch(1, 2).to(torch.int32))
        with self.assertRaises(ValueError):
            model(token_batch(1, 1)[0])

    def test_seed_determines_initialization(self) -> None:
        first = build(seed=5).state_dict()
        second = build(seed=5).state_dict()
        other = build(seed=6).state_dict()
        self.assertTrue(all(torch.equal(first[key], second[key]) for key in first))
        self.assertFalse(all(torch.equal(first[key], other[key]) for key in first))

    def test_export_module_applies_sigmoid_to_value_only(self) -> None:
        model = build().eval()
        tokens = token_batch(2, 2)
        with torch.no_grad():
            policy, value_logit = model(tokens)
            exported_policy, exported_value = ExportModule(model)(tokens)
        self.assertTrue(torch.equal(policy, exported_policy))
        self.assertTrue(torch.equal(torch.sigmoid(value_logit), exported_value))


class PolicyLayoutTest(unittest.TestCase):
    def test_flatten_is_channel_major(self) -> None:
        per_square = torch.empty(2, SQUARE_COUNT, 27)
        for batch in range(2):
            for square in range(SQUARE_COUNT):
                for channel in range(27):
                    per_square[batch, square, channel] = batch * 10000 + channel * 100 + square
        flat = flatten_policy(per_square)
        self.assertEqual(flat.shape, (2, POLICY_SIZE))
        for batch in range(2):
            for channel in range(27):
                for square in range(SQUARE_COUNT):
                    self.assertEqual(
                        flat[batch, SQUARE_COUNT * channel + square].item(),
                        batch * 10000 + channel * 100 + square,
                    )

    def test_heads_read_the_documented_positions(self) -> None:
        model = build().eval()
        captured = {}
        handle = model.final_norm.register_forward_hook(
            lambda module, inputs, output: captured.update(hidden=output)
        )
        tokens = token_batch(3, 2)
        with torch.no_grad():
            policy, value = model(tokens)
            hidden = captured["hidden"]
            self.assertEqual(hidden.shape, (2, 96, TEST_CONFIG.width))
            per_square = model.policy_head(hidden[:, :SQUARE_COUNT])
            expected_value = model.value_head(hidden[:, CONTEXT_POSITION])
        handle.remove()
        for square in (0, 40, 80):
            for channel in (0, 13, 26):
                self.assertTrue(
                    torch.equal(
                        policy[:, SQUARE_COUNT * channel + square], per_square[:, square, channel]
                    )
                )
        self.assertTrue(torch.equal(value, expected_value))


class AttentionTest(unittest.TestCase):
    def test_information_flows_between_all_token_kinds(self) -> None:
        model = build().eval()
        base = token_batch(4, 1)

        def outputs(position: int) -> tuple[torch.Tensor, torch.Tensor]:
            tokens = base.clone()
            # 位置ごとの ID 範囲の中で別の値に変える。
            low = {0: 0, 81: 29, 94: 114, 95: 119}[position]
            tokens[0, position] = low + (tokens[0, position] - low + 1) % 2
            with torch.no_grad():
                return model(tokens)

        with torch.no_grad():
            policy, value = model(base)
        far_square = slice(80, POLICY_SIZE, SQUARE_COUNT)
        for position in (0, 81, 94):
            changed_policy, changed_value = outputs(position)
            with self.subTest(position=position):
                # 変えた位置以外のマスの policy と、最後の token から読む value が動く。
                self.assertFalse(torch.equal(policy[:, far_square], changed_policy[:, far_square]))
                self.assertFalse(torch.equal(value, changed_value))
        changed_policy, _ = outputs(95)
        self.assertFalse(torch.equal(policy, changed_policy))

    def test_batch_rows_are_independent(self) -> None:
        model = build().eval()
        tokens = token_batch(5, 4)
        with torch.no_grad():
            policy, value = model(tokens)
            single_policy, single_value = model(tokens[2:3])
        self.assertTrue(torch.allclose(policy[2:3], single_policy, rtol=1e-5, atol=1e-6))
        self.assertTrue(torch.allclose(value[2:3], single_value, rtol=1e-5, atol=1e-6))


class AttentionBackendTest(unittest.TestCase):
    def test_default_and_invalid_backend(self) -> None:
        self.assertEqual(SquareTransformer(TEST_CONFIG).attention_backend, "explicit")
        self.assertEqual(ATTENTION_BACKENDS, ("explicit", "sdpa"))
        for backend in ("flash", "", None, True):
            with self.subTest(backend=backend), self.assertRaises(ValueError):
                SquareTransformer(TEST_CONFIG, attention_backend=backend)

    def test_state_layout_outputs_losses_and_gradients_match(self) -> None:
        for config in (TEST_CONFIG, INITIAL_CONFIG):
            for rows in (1, 3):
                with self.subTest(config=config, rows=rows):
                    explicit = build(config, seed=19)
                    sdpa = SquareTransformer(config, attention_backend="sdpa")
                    sdpa.load_state_dict(explicit.state_dict(), strict=True)
                    self.assertEqual(explicit.state_dict().keys(), sdpa.state_dict().keys())
                    records = synthetic_records(seed=23, count=rows)
                    batch = collate([validate_record(record) for record in records])
                    outputs = [model(batch.tokens) for model in (explicit, sdpa)]
                    for expected, actual in zip(*outputs, strict=True):
                        torch.testing.assert_close(actual, expected, rtol=1e-4, atol=1e-5)
                    losses = [policy_value_loss(*output, batch) for output in outputs]
                    torch.testing.assert_close(losses[0].total, losses[1].total, rtol=1e-5, atol=1e-6)
                    for loss in losses:
                        loss.total.backward()
                    for (name, expected), (_, actual) in zip(
                        explicit.named_parameters(), sdpa.named_parameters(), strict=True
                    ):
                        self.assertIsNotNone(expected.grad, name)
                        self.assertIsNotNone(actual.grad, name)
                        self.assertTrue(torch.isfinite(actual.grad).all(), name)
                        torch.testing.assert_close(actual.grad, expected.grad, rtol=1e-4, atol=1e-6, msg=name)
                    for model in (explicit, sdpa):
                        torch.optim.SGD(model.parameters(), lr=1e-3).step()
                    for (name, expected), (_, actual) in zip(
                        explicit.named_parameters(), sdpa.named_parameters(), strict=True
                    ):
                        torch.testing.assert_close(actual, expected, rtol=1e-4, atol=1e-6, msg=name)

    def test_adamw_update_matches_in_double_precision(self) -> None:
        explicit = build(seed=29).double()
        sdpa = SquareTransformer(TEST_CONFIG, attention_backend="sdpa").double()
        sdpa.load_state_dict(explicit.state_dict(), strict=True)
        batch = collate([validate_record(record) for record in synthetic_records(seed=31, count=3)])
        optimizers = [torch.optim.AdamW(model.parameters(), lr=1e-3) for model in (explicit, sdpa)]
        for model, optimizer in zip((explicit, sdpa), optimizers, strict=True):
            policy, value = model(batch.tokens)
            policy_value_loss(policy, value, batch).total.backward()
            optimizer.step()
        for (name, expected), (_, actual) in zip(
            explicit.named_parameters(), sdpa.named_parameters(), strict=True
        ):
            torch.testing.assert_close(actual, expected, rtol=1e-8, atol=1e-10, msg=name)


class GradientTest(unittest.TestCase):
    def setUp(self) -> None:
        self.model = build()
        self.model.train()
        self.groups = self.model.parameter_groups()

    def test_groups_partition_all_parameters(self) -> None:
        grouped = [id(p) for parameters in self.groups.values() for p in parameters]
        self.assertEqual(len(grouped), len(set(grouped)))
        self.assertEqual(set(grouped), {id(p) for p in self.model.parameters()})
        self.assertEqual(
            set(self.groups),
            {
                "token_embedding",
                "position_embedding",
                "attention",
                "ffn",
                "norm",
                "policy_head",
                "value_head",
            },
        )

    def _backward(self, records: list[dict]) -> None:
        batch = collate([validate_record(record) for record in records])
        policy, value = self.model(batch.tokens)
        loss = policy_value_loss(policy, value, batch)
        self.assertTrue(torch.isfinite(loss.total))
        loss.total.backward()

    def test_every_group_gets_finite_nonzero_gradient_and_is_updated(self) -> None:
        optimizer = torch.optim.AdamW(self.model.parameters(), lr=1e-3, weight_decay=0.0)
        before = {
            name: [p.detach().clone() for p in parameters]
            for name, parameters in self.groups.items()
        }
        self._backward(synthetic_records(seed=7, count=8))
        for name, parameters in self.groups.items():
            with self.subTest(group=name):
                self._assert_group_gradient(parameters)
        optimizer.step()
        for name, parameters in self.groups.items():
            with self.subTest(group=name):
                changed = False
                for parameter, old in zip(parameters, before[name], strict=True):
                    self.assertTrue(torch.isfinite(parameter).all())
                    changed = changed or not torch.equal(parameter, old)
                self.assertTrue(changed)

    def _assert_group_gradient(self, parameters: list[torch.nn.Parameter]) -> None:
        # key の bias のように勾配が恒等的に 0 になる parameter があるので、非 zero は group 単位で見る。
        total = 0.0
        for parameter in parameters:
            self.assertIsNotNone(parameter.grad)
            self.assertTrue(torch.isfinite(parameter.grad).all())
            total += parameter.grad.abs().sum().item()
        self.assertGreater(total, 0.0)

    def test_policy_null_batch_trains_value_path_only(self) -> None:
        records = synthetic_records(seed=8, count=4)
        for record in records:
            record["policy"] = None
        self._backward(records)
        for name, parameters in self.groups.items():
            with self.subTest(group=name):
                if name == "policy_head":
                    self.assertTrue(all(parameter.grad is None for parameter in parameters))
                else:
                    self._assert_group_gradient(parameters)


if __name__ == "__main__":
    unittest.main()
