"""学習中の重みの等重み平均と指数移動平均。"""

import torch

from .trainer_checkpoint import ResumeError


class WeightAverage:
    def __init__(self, model, *, start_step, every, decay):
        self.model_state = model.state_dict()
        self.start_step = start_step
        self.every = every
        self.decay = decay
        self.samples = 0
        self.weights = {}

    @property
    def last_step(self):
        return self.start_step + (self.samples - 1) * self.every

    @torch.no_grad()
    def sample(self, step):
        if step < self.start_step or (step - self.start_step) % self.every:
            return
        self.samples += 1
        if self.samples == 1:
            self.weights = {
                name: weight.detach().clone() for name, weight in self.model_state.items()
            }
            return
        contribution = 1 / self.samples if self.decay is None else 1 - self.decay
        torch._foreach_lerp_(list(self.weights.values()), list(self.model_state.values()), contribution)

    def state_dict(self):
        return {"samples": self.samples, "weights": self.weights}

    def load_state_dict(self, state, step):
        expected_samples = max(0, 1 + (step - self.start_step) // self.every)
        if (
            type(state) is not dict or set(state) != {"samples", "weights"}
            or type(state["samples"]) is not int or state["samples"] != expected_samples
            or type(state["weights"]) is not dict
            or state["weights"].keys() != (self.model_state.keys() if expected_samples else set())
        ):
            raise ResumeError("weight average state differs from the sampling schedule")
        weights = {}
        for name, weight in state["weights"].items():
            parameter = self.model_state[name]
            if (
                not isinstance(weight, torch.Tensor) or weight.layout != torch.strided
                or weight.shape != parameter.shape or weight.dtype != parameter.dtype
                or not bool(torch.isfinite(weight).all())
            ):
                raise ResumeError(f"invalid averaged weight: {name}")
            weights[name] = weight.to(device=parameter.device, copy=True)
        self.weights = {name: weights[name] for name in self.model_state if name in weights}
        self.samples = expected_samples
