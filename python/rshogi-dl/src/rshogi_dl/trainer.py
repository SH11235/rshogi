"""教師データを有限回学習し、保持した cursor と optimizer 状態から再開する。"""

from __future__ import annotations

import argparse
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing, contextmanager, nullcontext
from dataclasses import asdict, dataclass, fields
from itertools import islice
import json
import math
import os
from pathlib import Path
import random
import sys
import time

import numpy as np
import torch

from .contract import TRAINING_STATUS
from .data import Batch
from .hcpe3_stream import hcpe3_dataset_info, is_hcpe3_manifest, iter_hcpe3_batches
from .loss import policy_value_loss
from .model import ATTENTION_BACKENDS, NAMED_CONFIGS, ModelConfig, SquareTransformer
from .paths import ArtifactPathError, resolve_new_artifact, shogi_data_root
from ._optimizers import (
    NADAM_MOMENTUM_DECAY, MuonAdamW, TrainerSGD, load_nadam_state, validate_adamw_state, validate_nadam_state,
    validate_optimizer_groups,
)
from ._policy_grad import clear_unused_policy_head_gradients, has_policy_targets
from ._residual_norm import compiled_residual_forward
from ._step_graph import StepGraph
from ._tuned_blocks import tuned_training_forward
from ._weight_average import WeightAverage
from .sharded import DatasetError, dataset_info, iter_batches
from .trainer_checkpoint import (
    CONTRACT, ResumeError, atomic_save, capture_rng, export_checkpoint, load_resume, restore_rng,
    SCALER_BACKOFF_FACTOR, SCALER_GROWTH_FACTOR, SCALER_GROWTH_INTERVAL,
    sync_directory,
    validate_scaler_state,
)

_OPTIONAL_SETTINGS = {
    "policy_loss_weight": 1.0,
    "value_loss_weight": 1.0,
    "adamw_betas": (0.9, 0.999),
    "adamw_epsilon": 1e-8,
    "clip_grad_norm": 0.0,
    "weight_average": None,
    "weight_average_start_step": 1,
    "weight_average_every": 1,
    "ema_decay": 0.999,
    "sgd_momentum": 0.0,
    "sgd_nesterov": False,
}
_OPTIMIZERS = ("adamw", "nadam", "sgd", "muon", "batched-muon")
COMPILE_MODES = ("default", "reduce-overhead", "max-autotune", "max-autotune-no-cudagraphs")
_CUDAGRAPH_MODES = ("reduce-overhead", "max-autotune")
_STEP_GRAPH_EXECUTIONS = ("cuda-graph", "cuda-graph-fused-norm")
_EXECUTIONS = ("eager", "compile", *_STEP_GRAPH_EXECUTIONS)


class TrainingError(RuntimeError):
    """実行条件・学習状態が不正。"""


