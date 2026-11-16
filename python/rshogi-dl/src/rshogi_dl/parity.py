"""PyTorch (CPU) と ONNX Runtime の出力を照合する。

``python -m rshogi_dl.parity --checkpoint CKPT --onnx MODEL.onnx --data FIXTURE.jsonl --provider cpu``
"""

from __future__ import annotations

import argparse
import itertools
import json
import sys
from pathlib import Path

import numpy as np
import onnxruntime
import torch

from .checkpoint import CheckpointError, load_checkpoint
from .contract import (
    CPU_PARITY_ATOL,
    CPU_PARITY_RTOL,
    CUDA_PARITY_ATOL,
    CUDA_PARITY_RTOL,
    ONNX_INPUT_NAME,
    ONNX_POLICY_NAME,
    ONNX_VALUE_NAME,
    PARITY_BATCH_SIZES,
)
from .data import RecordError, iter_records
from .model import ExportModule, SquareTransformer

_PROVIDERS = {
    "cpu": ("CPUExecutionProvider", CPU_PARITY_RTOL, CPU_PARITY_ATOL),
    "cuda": ("CUDAExecutionProvider", CUDA_PARITY_RTOL, CUDA_PARITY_ATOL),
}
_ROWS = max(PARITY_BATCH_SIZES)


class ParityError(RuntimeError):
    """出力が許容差を超えた、または要求した provider で実行できなかった。"""


def torch_outputs(model: SquareTransformer, tokens: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """export と同じ意味 (value は sigmoid 後) の PyTorch CPU 出力。"""
    wrapper = ExportModule(model).eval()
    with torch.no_grad():
        policy, value = wrapper(torch.from_numpy(tokens))
    return policy.numpy(), value.numpy()


def open_session(onnx_path: str | Path, provider: str) -> onnxruntime.InferenceSession:
    """指定した provider だけで session を開く。別 provider に落ちた場合は失敗させる。"""
    name = _PROVIDERS[provider][0]
    options = onnxruntime.SessionOptions()
    options.intra_op_num_threads = 1
    session = onnxruntime.InferenceSession(str(onnx_path), options, providers=[name])
    if session.get_providers()[0] != name:
        raise ParityError(f"{name} is not active: {session.get_providers()}")
    return session


def onnx_outputs(
    session: onnxruntime.InferenceSession, tokens: np.ndarray
) -> tuple[np.ndarray, np.ndarray]:
    policy, value = session.run([ONNX_POLICY_NAME, ONNX_VALUE_NAME], {ONNX_INPUT_NAME: tokens})
    return policy, value


def _cases(tokens: np.ndarray) -> dict[str, np.ndarray]:
    cases = {f"batch{size}": tokens[:size] for size in PARITY_BATCH_SIZES}
    cases["reordered"] = tokens[::-1]
    cases["repeated"] = np.repeat(tokens[:1], _ROWS, axis=0)
    return {name: np.ascontiguousarray(case) for name, case in cases.items()}


def check_parity(
    model: SquareTransformer, session: onnxruntime.InferenceSession, tokens: np.ndarray, provider: str
) -> dict[str, dict[str, float]]:
    """batch 1/2/4・並べ替え・同一行の反復で照合し、case ごとの最大絶対差を返す。"""
    if tokens.shape[0] != _ROWS:
        raise ValueError(f"parity needs exactly {_ROWS} token rows")
    _, rtol, atol = _PROVIDERS[provider]
    report: dict[str, dict[str, float]] = {}
    for name, case in _cases(tokens).items():
        expected = torch_outputs(model, case)
        actual = onnx_outputs(session, case)
        differences = {}
        for label, want, got in zip(("policy", "value"), expected, actual, strict=True):
            if want.shape != got.shape:
                raise ParityError(f"{name} {label}: shape {got.shape} != {want.shape}")
            if not np.allclose(got, want, rtol=rtol, atol=atol):
                raise ParityError(
                    f"{name} {label}: max abs diff {np.abs(got - want).max()} exceeds "
                    f"rtol={rtol} atol={atol}"
                )
            differences[label] = float(np.abs(got - want).max())
        report[name] = differences
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m rshogi_dl.parity",
        description="checkpoint と ONNX の出力を固定の許容差で照合する。",
    )
    parser.add_argument("--checkpoint", required=True)
    parser.add_argument("--onnx", required=True)
    parser.add_argument("--data", required=True, help=f"先頭 {_ROWS} 行を使う JSONL")
    parser.add_argument("--provider", choices=sorted(_PROVIDERS), default="cpu")
    args = parser.parse_args(argv)
    torch.set_num_threads(1)
    try:
        records = list(itertools.islice(iter_records(args.data), _ROWS))
        if len(records) < _ROWS:
            raise ParityError(f"--data needs at least {_ROWS} records")
        tokens = np.array([record.tokens for record in records], dtype=np.int64)
        model = load_checkpoint(args.checkpoint).model
        session = open_session(args.onnx, args.provider)
        report = check_parity(model, session, tokens, args.provider)
    except (CheckpointError, ParityError, RecordError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    summary = {
        "provider": args.provider,
        "provider_options": session.get_provider_options(),
        "max_abs_diff": report,
    }
    print(json.dumps(summary, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
