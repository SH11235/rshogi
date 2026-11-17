"""有限回の学習更新を計測し、実行方式とデータ供給の費用を比較する。"""

from __future__ import annotations

import argparse
from contextlib import contextmanager, nullcontext
from dataclasses import asdict, dataclass, fields, replace
import hashlib
from itertools import islice
import json
import math
from pathlib import Path
import sys
import time
from collections.abc import Iterator

import torch

from .contract import TRAINING_STATUS
from .data import Batch, RecordError, iter_batches
from .loss import policy_value_loss
from .model import ATTENTION_BACKENDS, NAMED_CONFIGS, ModelConfig, SquareTransformer
from .paths import ArtifactPathError, resolve_new_artifact
from ._policy_grad import clear_unused_policy_head_gradients, has_policy_targets


COMPILE_MODES = ("default", "reduce-overhead", "max-autotune", "max-autotune-no-cudagraphs")
_CUDAGRAPH_MODES = ("reduce-overhead", "max-autotune")


class BenchmarkError(RuntimeError):
    """計測条件または学習更新が不正。"""


@dataclass(frozen=True)
class BenchmarkConfig:
    """上限のある有限計測の条件。"""

    batch_size: int = 32
    steps: int = 20
    warmup: int = 5
    window_batches: int = 4
    seed: int = 0
    threads: int = 1
    device: str = "cpu"
    attention_backend: str = "explicit"
    execution: str = "eager"
    compile_mode: str = "default"
    precision: str = "fp32"
    input_mode: str = "resident"
    policy_rows_mode: str = "prepared"
    fused_optimizer: bool = False
    learning_rate: float = 1e-3
    weight_decay: float = 0.0

    def __post_init__(self) -> None:
        limits = {
            "batch_size": (1, 4096), "steps": (1, 10000), "warmup": (0, 10000),
            "window_batches": (1, 32), "seed": (0, 2**63 - 1), "threads": (1, 256),
        }
        for name, (low, high) in limits.items():
            value = getattr(self, name)
            if type(value) is not int or not low <= value <= high:
                raise ValueError(f"{name} must be an integer in {low}..{high}")
        for name, choices in {
            "attention_backend": ATTENTION_BACKENDS,
            "execution": ("eager", "compile"), "compile_mode": COMPILE_MODES,
            "precision": ("fp32", "tf32", "fp16", "bf16"),
            "input_mode": ("resident", "stream"), "policy_rows_mode": ("prepared", "legacy"),
        }.items():
            if getattr(self, name) not in choices:
                raise ValueError(f"{name} must be one of {choices}")
        if type(self.device) is not str or self.device not in ("cpu", "cuda"):
            raise ValueError("device must be cpu or cuda")
        if self.execution == "eager" and self.compile_mode != "default":
            raise ValueError("non-default compile_mode requires execution=compile")
        if self.device == "cpu" and self.compile_mode in _CUDAGRAPH_MODES:
            raise ValueError("this compile_mode requires device=cuda")
        if type(self.fused_optimizer) is not bool:
            raise ValueError("fused_optimizer must be boolean")
        if self.device == "cpu" and (self.precision != "fp32" or self.fused_optimizer):
            raise ValueError("CPU benchmark requires fp32 and an unfused optimizer")
        for name, positive in (("learning_rate", True), ("weight_decay", False)):
            value = getattr(self, name)
            if type(value) not in (int, float) or not math.isfinite(value):
                raise ValueError(f"{name} must be finite")
            if value < 0 or (positive and value == 0):
                raise ValueError(f"{name} must be {'positive' if positive else 'non-negative'}")


def _batches(path: str | Path, config: BenchmarkConfig) -> Iterator[Batch]:
    while True:
        produced = False
        for batch in islice(
            iter_batches(path, config.batch_size, drop_last=True), config.window_batches
        ):
            produced = True
            yield batch
        if not produced:
            raise BenchmarkError(f"data needs at least {config.batch_size} records")