@dataclass(frozen=True)
class TrainerConfig:
    """max_steps は AMP skip を含む有限の更新試行数。

    再開で変更できるのは総試行数、保存・記録間隔と入力先読み。他の設定は更新列・評価条件を固定する。
    """

    max_steps: int
    batch_size: int = 128
    learning_rate: float = 1e-3
    weight_decay: float = 0.0
    seed: int = 0
    threads: int = 1
    device: str = "cpu"
    attention_backend: str = "explicit"
    execution: str = "eager"
    compile_mode: str = "default"
    precision: str = "fp32"
    fused_optimizer: bool = False
    deterministic: bool = False
    validation_only: bool = False
    validation_batches: int = 32
    checkpoint_every: int = 0
    log_every: int = 100
    optimizer: str = "adamw"
    muon_learning_rate: float | None = None
    compile_ffn: bool = False
    fuse_norm_cast: bool = False
    prefetch_input: bool = False
    compile_attention: bool = False
    fuse_ffn_residual_norm: bool = False
    fuse_packed_attention: bool = False
    fuse_packed_attention_backward: bool = False
    fuse_ffn_residual_norm_backward: bool = False
    tile_packed_attention_backward: bool = False
    fuse_muon_symmetric_polynomial: bool = False
    tile_packed_attention_forward: bool = False
    fuse_attention_residual_norm: bool = False
    fuse_final_residual_norm: bool = False
    fuse_packed_qkv_bias_gradient: bool = False
    fuse_ffn_expand_bias_gradient: bool = False
    tile_packed_attention_short_key_first: bool = False
    lookup_ffn_gelu_derivative: bool = False
    learning_rate_steps: tuple[tuple[int, float], ...] = ()
    policy_loss_weight: float = 1.0
    value_loss_weight: float = 1.0
    adamw_betas: tuple[float, float] = (0.9, 0.999)
    adamw_epsilon: float = 1e-8
    clip_grad_norm: float = 0.0
    warmup_steps: int = 0
    cosine_end_step: int = 0
    cosine_min_ratio: float = 0.1
    weight_average: str | None = None
    weight_average_start_step: int = 1
    weight_average_every: int = 1
    ema_decay: float = 0.999
    sgd_momentum: float = 0.0
    sgd_nesterov: bool = False

    def __post_init__(self) -> None:
        limits = {
            "max_steps": (1, 10_000_000), "batch_size": (1, 4096), "seed": (0, 2**63 - 1),
            "threads": (1, 256), "validation_batches": (0, 1_000_000),
            "warmup_steps": (0, 9_999_999), "cosine_end_step": (0, 10_000_000),
            "checkpoint_every": (0, 10_000_000), "log_every": (1, 1_000_000),
            "weight_average_start_step": (1, 10_000_000), "weight_average_every": (1, 10_000_000),
        }
        for name, (low, high) in limits.items():
            value = getattr(self, name)
            if type(value) is not int or not low <= value <= high:
                raise ValueError(f"{name} must be an integer in {low}..{high}")
        for name in (
            "fused_optimizer", "deterministic", "validation_only",
            "compile_ffn", "compile_attention", "fuse_norm_cast", "prefetch_input", "fuse_ffn_residual_norm",
            "fuse_packed_attention", "fuse_packed_attention_backward",
            "fuse_ffn_residual_norm_backward", "tile_packed_attention_backward",
            "fuse_muon_symmetric_polynomial",
            "tile_packed_attention_forward", "fuse_attention_residual_norm", "fuse_final_residual_norm",
            "fuse_packed_qkv_bias_gradient", "fuse_ffn_expand_bias_gradient",
            "tile_packed_attention_short_key_first", "lookup_ffn_gelu_derivative",
        ):
            if type(getattr(self, name)) is not bool:
                raise ValueError(f"{name} must be boolean")
        for name, choices in {
            "device": ("cpu", "cuda"), "attention_backend": ATTENTION_BACKENDS,
            "execution": _EXECUTIONS, "precision": ("fp32", "tf32", "fp16", "bf16"),
            "compile_mode": COMPILE_MODES, "optimizer": _OPTIMIZERS,
        }.items():
            if getattr(self, name) not in choices:
                raise ValueError(f"{name} must be one of {choices}")
        for name in (
            "learning_rate", "weight_decay", "policy_loss_weight", "value_loss_weight",
            "adamw_epsilon", "clip_grad_norm",
        ):
            value = getattr(self, name)
            if type(value) not in (int, float) or not math.isfinite(value):
                raise ValueError(f"{name} must be finite")
            if value < 0 or (name in ("learning_rate", "adamw_epsilon", "value_loss_weight") and value == 0):
                raise ValueError(f"invalid {name}")
        if (
            type(self.adamw_betas) not in (list, tuple) or len(self.adamw_betas) != 2
            or any(type(beta) not in (int, float) or not 0 <= beta < 1 for beta in self.adamw_betas)
        ):
            raise ValueError("adamw_betas must contain two values in [0,1)")
        object.__setattr__(self, "adamw_betas", tuple(float(beta) for beta in self.adamw_betas))
        if self.weight_average not in (None, "swa", "ema"):
            raise ValueError("weight_average must be swa or ema")
        if type(self.ema_decay) not in (int, float) or not 0 <= self.ema_decay < 1:
            raise ValueError("ema_decay must be in [0,1)")
        if self.weight_average != "ema" and self.ema_decay != _OPTIONAL_SETTINGS["ema_decay"]:
            raise ValueError("ema_decay requires EMA")
        if self.weight_average is None and (self.weight_average_start_step, self.weight_average_every) != (1, 1):
            raise ValueError("weight averaging cadence requires SWA or EMA")
        if type(self.sgd_momentum) not in (int, float) or not 0 <= self.sgd_momentum < 1:
            raise ValueError("sgd_momentum must be in [0,1)")
        if type(self.sgd_nesterov) is not bool:
            raise ValueError("sgd_nesterov must be boolean")
        if self.sgd_nesterov and not self.sgd_momentum:
            raise ValueError("SGD Nesterov acceleration requires positive momentum")
        if self.optimizer != "sgd" and (self.sgd_momentum or self.sgd_nesterov):
            raise ValueError("SGD momentum settings require the SGD optimizer")
        if self.optimizer == "sgd" and (
            self.adamw_betas != _OPTIONAL_SETTINGS["adamw_betas"]
            or self.adamw_epsilon != _OPTIONAL_SETTINGS["adamw_epsilon"]
        ):
            raise ValueError("betas and epsilon require an adaptive optimizer")
        if type(self.learning_rate_steps) not in (tuple, list):
            raise ValueError("learning_rate_steps must be a sequence")
        previous_step, previous_rate = 0, self.learning_rate
        steps = []
        for entry in self.learning_rate_steps:
            if type(entry) not in (tuple, list) or len(entry) != 2:
                raise ValueError("learning rate steps require STEP:RATE")
            step, rate = entry
            if type(step) is not int or not previous_step < step < 10_000_000:
                raise ValueError("learning rate steps must be strictly increasing in 1..9999999")
            if type(rate) not in (int, float) or not 0 < rate < previous_rate:
                raise ValueError("scheduled learning rates must be positive and strictly decreasing")
            steps.append((step, float(rate)))
            previous_step, previous_rate = step, rate
        object.__setattr__(self, "learning_rate_steps", tuple(steps))
        if self.cosine_end_step and self.cosine_end_step <= self.warmup_steps:
            raise ValueError("cosine_end_step must follow warmup")
        if (
            type(self.cosine_min_ratio) not in (int, float)
            or not 0 < self.cosine_min_ratio < 1
            or (not self.cosine_end_step and self.cosine_min_ratio != 0.1)
        ):
            raise ValueError("cosine_min_ratio must be in (0, 1) and requires cosine decay when customized")
        if (
            (self.warmup_steps or self.cosine_end_step)
            and self.execution in _STEP_GRAPH_EXECUTIONS and self.optimizer != "adamw"
        ):
            raise ValueError("continuous learning rate under CUDA Graph requires AdamW")
        if self.learning_rate_steps and self.cosine_end_step:
            raise ValueError("cosine decay and explicit learning rate steps are mutually exclusive")
        if self.learning_rate_steps and self.learning_rate_steps[0][0] < self.warmup_steps:
            raise ValueError("learning rate decay steps must follow warmup")
        if (self.warmup_steps or self.cosine_end_step) and self.execution in _STEP_GRAPH_EXECUTIONS:
            minimum = min(
                self.learning_rate / max(1, self.warmup_steps),
                self.learning_rate * self.cosine_min_ratio if self.cosine_end_step else self.learning_rate,
                self.learning_rate_steps[-1][1] if self.learning_rate_steps else self.learning_rate,
            )
            bounds = torch.finfo(torch.float32)
            if not bounds.tiny <= minimum <= self.learning_rate <= bounds.max:
                raise ValueError("continuous CUDA Graph learning rates must be normal FP32 values")
        if self.optimizer in ("adamw", "nadam", "sgd"):
            if self.muon_learning_rate is not None:
                raise ValueError("muon_learning_rate requires a Muon optimizer")
            if self.optimizer in ("nadam", "sgd") and (self.fused_optimizer or self.execution != "eager"):
                name = "NAdam" if self.optimizer == "nadam" else "SGD"
                raise ValueError(f"{name} requires eager execution without a fused optimizer")
        else:
            rate = self.muon_learning_rate
            if type(rate) not in (int, float) or not math.isfinite(rate) or rate <= 0:
                raise ValueError("Muon requires a positive finite muon_learning_rate")
            if self.device != "cuda" or self.precision != "bf16" or not self.fused_optimizer:
                raise ValueError("Muon requires CUDA, BF16 and fused AdamW")
        if self.fuse_muon_symmetric_polynomial and self.optimizer != "batched-muon":
            raise ValueError("fuse_muon_symmetric_polynomial requires batched-muon")
        if self.device == "cpu" and (self.precision != "fp32" or self.fused_optimizer):
            raise ValueError("CPU requires fp32 and an unfused optimizer")
        if self.execution != "compile" and self.compile_mode != "default":
            raise ValueError("non-default compile_mode requires compiled execution")
        for name in ("compile_ffn", "compile_attention", "prefetch_input", "fuse_packed_attention"):
            if getattr(self, name) and self.execution not in _STEP_GRAPH_EXECUTIONS:
                raise ValueError(f"{name} requires whole-step CUDA Graph execution")
        if self.fuse_packed_attention and self.attention_backend != "sdpa":
            raise ValueError("fuse_packed_attention requires the sdpa attention backend")
        if self.fuse_packed_attention_backward and not self.fuse_packed_attention:
            raise ValueError("fuse_packed_attention_backward requires fuse_packed_attention")
        if self.tile_packed_attention_backward and not self.fuse_packed_attention_backward:
            raise ValueError("tile_packed_attention_backward requires fuse_packed_attention_backward")
        if self.tile_packed_attention_short_key_first and not self.tile_packed_attention_backward:
            raise ValueError("tile_packed_attention_short_key_first requires tile_packed_attention_backward")
        if self.tile_packed_attention_forward and not self.fuse_packed_attention:
            raise ValueError("tile_packed_attention_forward requires fuse_packed_attention")
        if self.fuse_packed_qkv_bias_gradient and not (
            self.tile_packed_attention_forward and self.tile_packed_attention_backward
            and self.fuse_ffn_residual_norm and self.compile_attention
        ):
            raise ValueError(
                "fuse_packed_qkv_bias_gradient requires tiled attention forward and backward, "
                "fuse_ffn_residual_norm and compile_attention"
            )
        for name in ("fuse_norm_cast", "fuse_ffn_residual_norm"):
            if getattr(self, name) and self.execution != "cuda-graph-fused-norm":
                raise ValueError(f"{name} requires cuda-graph-fused-norm execution")
        if self.fuse_ffn_residual_norm_backward and not self.fuse_ffn_residual_norm:
            raise ValueError("fuse_ffn_residual_norm_backward requires fuse_ffn_residual_norm")
        if self.fuse_attention_residual_norm and not self.fuse_norm_cast:
            raise ValueError("fuse_attention_residual_norm requires fuse_norm_cast")
        if self.fuse_final_residual_norm and not self.fuse_norm_cast:
            raise ValueError("fuse_final_residual_norm requires fuse_norm_cast")
        if self.fuse_ffn_expand_bias_gradient and not (self.compile_ffn and self.fuse_norm_cast):
            raise ValueError("fuse_ffn_expand_bias_gradient requires compile_ffn and fuse_norm_cast")
        if self.lookup_ffn_gelu_derivative and not self.fuse_ffn_expand_bias_gradient:
            raise ValueError("lookup_ffn_gelu_derivative requires fuse_ffn_expand_bias_gradient")
        if self.execution in _STEP_GRAPH_EXECUTIONS and (
            self.device != "cuda" or self.precision != "bf16" or not self.fused_optimizer
        ):
            raise ValueError("CUDA Graph execution requires CUDA, BF16 and fused AdamW")
        if self.device == "cpu" and self.compile_mode in _CUDAGRAPH_MODES:
            raise ValueError("CUDA graph compile modes require a CUDA device")
        if self.checkpoint_every and self.max_steps // self.checkpoint_every > 8:
            raise ValueError("at most eight intermediate checkpoints per run are allowed")


