"""Muon updates for hidden linear weights with AdamW for the remainder."""

from collections import defaultdict

import torch

from .trainer_checkpoint import ResumeError


class BatchedMuon(torch.optim.Muon):
    def __init__(self, params, *, lr, weight_decay, fuse_symmetric_polynomial=False):
        super().__init__(params, lr=lr, weight_decay=weight_decay, nesterov=True, adjust_lr_fn="match_rms_adamw")
        self._symmetric_polynomial = None
        if fuse_symmetric_polynomial:
            try:
                from ._muon_symmetric import symmetric_polynomial
            except ImportError as error:
                raise ValueError("fuse_muon_symmetric_polynomial requires CUDA Triton") from error
            self._symmetric_polynomial = symmetric_polynomial

    @torch.no_grad()
    def step(self, closure=None):
        if closure is not None:
            raise ValueError("batched Muon does not take a closure")
        for group in self.param_groups:
            shapes = defaultdict(list)
            for parameter in group["params"]:
                if parameter.grad is not None:
                    shapes[parameter.shape].append(parameter)
            a, b, c = group["ns_coefficients"]
            momentum = group["momentum"]
            for shape, parameters in shapes.items():
                gradients = [p.grad for p in parameters]
                buffers = []
                for parameter in parameters:
                    state = self.state[parameter]
                    if "momentum_buffer" not in state:
                        state["momentum_buffer"] = torch.zeros_like(parameter)
                    buffers.append(state["momentum_buffer"])
                torch._foreach_lerp_(buffers, gradients, 1 - momentum)
                update = torch.stack(torch._foreach_lerp(gradients, buffers, momentum)).bfloat16()
                if shape[0] > shape[1]:
                    update = update.mT
                norm = torch.linalg.vector_norm(update, dim=(-2, -1), keepdim=True)
                update.div_(norm.clamp_min_(group["eps"]))
                symmetric = self._symmetric_polynomial
                if symmetric is not None and (
                    not update.is_cuda or min(shape) != 256 or max(shape) % 32 != 0
                    or len(parameters) * 256 * 256 >= 2**31
                ):
                    symmetric = None
                for _ in range(group["ns_steps"]):
                    gram = torch.bmm(update, update.mT)
                    if symmetric is None:
                        polynomial = torch.baddbmm(gram, gram, gram, beta=b, alpha=c)
                    else:
                        polynomial = symmetric(gram, linear=b, quadratic=c)
                    update = torch.baddbmm(update, polynomial, update, beta=a)
                if shape[0] > shape[1]:
                    update = update.mT
                lr = group["lr"]
                if group["weight_decay"]:
                    torch._foreach_mul_(parameters, 1 - lr * group["weight_decay"])
                rate = lr * (0.2 * max(shape) ** 0.5)
                torch._foreach_add_(parameters, tuple(update.float().unbind()), alpha=-rate)


def validate_optimizer_groups(saved, optimizer, label) -> None:
    expected = optimizer.state_dict()["param_groups"]
    if (
        type(saved) is not dict or set(saved) != {"state", "param_groups"}
        or type(saved["state"]) is not dict or saved["param_groups"] != expected
    ):
        raise ResumeError(f"{label} parameter groups differ from configuration")
    indices = {index for group in expected for index in group["params"]}
    if any(type(index) is not int or index not in indices for index in saved["state"]):
        raise ResumeError(f"unknown {label} parameter in optimizer state")


def _validate_momentum(state, parameter) -> None:
    if type(state) is not dict or set(state) != {"momentum_buffer"}:
        raise ResumeError("invalid Muon state fields")
    buffer = state["momentum_buffer"]
    if (
        not isinstance(buffer, torch.Tensor) or buffer.shape != parameter.shape
        or buffer.dtype != parameter.dtype or not bool(torch.isfinite(buffer).all())
    ):
        raise ResumeError("invalid Muon momentum buffer")