def _to_device(
    batch: Batch, device: torch.device, *, policy_rows_mode: str = "prepared"
) -> Batch:
    if policy_rows_mode == "legacy":
        batch = replace(batch, policy_rows=None)
    values = {field.name: getattr(batch, field.name) for field in fields(Batch)}
    return Batch(**{name: tensor.to(device) if tensor is not None else None
                    for name, tensor in values.items()})


def _digest(model: SquareTransformer) -> str:
    digest = hashlib.sha256()
    for name, parameter in model.state_dict().items():
        digest.update(name.encode())
        digest.update(parameter.detach().cpu().contiguous().numpy().tobytes())
    return digest.hexdigest()


def _synchronize(device: torch.device) -> None:
    if device.type == "cuda":
        torch.cuda.synchronize(device)


@contextmanager
def _settings(config: BenchmarkConfig) -> Iterator[torch.device]:
    previous_threads = torch.get_num_threads()
    previous_precision = torch.get_float32_matmul_precision()
    previous_cudnn_tf32 = torch.backends.cudnn.allow_tf32
    random_state = torch.random.get_rng_state()
    device = torch.device(config.device)
    if device.type == "cuda":
        if not torch.cuda.is_available():
            raise BenchmarkError("CUDA is not available; no CPU fallback")
        if config.precision == "bf16" and not torch.cuda.is_bf16_supported(including_emulation=False):
            raise BenchmarkError("this CUDA device does not support BF16")
        if config.precision == "tf32" and torch.cuda.get_device_capability(device)[0] < 8:
            raise BenchmarkError("this CUDA device does not support TF32")
    try:
        torch.set_num_threads(config.threads)
        torch.set_float32_matmul_precision("high" if config.precision == "tf32" else "highest")
        torch.backends.cudnn.allow_tf32 = config.precision == "tf32"
        torch.random.default_generator.manual_seed(config.seed)
        yield device
    finally:
        torch.random.set_rng_state(random_state)
        torch.set_num_threads(previous_threads)
        torch.set_float32_matmul_precision(previous_precision)
        torch.backends.cudnn.allow_tf32 = previous_cudnn_tf32


def _autocast(config: BenchmarkConfig):
    if config.precision in ("fp16", "bf16"):
        dtype = torch.float16 if config.precision == "fp16" else torch.bfloat16
        return torch.autocast("cuda", dtype=dtype)
    return nullcontext()


def _optimizer_steps(optimizer: torch.optim.Optimizer, parameter: torch.nn.Parameter) -> int:
    state = optimizer.state.get(parameter, {})
    return int(state["step"].item()) if "step" in state else 0