def _config_dict(config: TrainerConfig) -> dict:
    payload = asdict(config)
    for name, default in _OPTIONAL_SETTINGS.items():
        if payload[name] == default:
            del payload[name]
    if not config.warmup_steps:
        del payload["warmup_steps"]
    if not config.cosine_end_step:
        del payload["cosine_end_step"], payload["cosine_min_ratio"]
    if not config.learning_rate_steps:
        del payload["learning_rate_steps"]
    if config.optimizer == "adamw":
        del payload["optimizer"], payload["muon_learning_rate"]
    for name in ("compile_ffn", "compile_attention", "fuse_norm_cast", "prefetch_input", "fuse_ffn_residual_norm",
                 "fuse_packed_attention", "fuse_packed_attention_backward",
                 "fuse_ffn_residual_norm_backward", "tile_packed_attention_backward",
                 "fuse_muon_symmetric_polynomial", "tile_packed_attention_forward", "fuse_attention_residual_norm",
                 "fuse_final_residual_norm", "fuse_packed_qkv_bias_gradient", "fuse_ffn_expand_bias_gradient",
                 "tile_packed_attention_short_key_first", "lookup_ffn_gelu_derivative"):
        if not payload[name]:
            del payload[name]
    return payload


def _identity(config: TrainerConfig) -> dict:
    return {
        name: value for name, value in _config_dict(config).items()
        if name not in ("max_steps", "checkpoint_every", "log_every", "prefetch_input")
    }


def _transfer(batch: Batch, device: torch.device) -> Batch:
    return Batch(**{
        field.name: value.to(device) if isinstance(value := getattr(batch, field.name), torch.Tensor) else value
        for field in fields(Batch)
    })


def _read_ahead(source: Iterator[Batch], executor: ThreadPoolExecutor) -> Iterator[Batch]:
    pending = executor.submit(next, source, None)
    try:
        batch = pending.result()
        while batch is not None:
            pending = executor.submit(next, source, None)
            yield batch
            batch = pending.result()
    finally:
        pending.cancel()


def _autocast(config: TrainerConfig):
    if config.precision in ("fp16", "bf16"):
        dtype = torch.float16 if config.precision == "fp16" else torch.bfloat16
        return torch.autocast("cuda", dtype=dtype)
    return nullcontext()


def _sync(device: torch.device) -> None:
    if device.type == "cuda":
        torch.cuda.synchronize(device)


@contextmanager
def _settings(config: TrainerConfig) -> Iterator[torch.device]:
    device = torch.device(config.device)
    if device.type == "cuda":
        if not torch.cuda.is_available():
            raise TrainingError("CUDA is unavailable; no CPU fallback")
        if config.precision == "bf16" and not torch.cuda.is_bf16_supported(including_emulation=False):
            raise TrainingError("CUDA device does not support BF16")
        if config.precision == "tf32" and torch.cuda.get_device_capability()[0] < 8:
            raise TrainingError("CUDA device does not support TF32")
        if config.deterministic and os.environ.get("CUBLAS_WORKSPACE_CONFIG") not in (":16:8", ":4096:8"):
            raise TrainingError("deterministic CUDA requires CUBLAS_WORKSPACE_CONFIG before startup")
    previous = {
        "threads": torch.get_num_threads(),
        "precision": torch.get_float32_matmul_precision(),
        "cudnn_tf32": torch.backends.cudnn.allow_tf32,
        "deterministic": torch.are_deterministic_algorithms_enabled(),
        "rng": capture_rng(cuda=device.type == "cuda"),
    }
    try:
        torch.set_num_threads(config.threads)
        torch.set_float32_matmul_precision("high" if config.precision == "tf32" else "highest")
        torch.backends.cudnn.allow_tf32 = config.precision == "tf32"
        torch.use_deterministic_algorithms(config.deterministic)
        random.seed(config.seed)
        np.random.seed(config.seed % 2**32)
        torch.random.default_generator.manual_seed(config.seed)
        if device.type == "cuda":
            torch.cuda.manual_seed_all(config.seed)
        yield device
    finally:
        restore_rng(previous["rng"], cuda=device.type == "cuda")
        torch.set_num_threads(previous["threads"])
        torch.set_float32_matmul_precision(previous["precision"])
        torch.backends.cudnn.allow_tf32 = previous["cudnn_tf32"]
        torch.use_deterministic_algorithms(previous["deterministic"])