def validate_adamw_state(state, parameter, updates=None, *, serialized=False) -> None:
    if type(state) is not dict or set(state) != {"step", "exp_avg", "exp_avg_sq"}:
        raise ResumeError("unexpected AdamW state")
    for name in ("exp_avg", "exp_avg_sq"):
        value = state[name]
        if (
            not isinstance(value, torch.Tensor) or value.shape != parameter.shape
            or value.dtype != parameter.dtype or not bool(torch.isfinite(value).all())
        ):
            raise ResumeError("invalid AdamW moments")
        if name == "exp_avg_sq" and bool((value < 0).any()):
            raise ResumeError("AdamW second moments must be non-negative")
        if updates == 0 and bool((value != 0).any()):
            raise ResumeError("unupdated AdamW moments must be zero")
    step = state["step"]
    if (
        not isinstance(step, torch.Tensor) or step.numel() != 1
        or not bool(torch.isfinite(step).all())
        or (serialized and (step.ndim != 0 or step.dtype != torch.float32))
    ):
        raise ResumeError("invalid AdamW step")
    value = step.item()
    if value < 0 or value != int(value):
        raise ResumeError("invalid AdamW step")
    if updates is not None and int(value) != updates:
        raise ResumeError("AdamW step differs from recorded parameter update count")


class MuonAdamW:
    def __init__(
        self, model, *, lr, muon_lr, weight_decay, capturable, batched, fuse_symmetric_polynomial=False,
        adamw_betas=(0.9, 0.999), adamw_epsilon=1e-8,
    ):
        groups = model.parameter_groups()
        selected = {p for key in ("attention", "ffn") for p in groups[key] if p.ndim == 2}
        adamw, muon = [], []
        for name, parameter in model.named_parameters():
            (muon if parameter in selected else adamw).append((name, parameter))
        self._names = {"adamw": [name for name, _ in adamw], "muon": [name for name, _ in muon]}
        self.adamw_parameters = tuple(p for _, p in adamw)
        self._muon_parameters = tuple(p for _, p in muon)
        self._adamw = torch.optim.AdamW(
            self.adamw_parameters, lr=lr, weight_decay=weight_decay, betas=adamw_betas, eps=adamw_epsilon,
            fused=True, capturable=capturable,
        )
        if batched:
            self._muon = BatchedMuon(
                self._muon_parameters, lr=muon_lr, weight_decay=weight_decay,
                fuse_symmetric_polynomial=fuse_symmetric_polynomial,
            )
        else:
            self._muon = torch.optim.Muon(
                self._muon_parameters, lr=muon_lr, weight_decay=weight_decay, adjust_lr_fn="match_rms_adamw",
            )

    @property
    def state(self):
        return self._adamw.state

    def set_learning_rates(self, adamw_rate, muon_rate):
        for optimizer, rate in ((self._adamw, adamw_rate), (self._muon, muon_rate)):
            for group in optimizer.param_groups:
                group["lr"] = rate

    def zero_grad(self, *, set_to_none):
        self._adamw.zero_grad(set_to_none=set_to_none)
        self._muon.zero_grad(set_to_none=set_to_none)

    def step(self):
        self._adamw.step()
        self._muon.step()

    def state_dict(self):
        return {"parameter_names": self._names, "adamw": self._adamw.state_dict(), "muon": self._muon.state_dict()}

    def load_state_dict(self, saved):
        if type(saved) is not dict or set(saved) != {"parameter_names", "adamw", "muon"}:
            raise ResumeError("invalid hybrid optimizer state fields")
        if saved["parameter_names"] != self._names:
            raise ResumeError("hybrid optimizer parameter order differs")
        validate_optimizer_groups(saved["adamw"], self._adamw, "AdamW")
        validate_optimizer_groups(saved["muon"], self._muon, "Muon")
        for index, state in saved["adamw"]["state"].items():
            validate_adamw_state(state, self.adamw_parameters[index], serialized=True)
        for index, state in saved["muon"]["state"].items():
            _validate_momentum(state, self._muon_parameters[index])
        self._adamw.load_state_dict(saved["adamw"])
        self._muon.load_state_dict(saved["muon"])

    def validate_muon(self, updates):
        expected = set(self._muon_parameters) if updates else set()
        if set(self._muon.state) != expected:
            raise ResumeError("Muon momentum coverage differs from training progress")
        for parameter, state in self._muon.state.items():
            _validate_momentum(state, parameter)