def run_benchmark(
    data_path: str | Path, model_config: ModelConfig, config: BenchmarkConfig
) -> dict:
    """同一 seed の初期重みから更新し、JSON 化できる診断と時間を返す。

    resident は先頭の有限 window を device に置く。stream は同じ window を再読込・転送する。
    CUDA event の経過時間にも CPU の投入待ちが含まれ、kernel 実行時間だけを表さない。
    """
    with _settings(config) as device:
        startup_started = time.perf_counter()
        model = SquareTransformer(model_config, attention_backend=config.attention_backend).to(device)
        model.train()
        initial_digest = _digest(model)
        optimizer = torch.optim.AdamW(
            model.parameters(), lr=config.learning_rate, weight_decay=config.weight_decay,
            fused=config.fused_optimizer,
        )
        scaler = torch.amp.GradScaler("cuda", enabled=config.precision == "fp16")
        batches = _batches(data_path, config)
        window = list(islice(
            iter_batches(data_path, config.batch_size, drop_last=True), config.window_batches
        ))
        if not window:
            raise BenchmarkError(f"data needs at least {config.batch_size} records")
        data_digest = hashlib.sha256()
        for batch in window:
            for field in fields(Batch):
                # 補助 index の有無を変えても、比較する元の 6 Tensor の同一性を判定する。
                if field.name == "policy_rows":
                    continue
                tensor = getattr(batch, field.name)
                data_digest.update(field.name.encode())
                data_digest.update(str(tuple(tensor.shape)).encode())
                data_digest.update(tensor.numpy().tobytes())
        window_count = len(window)
        def prepare_batch(batch: Batch) -> tuple[Batch, bool]:
            has_policy = has_policy_targets(batch)
            return _to_device(batch, device, policy_rows_mode=config.policy_rows_mode), has_policy

        resident = [prepare_batch(batch) for batch in window] if config.input_mode == "resident" else None
        del window
        batch_index = 0

        def next_batch() -> tuple[Batch, bool]:
            nonlocal batch_index
            if resident is None:
                return prepare_batch(next(batches))
            batch = resident[batch_index % len(resident)]
            batch_index += 1
            return batch

        diagnostic_batch, _ = next_batch()
        with torch.no_grad(), _autocast(config):
            policy, value = model(diagnostic_batch.tokens)
            initial_loss = policy_value_loss(policy, value, diagnostic_batch).total.item()
        if not math.isfinite(initial_loss):
            raise BenchmarkError("initial loss is not finite")
        # 診断で消費した先頭 batch は、計測する両入力方式でも同じ順序に戻す。
        batches = _batches(data_path, config)
        batch_index = 0
        forward = (
            torch.compile(model, mode=config.compile_mode) if config.execution == "compile" else model
        )
        cudagraphs_requested = (
            config.execution == "compile" and config.compile_mode in _CUDAGRAPH_MODES
        )
        _synchronize(device)
        startup_seconds = time.perf_counter() - startup_started

        def step(prepared: tuple[Batch, bool]) -> torch.Tensor:
            batch, has_policy = prepared
            if cudagraphs_requested:
                # compiled forward/backward の前の反復で使った一時 Tensor の寿命を区切る。
                torch.compiler.cudagraph_mark_step_begin()
            optimizer.zero_grad(set_to_none=True)
            with _autocast(config):
                policy, value = forward(batch.tokens)
                loss = policy_value_loss(policy, value, batch).total
            scaler.scale(loss).backward()
            clear_unused_policy_head_gradients(model, has_policy)
            scaler.step(optimizer)
            scaler.update()
            return loss.detach()

        warmup_started = time.perf_counter()
        for _ in range(config.warmup):
            step(next_batch())
        _synchronize(device)
        warmup_seconds = time.perf_counter() - warmup_started
        updates_before = _optimizer_steps(optimizer, model.position_embedding)
        losses = torch.empty(config.steps, device=device)
        if device.type == "cuda":
            torch.cuda.reset_peak_memory_stats(device)
            start_event = torch.cuda.Event(enable_timing=True)
            end_event = torch.cuda.Event(enable_timing=True)
        _synchronize(device)
        measured_started = time.perf_counter()
        if device.type == "cuda":
            start_event.record()
        for index in range(config.steps):
            losses[index].copy_(step(next_batch()))
        if device.type == "cuda":
            end_event.record()
        _synchronize(device)
        wall_seconds = time.perf_counter() - measured_started
        cuda_seconds = start_event.elapsed_time(end_event) / 1000 if device.type == "cuda" else None
        memory = (
            {"peak_allocated_bytes": torch.cuda.max_memory_allocated(device),
             "peak_reserved_bytes": torch.cuda.max_memory_reserved(device)}
            if device.type == "cuda" else None
        )
        loss_values = losses.cpu().tolist()
        if not all(math.isfinite(value) for value in loss_values):
            raise BenchmarkError("measured losses are not all finite")
        parameters_finite = all(
            bool(torch.isfinite(parameter).all()) for parameter in model.parameters()
        )
        if not parameters_finite:
            raise BenchmarkError("final parameters are not all finite")
        updates = _optimizer_steps(optimizer, model.position_embedding) - updates_before
        final_digest = _digest(model)
        with torch.no_grad(), _autocast(config):
            policy, value = model(diagnostic_batch.tokens)
            final_loss = policy_value_loss(policy, value, diagnostic_batch).total.item()
        if not math.isfinite(final_loss) or final_digest == initial_digest:
            raise BenchmarkError("final diagnostics are non-finite or weights were not updated")
        if wall_seconds <= 0 or not math.isfinite(wall_seconds):
            raise BenchmarkError("invalid wall time")
        return {
            "format": "rshogi-dl-training-benchmark-v1", "training_status": TRAINING_STATUS,
            "model_config": model_config.to_dict(), "benchmark_config": asdict(config),
            "torch_version": torch.__version__, "cuda_version": torch.version.cuda,
            "device_name": torch.cuda.get_device_name(device) if device.type == "cuda" else "cpu",
            "compile_scope": "model" if config.execution == "compile" else None,
            "cudagraphs_requested": cudagraphs_requested,
            "input_window_sha256": data_digest.hexdigest(),
            "window_batches_loaded": window_count,
            "window_samples": window_count * config.batch_size,
            "startup_seconds": startup_seconds, "warmup_seconds": warmup_seconds,
            "measured_wall_seconds": wall_seconds, "cuda_stream_seconds": cuda_seconds,
            "attempted_steps": config.steps, "optimizer_updates": updates,
            "skipped_updates": config.steps - updates,
            "samples_per_second": config.steps * config.batch_size / wall_seconds,
            "updated_samples_per_second": updates * config.batch_size / wall_seconds,
            "memory": memory, "initial_loss": initial_loss, "final_loss": final_loss,
            "measured_loss_first": loss_values[0], "measured_loss_last": loss_values[-1],
            "losses_sha256": hashlib.sha256(losses.cpu().numpy().tobytes()).hexdigest(),
            "initial_weights_sha256": initial_digest, "final_weights_sha256": final_digest,
            "parameters_finite": parameters_finite,
            "deterministic_algorithms": torch.are_deterministic_algorithms_enabled(),
        }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="有限回の学習更新を計測する。学習品質の評価には使わない。")
    parser.add_argument("--data", required=True, help="encode 済み fixture JSONL")
    parser.add_argument("--config", choices=sorted(NAMED_CONFIGS), default="initial")
    parser.add_argument("--device", choices=("cpu", "cuda"), default="cpu")
    parser.add_argument("--attention-backend", choices=ATTENTION_BACKENDS, default="explicit")
    parser.add_argument("--execution", choices=("eager", "compile"), default="eager")
    parser.add_argument("--compile-mode", choices=COMPILE_MODES, default="default")
    parser.add_argument("--precision", choices=("fp32", "tf32", "fp16", "bf16"), default="fp32")
    parser.add_argument("--input-mode", choices=("resident", "stream"), default="resident")
    parser.add_argument(
        "--policy-rows", dest="policy_rows_mode", choices=("prepared", "legacy"), default="prepared"
    )
    parser.add_argument("--fused-optimizer", action="store_true")
    for name, default in (("batch-size", 32), ("steps", 20), ("warmup", 5),
                          ("window-batches", 4), ("seed", 0), ("threads", 1)):
        parser.add_argument(f"--{name}", type=int, default=default)
    parser.add_argument("--learning-rate", type=float, default=1e-3)
    parser.add_argument("--weight-decay", type=float, default=0.0)
    parser.add_argument("--output", help="新規 JSON の絶対 path ($SHOGI_DATA の許可された subtree 内)")
    args = parser.parse_args(argv)
    try:
        output = resolve_new_artifact(args.output) if args.output else None
        config = BenchmarkConfig(**{field.name: getattr(args, field.name) for field in fields(BenchmarkConfig)})
        report = run_benchmark(args.data, NAMED_CONFIGS[args.config], config)
        encoded = json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + "\n"
        if output is not None:
            output.parent.mkdir(parents=True, exist_ok=True)
            with output.open("x", encoding="utf-8") as stream:
                stream.write(encoded)
        print(encoded, end="")
        return 0
    except (ArtifactPathError, RecordError, BenchmarkError, ValueError, OSError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