def _datasets(train: str | Path, validation: str | Path, config: TrainerConfig) -> dict:
    train_info = hcpe3_dataset_info(train) if is_hcpe3_manifest(train) else dataset_info(train)
    information = {"train": train_info, "validation": dataset_info(validation)}
    left, right = information.values()
    if (
        left["path"] == right["path"] or left["fingerprint"] == right["fingerprint"]
        or left["manifest"]["dataset_content_sha256"] == right["manifest"]["dataset_content_sha256"]
    ):
        raise TrainingError("train and validation must be distinct datasets")
    for kind, info in information.items():
        manifest = info["manifest"]
        if not config.validation_only and (
            manifest["training_status"] == "validation-only"
            or manifest["teacher_kind"] == "unknown"
            or manifest["split"] in ("unknown", "fixture-only")
        ):
            raise TrainingError(f"{kind} needs explicit teacher provenance/split or --validation-only")
    return information


def _optimizer_updates(optimizer, model) -> int:
    if isinstance(optimizer, TrainerSGD):
        return optimizer.updates
    state = optimizer.state.get(model.value_head[-1].weight, {})
    return int(state["step"].item()) if "step" in state else 0


def _update_progress(optimizer, model, progress) -> None:
    progress["optimizer_updates"] = _optimizer_updates(optimizer, model)
    if isinstance(optimizer, TrainerSGD):
        progress["policy_optimizer_updates"] = optimizer.policy_updates
        return
    policy = optimizer.state.get(model.policy_head.weight, {})
    progress["policy_optimizer_updates"] = int(policy["step"].item()) if "step" in policy else 0


def _runtime() -> dict:
    return {"python": sys.version, "torch": str(torch.__version__), "numpy": str(np.__version__)}


def _validate_resume(payload, config, model_config, datasets) -> dict:
    try:
        expected = {field.name for field in fields(TrainerConfig)}
        optimizer_fields = {"optimizer", "muon_learning_rate"}
        required = expected - optimizer_fields - _OPTIONAL_SETTINGS.keys() - {
            "learning_rate_steps", "warmup_steps", "cosine_end_step", "cosine_min_ratio",
            "compile_ffn", "compile_attention", "fuse_norm_cast", "prefetch_input", "fuse_ffn_residual_norm",
            "fuse_packed_attention", "fuse_packed_attention_backward",
            "fuse_ffn_residual_norm_backward", "tile_packed_attention_backward",
            "fuse_muon_symmetric_polynomial",
            "tile_packed_attention_forward", "fuse_attention_residual_norm", "fuse_final_residual_norm",
            "fuse_packed_qkv_bias_gradient", "fuse_ffn_expand_bias_gradient",
            "tile_packed_attention_short_key_first", "lookup_ffn_gelu_derivative",
        }
        saved_config = payload["trainer_config"]
        if (
            type(saved_config) is not dict or not required <= saved_config.keys() <= expected
            or len(optimizer_fields & saved_config.keys()) == 1
        ):
            raise ResumeError("unexpected saved trainer configuration fields")
        saved = TrainerConfig(**saved_config)
        previous_model = ModelConfig.from_dict(payload["model_config"])
    except (TypeError, ValueError) as error:
        raise ResumeError(f"invalid saved configuration: {error}") from error
    if _identity(saved) != _identity(config) or previous_model != model_config:
        raise ResumeError("training or model configuration differs from checkpoint")
    if payload["datasets"] != datasets:
        raise ResumeError("dataset identity differs from checkpoint")
    if payload["runtime"] != _runtime():
        raise ResumeError("runtime version differs from checkpoint")
    progress = payload["progress"]
    expected = {"step", "optimizer_updates", "policy_optimizer_updates", "epoch", "sample_cursor", "samples_seen"}
    if type(progress) is not dict or set(progress) != expected:
        raise ResumeError("unexpected training progress")
    if any(type(value) is not int or value < 0 for value in progress.values()):
        raise ResumeError("invalid training progress counters")
    count = datasets["train"]["manifest"]["source_records"]
    if count is None:
        cursor_matches = (
            progress["epoch"] <= 1
            and progress["sample_cursor"] == progress["samples_seen"]
            and (progress["sample_cursor"] + config.batch_size - 1) // config.batch_size == progress["step"]
            and (progress["step"] > 0 if progress["epoch"] else progress["sample_cursor"] % config.batch_size == 0)
        )
        if cursor_matches and progress["epoch"]:
            train = datasets["train"]
            with closing(iter_hcpe3_batches(
                train["path"], config.batch_size, start_row=progress["sample_cursor"],
                expected_fingerprint=train["fingerprint"],
            )) as rows:
                cursor_matches = next(rows, None) is None
    else:
        steps_per_epoch = (count + config.batch_size - 1) // config.batch_size
        cursor_matches = (
            progress["epoch"] == progress["step"] // steps_per_epoch
            and progress["sample_cursor"] == (progress["step"] % steps_per_epoch) * config.batch_size
            and progress["samples_seen"] == progress["epoch"] * count + progress["sample_cursor"]
        )
    if (
        progress["step"] > config.max_steps
        or progress["step"] > saved.max_steps
        or progress["optimizer_updates"] > progress["step"]
        or progress["policy_optimizer_updates"] > progress["optimizer_updates"]
        or not cursor_matches
    ):
        raise ResumeError("checkpoint counters do not match the sequential cursor")
    return dict(progress)


def _parameter_update_counts(model, progress):
    policy_parameters = set(model.policy_head.parameters())
    return {
        parameter: progress["policy_optimizer_updates"] if parameter in policy_parameters else progress["optimizer_updates"]
        for parameter in model.parameters()
    }


def _check_optimizer(optimizer, model, progress) -> None:
    if isinstance(optimizer, TrainerSGD):
        optimizer.validate_state(_parameter_update_counts(model, progress))
        return
    if isinstance(optimizer, MuonAdamW):
        optimizer.validate_muon(progress["optimizer_updates"])
        parameters = set(optimizer.adamw_parameters)
    else:
        parameters = set(model.parameters())
    nadam = isinstance(optimizer, torch.optim.NAdam)
    name = "NAdam" if nadam else "AdamW"
    policy_parameters = set(model.policy_head.parameters())
    if not set(optimizer.state).issubset(parameters):
        raise ResumeError(f"unexpected {name} parameter state")
    policy_presence = [parameter in optimizer.state for parameter in policy_parameters]
    if any(policy_presence) and not all(policy_presence):
        raise ResumeError(f"policy head {name} state is incomplete")
    update_counts = _parameter_update_counts(model, progress)
    for parameter in parameters:
        updates = update_counts[parameter]
        state = optimizer.state.get(parameter)
        if state is None:
            if updates:
                raise ResumeError(f"missing {name} state for updated parameter")
            continue
        if nadam:
            validate_nadam_state(state, parameter, updates, optimizer.param_groups[0]["betas"][0])
        else:
            validate_adamw_state(state, parameter, updates)


def _learning_rate_for_checkpoint(config: TrainerConfig, completed_steps: int) -> float:
    if completed_steps <= 0:
        return config.learning_rate
    if config.warmup_steps and completed_steps <= config.warmup_steps:
        return config.learning_rate * (completed_steps / config.warmup_steps)
    if config.cosine_end_step:
        fraction = min(1.0, (completed_steps - config.warmup_steps) / (config.cosine_end_step - config.warmup_steps))
        blend = 0.5 * (1.0 + math.cos(math.pi * fraction))
        return config.learning_rate * (config.cosine_min_ratio + (1.0 - config.cosine_min_ratio) * blend)
    for step, rate in reversed(config.learning_rate_steps):
        if step < completed_steps:
            return rate
    return config.learning_rate


def _set_learning_rate(optimizer, config: TrainerConfig, rate: float) -> None:
    if isinstance(optimizer, MuonAdamW):
        optimizer.set_learning_rates(rate, config.muon_learning_rate * (rate / config.learning_rate))
    else:
        for group in optimizer.param_groups:
            group["lr"] = rate


def _parse_learning_rate_step(argument: str) -> tuple[int, float]:
    try:
        step, rate = argument.split(":")
        return int(step), float(rate)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected STEP:RATE") from error


def _checkpoint(path, model, optimizer, scaler, config, datasets, progress, average) -> None:
    scaler_state = scaler.state_dict()
    validate_scaler_state(scaler_state, enabled=config.precision == "fp16")
    payload = {
        **CONTRACT,
        "model_config": model.config.to_dict(), "trainer_config": _config_dict(config),
        "datasets": datasets, "model_state": model.state_dict(),
        "optimizer_state": optimizer.state_dict(), "scaler_state": scaler_state,
        "scheduler_state": None, "progress": dict(progress),
        "rng": capture_rng(cuda=config.device == "cuda"), "runtime": _runtime(),
    }
    if average is not None:
        payload["averaging_state"] = average.state_dict()
    atomic_save(path, payload)


def evaluate(model, data_path, config: TrainerConfig, device: torch.device) -> dict:
    """held-out の policy 行数と全行数を分けて CE / BCE / Brier を集計する。"""
    previous_mode = model.training
    model.eval()
    totals = torch.zeros(3, dtype=torch.float64, device=device)
    rows = 0
    policy_rows = 0
    started = time.perf_counter()
    batches = iter_batches(data_path, config.batch_size)
    if config.validation_batches:
        batches = islice(batches, config.validation_batches)
    try:
        with torch.no_grad():
            for batch in batches:
                count = batch.tokens.shape[0]
                policies = int(batch.policy_present.sum().item())
                batch = _transfer(batch, device)
                with _autocast(config):
                    policy, value = model(batch.tokens)
                    loss = policy_value_loss(policy, value, batch)
                brier = (torch.sigmoid(value.float()).squeeze(1) - batch.value).square().sum()
                totals += torch.stack((
                    loss.policy.double() * policies, loss.value.double() * count, brier.double()
                ))
                rows += count
                policy_rows += policies
        _sync(device)
        values = totals.cpu().tolist()
        if not rows or not all(math.isfinite(value) for value in values):
            raise TrainingError("held-out evaluation is empty or non-finite")
        return {
            "rows": rows, "policy_rows": policy_rows,
            "policy_ce": values[0] / policy_rows if policy_rows else None,
            "value_bce": values[1] / rows, "value_brier": values[2] / rows,
            "seconds": time.perf_counter() - started,
        }
    finally:
        model.train(previous_mode)


def _write_json(path: Path, value: dict) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    with temporary.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True, allow_nan=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    if path.exists():
        raise TrainingError("refusing to overwrite JSON artifact")
    temporary.rename(path)
    sync_directory(path.parent)


def run_training(
    train_path: str | Path,
    validation_path: str | Path,
    run_dir: str | Path,
    model_config: ModelConfig,
    config: TrainerConfig,
    *,
    resume: str | Path | None = None,
) -> dict:
    """有限回だけ更新し、再開状態・v1 export checkpoint・held-out report を新規 run に保存する。"""
    if config.fuse_packed_qkv_bias_gradient and (
        model_config.width != 256 or config.batch_size != 256 or model_config.layers < 2
    ):
        raise TrainingError("fuse_packed_qkv_bias_gradient requires width 256, batch size 256 and at least two layers")
    if config.fuse_ffn_expand_bias_gradient:
        if model_config.width != 256 or model_config.ffn_width != 1024 or config.batch_size != 256:
            raise TrainingError("fuse_ffn_expand_bias_gradient requires width 256, FFN width 1024 and batch size 256")
        if torch.version.cuda is None:
            raise TrainingError("fuse_ffn_expand_bias_gradient requires NVIDIA CUDA")
    if config.fuse_ffn_residual_norm:
        if model_config.width not in (128, 192, 224, 256) or not model_config.packed_qkv:
            raise TrainingError("fuse_ffn_residual_norm requires width 128, 192, 224 or 256 and packed QKV")
        if torch.version.cuda is None:
            raise TrainingError("fuse_ffn_residual_norm requires NVIDIA CUDA")
    if config.fuse_attention_residual_norm:
        if model_config.width not in (128, 192, 224, 256):
            raise TrainingError("fuse_attention_residual_norm requires width 128, 192, 224 or 256")
        if torch.version.cuda is None:
            raise TrainingError("fuse_attention_residual_norm requires NVIDIA CUDA")
    if config.fuse_final_residual_norm:
        if model_config.width not in (128, 192, 224, 256):
            raise TrainingError("fuse_final_residual_norm requires width 128, 192, 224 or 256")
        if torch.version.cuda is None:
            raise TrainingError("fuse_final_residual_norm requires NVIDIA CUDA")
    if config.fuse_packed_attention:
        if not model_config.packed_qkv or model_config.width != 32 * model_config.heads:
            raise TrainingError("fuse_packed_attention requires packed QKV with a head width of 32")
        if torch.version.cuda is None:
            raise TrainingError("fuse_packed_attention requires NVIDIA CUDA")
    output = resolve_new_artifact(run_dir)
    if not output.is_relative_to(shogi_data_root() / "runs" / "rshogi-dl"):
        raise ArtifactPathError("training runs must be below $SHOGI_DATA/runs/rshogi-dl")
    setup_started = time.perf_counter()
    datasets = _datasets(train_path, validation_path, config)
    payload = load_resume(resume) if resume is not None else None
    progress = _validate_resume(payload, config, model_config, datasets) if payload else {
        "step": 0, "optimizer_updates": 0, "policy_optimizer_updates": 0,
        "epoch": 0, "sample_cursor": 0, "samples_seen": 0,
    }
    initial_rate = _learning_rate_for_checkpoint(config, progress["step"])
    if payload:
        validate_scaler_state(payload["scaler_state"], enabled=config.precision == "fp16")
    initial_samples = progress["samples_seen"]
    initial_step = progress["step"]
    initial_updates = progress["optimizer_updates"]
    with _settings(config) as device:
        if config.fuse_ffn_expand_bias_gradient and torch.cuda.get_device_capability(device)[0] < 8:
            raise TrainingError("fuse_ffn_expand_bias_gradient requires NVIDIA CUDA compute capability 8.0 or later")
        if config.fuse_packed_attention_backward:
            properties = torch.cuda.get_device_properties(device)
            if properties.major < 8 or properties.shared_memory_per_block_optin < 64 * 1024:
                raise TrainingError("fuse_packed_attention_backward requires Ampere or newer and 64 KiB shared memory per block")
        if config.fuse_muon_symmetric_polynomial and (
            torch.version.hip is not None or torch.cuda.get_device_capability(device)[0] < 8
        ):
            raise TrainingError("fuse_muon_symmetric_polynomial requires NVIDIA CUDA compute capability 8.0 or later")
        uses_step_graph = config.execution in _STEP_GRAPH_EXECUTIONS
        model = SquareTransformer(model_config, attention_backend=config.attention_backend).to(device)
        average = None
        if config.weight_average is not None:
            average = WeightAverage(
                model, start_step=config.weight_average_start_step, every=config.weight_average_every,
                decay=config.ema_decay if config.weight_average == "ema" else None,
            )
        if config.optimizer == "adamw":
            optimizer = torch.optim.AdamW(
                model.parameters(), lr=config.learning_rate, weight_decay=config.weight_decay,
                betas=config.adamw_betas, eps=config.adamw_epsilon, fused=config.fused_optimizer,
                capturable=uses_step_graph,
            )
        elif config.optimizer == "nadam":
            optimizer = torch.optim.NAdam(
                model.parameters(), lr=config.learning_rate, weight_decay=config.weight_decay,
                betas=config.adamw_betas, eps=config.adamw_epsilon, momentum_decay=NADAM_MOMENTUM_DECAY,
                decoupled_weight_decay=True, foreach=False,
            )
        elif config.optimizer == "sgd":
            optimizer = TrainerSGD(
                model, lr=config.learning_rate, weight_decay=config.weight_decay,
                momentum=config.sgd_momentum, nesterov=config.sgd_nesterov,
            )
        else:
            optimizer = MuonAdamW(
                model, lr=config.learning_rate, muon_lr=config.muon_learning_rate,
                weight_decay=config.weight_decay, capturable=uses_step_graph,
                adamw_betas=config.adamw_betas, adamw_epsilon=config.adamw_epsilon, batched=config.optimizer == "batched-muon",
                fuse_symmetric_polynomial=config.fuse_muon_symmetric_polynomial,
            )
        scaler = torch.amp.GradScaler(
            "cuda", enabled=config.precision == "fp16",
            growth_factor=SCALER_GROWTH_FACTOR,
            backoff_factor=SCALER_BACKOFF_FACTOR,
            growth_interval=SCALER_GROWTH_INTERVAL,
        )
        if payload:
            try:
                model.load_state_dict(payload["model_state"], strict=True)
                if ("averaging_state" in payload) != (average is not None):
                    raise ResumeError("weight average state differs from trainer configuration")
                if average is not None:
                    average.load_state_dict(payload["averaging_state"], progress["step"])
                saved_optimizer = payload["optimizer_state"]
                if config.learning_rate_steps or config.warmup_steps or config.cosine_end_step:
                    _set_learning_rate(optimizer, config, initial_rate)
                if config.optimizer == "nadam":
                    load_nadam_state(optimizer, saved_optimizer, _parameter_update_counts(model, progress))
                elif config.optimizer == "sgd":
                    optimizer.load_training_state(saved_optimizer, _parameter_update_counts(model, progress))
                else:
                    if config.optimizer == "adamw":
                        validate_optimizer_groups(saved_optimizer, optimizer, "AdamW")
                    optimizer.load_state_dict(saved_optimizer)
                scaler.load_state_dict(payload["scaler_state"])
                _check_optimizer(optimizer, model, progress)
                restore_rng(payload["rng"], cuda=device.type == "cuda")
            except (TypeError, ValueError, RuntimeError, KeyError) as error:
                raise ResumeError(f"invalid training state: {error}") from error
        continuous_rate = bool(config.warmup_steps or config.cosine_end_step)
        rate_tensor = None
        if continuous_rate and uses_step_graph:
            rate_tensor = torch.tensor(initial_rate, dtype=torch.float32, device=device)
            optimizer.param_groups[0]["lr"] = rate_tensor

            def serialize_rate(_optimizer, state):
                state["param_groups"][0]["lr"] = _learning_rate_for_checkpoint(config, progress["step"])
                return state

            optimizer.register_state_dict_post_hook(serialize_rate)
        if any(not bool(torch.isfinite(parameter).all()) for parameter in model.parameters()):
            raise TrainingError("model parameters are non-finite")
        if config.execution == "compile":
            execution = torch.compile(model, backend="inductor", mode=config.compile_mode)
        elif config.compile_ffn or config.compile_attention or config.fuse_packed_attention:
            execution = tuned_training_forward(
                model, config.batch_size, fused_norm=config.execution == "cuda-graph-fused-norm",
                cast_norm=config.fuse_norm_cast, compile_ffn=config.compile_ffn, compile_attention=config.compile_attention,
                fuse_ffn_residual_norm=config.fuse_ffn_residual_norm,
                fuse_ffn_residual_norm_backward=config.fuse_ffn_residual_norm_backward,
                fuse_packed_attention=config.fuse_packed_attention,
                fuse_packed_attention_backward=config.fuse_packed_attention_backward,
                tile_packed_attention_backward=config.tile_packed_attention_backward,
                tile_packed_attention_forward=config.tile_packed_attention_forward,
                fuse_attention_residual_norm=config.fuse_attention_residual_norm,
                fuse_final_residual_norm=config.fuse_final_residual_norm,
                fuse_packed_qkv_bias_gradient=config.fuse_packed_qkv_bias_gradient,
                fuse_ffn_expand_bias_gradient=config.fuse_ffn_expand_bias_gradient,
                tile_packed_attention_short_key_first=config.tile_packed_attention_short_key_first,
                lookup_ffn_gelu_derivative=config.lookup_ffn_gelu_derivative,
            )
        elif config.execution == "cuda-graph-fused-norm":
            execution = compiled_residual_forward(
                model, cast_output=config.fuse_norm_cast, fuse_ffn_residual_norm=config.fuse_ffn_residual_norm,
                fuse_ffn_residual_norm_backward=config.fuse_ffn_residual_norm_backward,
                fuse_attention_residual_norm=config.fuse_attention_residual_norm,
                fuse_final_residual_norm=config.fuse_final_residual_norm,
            )
        else:
            execution = model
        output.parent.mkdir(parents=True, exist_ok=True)
        output.mkdir()
        parent = output.parent
        data_root = shogi_data_root()
        while parent.is_relative_to(data_root):
            sync_directory(parent)
            if parent == data_root:
                break
            parent = parent.parent
        _write_json(output / "run.json", {
            "model_config": model_config.to_dict(), "trainer_config": _config_dict(config),
            "datasets": datasets, "resume": str(Path(resume).resolve()) if resume else None,
            "training_status": TRAINING_STATUS,
        })
        setup_seconds = time.perf_counter() - setup_started
        model.train()
        train = datasets["train"]
        count = train["manifest"]["source_records"]
        stream = count is None
        source = None
        rolling = torch.zeros(3, device=device)
        rolling_steps = 0
        finite = torch.ones((), dtype=torch.bool, device=device)

        weighted_loss = (config.policy_loss_weight, config.value_loss_weight) != (1.0, 1.0)
        parameters = tuple(model.parameters())

        def update(batch: Batch, has_policy: bool) -> None:
            optimizer.zero_grad(set_to_none=True)
            with _autocast(config):
                policy, value = execution(batch.tokens)
                loss = policy_value_loss(policy, value, batch)
                total = (config.policy_loss_weight * loss.policy + config.value_loss_weight * loss.value
                         if weighted_loss else loss.total)
            finite.logical_and_(torch.isfinite(total.detach()))
            scaler.scale(total).backward()
            clear_unused_policy_head_gradients(model, has_policy and config.policy_loss_weight > 0)
            if config.clip_grad_norm:
                scaler.unscale_(optimizer)
                torch.nn.utils.clip_grad_norm_(parameters, config.clip_grad_norm)
            scaler.step(optimizer)
            scaler.update()
            rolling.add_(torch.stack((total.detach(), loss.policy.detach(), loss.value.detach())))

        def make_graph():
            return StepGraph(
                config.batch_size, device, update, _transfer, parameters,
                prefetch_input=config.prefetch_input,
            ) if uses_step_graph else None

        graph = make_graph()
        graph_replays = 0
        active_rate = initial_rate
        pending_rates = iter(pair for pair in config.learning_rate_steps if pair[0] >= progress["step"])
        next_rate = next(pending_rates, None)
        _sync(device)
        started = time.perf_counter()
        reader = ThreadPoolExecutor(max_workers=1) if config.prefetch_input else nullcontext()
        direct_source = (
            iter_hcpe3_batches(
                train["path"], config.batch_size, start_row=progress["sample_cursor"],
                expected_fingerprint=train["fingerprint"],
            )
            if stream and not progress["epoch"] else None
        )
        with (
            closing(direct_source) if direct_source is not None else nullcontext(),
            (output / "metrics.jsonl").open("x", encoding="utf-8") as metrics,
            reader as executor,
        ):
            def write_metrics() -> None:
                values = (rolling / rolling_steps).cpu().tolist()
                event = {
                    **progress, "amp_skipped_steps": progress["step"] - progress["optimizer_updates"],
                    "loss": values[0], "policy_loss": values[1], "value_loss": values[2],
                    "seconds": time.perf_counter() - started,
                }
                if config.learning_rate_steps or continuous_rate:
                    event["learning_rate"] = active_rate
                metrics.write(json.dumps(event, sort_keys=True, allow_nan=False) + "\n")
                metrics.flush()
                rolling.zero_()

            while progress["step"] < config.max_steps:
                if stream and progress["epoch"]:
                    break
                if source is None:
                    source = direct_source if stream else iter_batches(
                        train["path"], config.batch_size, start_record=progress["sample_cursor"],
                    )
                    if executor is not None:
                        source = _read_ahead(islice(source, config.max_steps - progress["step"]), executor)
                try:
                    batch = next(source)
                except StopIteration:
                    if not stream:
                        raise TrainingError("sharded dataset ended before its declared row count") from None
                    if not progress["samples_seen"]:
                        raise TrainingError("HCPE3 manifest has no accepted training rows") from None
                    progress["epoch"] = 1
                    if rolling_steps:
                        _update_progress(optimizer, model, progress)
                        if not bool(finite.item()):
                            raise TrainingError("non-finite training loss")
                        write_metrics()
                    break
                samples = batch.tokens.shape[0]
                if config.cosine_end_step or progress["step"] < config.warmup_steps:
                    rate = _learning_rate_for_checkpoint(config, progress["step"] + 1)
                    if rate != active_rate:
                        active_rate = rate
                        if rate_tensor is None:
                            _set_learning_rate(optimizer, config, rate)
                        else:
                            rate_tensor.fill_(rate)
                elif next_rate is not None and progress["step"] == next_rate[0]:
                    active_rate = next_rate[1]
                    next_rate = next(pending_rates, None)
                    if rate_tensor is not None:
                        rate_tensor.fill_(active_rate)
                    else:
                        _set_learning_rate(optimizer, config, active_rate)
                    if graph is not None and rate_tensor is None:
                        # AdamW と Muon の scalar 学習率は capture 時の値が埋め込まれる。
                        _sync(device)
                        optimizer.zero_grad(set_to_none=True)
                        graph_replays += graph.replays
                        del graph
                        graph = make_graph()
                if graph is None or not graph.try_update(batch):
                    has_policy = has_policy_targets(batch)
                    batch = _transfer(batch, device)
                    if config.execution == "compile" and config.compile_mode in _CUDAGRAPH_MODES:
                        torch.compiler.cudagraph_mark_step_begin()
                    update(batch, has_policy)
                rolling_steps += 1
                progress["step"] += 1
                if average is not None:
                    average.sample(progress["step"])
                progress["sample_cursor"] += samples
                progress["samples_seen"] += samples
                if progress["sample_cursor"] == count:
                    progress["epoch"] += 1
                    progress["sample_cursor"] = 0
                    source = None
                if stream and samples < config.batch_size:
                    progress["epoch"] = 1
                log = progress["step"] % config.log_every == 0 or progress["step"] == config.max_steps or (
                    stream and progress["epoch"]
                )
                checkpoint = (
                    config.checkpoint_every
                    and progress["step"] % config.checkpoint_every == 0
                    and progress["step"] < config.max_steps
                )
                if log or checkpoint:
                    _update_progress(optimizer, model, progress)
                    if not bool(finite.item()):
                        raise TrainingError("non-finite training loss")
                if log:
                    write_metrics()
                    rolling_steps = 0
                if checkpoint:
                    if any(not bool(torch.isfinite(parameter).all()) for parameter in model.parameters()):
                        raise TrainingError("non-finite model parameters at checkpoint")
                    _check_optimizer(optimizer, model, progress)
                    _checkpoint(
                        output / f"resume-{progress['step']:010d}.pt",
                        model, optimizer, scaler, config, datasets, progress, average,
                    )
        _sync(device)
        training_seconds = time.perf_counter() - started
        if graph is not None:
            graph_replays += graph.replays
            optimizer.zero_grad(set_to_none=True)
            del graph
        _update_progress(optimizer, model, progress)
        if progress["step"] > initial_step and progress["optimizer_updates"] == initial_updates:
            raise TrainingError("no optimizer update completed in this segment")
        if not bool(finite.item()) or any(
            not bool(torch.isfinite(parameter).all()) for parameter in model.parameters()
        ):
            raise TrainingError("final loss or model parameters are non-finite")
        _check_optimizer(optimizer, model, progress)
        _checkpoint(output / "resume-final.pt", model, optimizer, scaler, config, datasets, progress, average)
        export_checkpoint(output / "checkpoint.pt", model, optimizer, step=progress["step"], seed=config.seed)
        validation = evaluate(model, datasets["validation"]["path"], config, device)
        segment_samples = progress["samples_seen"] - initial_samples
        report = {
            "training_status": TRAINING_STATUS, "dataset_validation_only": config.validation_only,
            "cudagraphs_requested": (
                uses_step_graph
                or (config.execution == "compile" and config.compile_mode in _CUDAGRAPH_MODES)
            ),
            "trainer_config": _config_dict(config), "model_config": model_config.to_dict(),
            "datasets": datasets, "progress": progress,
            "amp_skipped_steps": progress["step"] - progress["optimizer_updates"],
            "setup_seconds": setup_seconds, "training_seconds": training_seconds,
            "segment_samples": segment_samples,
            "samples_per_second": segment_samples / training_seconds,
            "validation": validation, "resume_checkpoint": "resume-final.pt",
            "export_checkpoint": "checkpoint.pt",
        }
        if uses_step_graph:
            report["cuda_graph_replayed_steps"] = graph_replays
        if average is not None:
            report["weight_average_samples"] = average.samples
            if average.samples:
                model.load_state_dict(average.weights)
                export_checkpoint(output / "checkpoint-averaged.pt", model, None, step=average.last_step,
                                  seed=config.seed)
                report["averaged_checkpoint"] = "checkpoint-averaged.pt"
                report["averaged_validation"] = evaluate(model, datasets["validation"]["path"], config, device)
        _write_json(output / "report.json", report)
        return report


def main(argv: list[str] | None = None) -> int:
    """有限学習と再開の CLI を実行し、条件違反は終了 code 2 で返す。"""
    parser = argparse.ArgumentParser(description="教師データの有限学習・held-out評価・再開。")
    parser.add_argument("--train", required=True, help="sharded dataset directory または HCPE3 manifest file（1 pass）")
    parser.add_argument("--validation", required=True)
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--resume")
    parser.add_argument("--config", choices=sorted(NAMED_CONFIGS), default="initial")
    parser.add_argument("--model-config", help="layers/width/heads/ffn_width と任意の packed_qkv を含む JSON object")
    parser.add_argument("--max-steps", type=int, required=True)
    parser.add_argument("--warmup-steps", type=int, default=0)
    parser.add_argument("--cosine-end-step", type=int, default=0)
    parser.add_argument("--cosine-min-ratio", type=float, default=0.1)
    parser.add_argument("--batch-size", type=int, default=128)
    parser.add_argument("--learning-rate", type=float, default=1e-3)
    parser.add_argument("--learning-rate-step", dest="learning_rate_steps", action="append",
                        default=[], type=_parse_learning_rate_step, metavar="STEP:RATE",
                        help="STEP 回の更新試行後に RATE に下げる。複数指定可")
    parser.add_argument("--weight-decay", type=float, default=0.0)
    parser.add_argument("--adamw-betas", "--betas", type=float, nargs=2, default=(0.9, 0.999), metavar=("BETA1", "BETA2"))
    parser.add_argument("--adamw-epsilon", "--epsilon", type=float, default=1e-8)
    parser.add_argument("--sgd-momentum", type=float, default=0.0)
    parser.add_argument("--sgd-nesterov", action="store_true")
    parser.add_argument("--clip-grad-norm", type=float, default=0.0)
    parser.add_argument("--policy-loss-weight", type=float, default=1.0)
    parser.add_argument("--value-loss-weight", type=float, default=1.0)
    parser.add_argument("--weight-average", choices=("swa", "ema"))
    parser.add_argument("--weight-average-start-step", type=int, default=1)
    parser.add_argument("--weight-average-every", type=int, default=1)
    parser.add_argument("--ema-decay", type=float, default=0.999)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--threads", type=int, default=1)
    parser.add_argument("--device", choices=("cpu", "cuda"), default="cpu")
    parser.add_argument("--attention-backend", choices=ATTENTION_BACKENDS, default="explicit")
    parser.add_argument("--execution", choices=_EXECUTIONS, default="eager")
    parser.add_argument("--compile-mode", choices=COMPILE_MODES, default="default")
    parser.add_argument("--compile-ffn", action="store_true")
    parser.add_argument("--fuse-ffn-expand-bias-gradient", action="store_true")
    parser.add_argument("--lookup-ffn-gelu-derivative", action="store_true")
    parser.add_argument("--compile-attention", action="store_true")
    parser.add_argument("--fuse-norm-cast", action="store_true")
    parser.add_argument("--fuse-ffn-residual-norm", action="store_true")
    parser.add_argument("--fuse-ffn-residual-norm-backward", action="store_true")
    parser.add_argument("--fuse-attention-residual-norm", action="store_true")
    parser.add_argument("--fuse-final-residual-norm", action="store_true")
    parser.add_argument("--fuse-packed-attention", action="store_true")
    parser.add_argument("--fuse-packed-attention-backward", action="store_true")
    parser.add_argument("--tile-packed-attention-backward", action="store_true")
    parser.add_argument("--tile-packed-attention-short-key-first", action="store_true")
    parser.add_argument("--tile-packed-attention-forward", action="store_true")
    parser.add_argument("--fuse-packed-qkv-bias-gradient", action="store_true")
    parser.add_argument("--prefetch-input", action="store_true")
    parser.add_argument("--precision", choices=("fp32", "tf32", "fp16", "bf16"), default="fp32")
    parser.add_argument("--fused-optimizer", action="store_true")
    parser.add_argument("--optimizer", choices=_OPTIMIZERS, default="adamw")
    parser.add_argument("--muon-learning-rate", type=float)
    parser.add_argument("--fuse-muon-symmetric-polynomial", action="store_true")
    parser.add_argument("--deterministic", action="store_true")
    parser.add_argument("--validation-only", action="store_true")
    parser.add_argument("--validation-batches", type=int, default=32, help="0 で全 held-out dataset")
    parser.add_argument("--checkpoint-every", type=int, default=0)
    parser.add_argument("--log-every", type=int, default=100)
    args = parser.parse_args(argv)
    try:
        options = {field.name: getattr(args, field.name) for field in fields(TrainerConfig)}
        config = TrainerConfig(**options)
        model_config = (
            ModelConfig.from_dict(json.loads(args.model_config))
            if args.model_config else NAMED_CONFIGS[args.config]
        )
        report = run_training(args.train, args.validation, args.run_dir, model_config, config, resume=args.resume)
        print(json.dumps(report, sort_keys=True, allow_nan=False))
    except (ArtifactPathError, DatasetError, ResumeError, TrainingError, ValueError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
