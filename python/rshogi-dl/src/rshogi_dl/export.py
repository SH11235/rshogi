"""checkpoint を engine 用の ONNX に書き出す。

``python -m rshogi_dl.export --checkpoint CKPT --output OUT.onnx``
"""

from __future__ import annotations

import argparse
import io
import json
import sys
from pathlib import Path

import numpy as np
import onnx
import onnxruntime
import torch

from .checkpoint import CheckpointError, load_checkpoint
from .contract import (
    ARCHITECTURE,
    CONTEXT_BLACK_TO_MOVE,
    INPUT_FORMAT,
    METADATA_ARCHITECTURE,
    METADATA_INPUT_FORMAT,
    METADATA_MODEL_CONFIG,
    METADATA_POLICY_FORMAT,
    METADATA_TRAINING_STATUS,
    METADATA_VALUE_FORMAT,
    ONNX_INPUT_NAME,
    ONNX_OPSET,
    ONNX_POLICY_NAME,
    ONNX_VALUE_NAME,
    POLICY_FORMAT,
    POLICY_SIZE,
    TOKEN_COUNT,
    TOKEN_ID_RANGES,
    TRAINING_STATUS,
    VALUE_FORMAT,
)
from .model import ExportModule, ModelConfig, SquareTransformer
from .paths import ArtifactPathError, resolve_new_artifact


class ExportError(RuntimeError):
    """書き出した ONNX が契約を満たさない。"""


def expected_metadata(config: ModelConfig) -> dict[str, str]:
    """ONNX に記録する metadata。"""
    return {
        METADATA_INPUT_FORMAT: INPUT_FORMAT,
        METADATA_POLICY_FORMAT: POLICY_FORMAT,
        METADATA_VALUE_FORMAT: VALUE_FORMAT,
        METADATA_ARCHITECTURE: ARCHITECTURE,
        METADATA_MODEL_CONFIG: json.dumps(config.to_dict(), sort_keys=True),
        METADATA_TRAINING_STATUS: TRAINING_STATUS,
    }


def _example_tokens(batch_size: int) -> torch.Tensor:
    row = [low for low, _ in TOKEN_ID_RANGES]
    assert row[-1] == CONTEXT_BLACK_TO_MOVE
    return torch.tensor([row] * batch_size, dtype=torch.int64)


def _declared_shape(value_info: onnx.ValueInfoProto) -> list[int | str]:
    shape: list[int | str] = []
    for dim in value_info.type.tensor_type.shape.dim:
        if dim.HasField("dim_value"):
            shape.append(dim.dim_value)
        elif dim.dim_param:
            shape.append(dim.dim_param)
        else:
            raise ExportError(f"{value_info.name}: dimension without value or name")
    return shape


def _check_tensor(
    value_info: onnx.ValueInfoProto, name: str, elem_type: int, trailing: int
) -> None:
    if value_info.name != name:
        raise ExportError(f"expected tensor {name!r}, got {value_info.name!r}")
    if value_info.type.tensor_type.elem_type != elem_type:
        raise ExportError(f"{name}: unexpected element type")
    shape = _declared_shape(value_info)
    if len(shape) != 2 or not isinstance(shape[0], str) or shape[1] != trailing:
        raise ExportError(f"{name}: expected shape [dynamic batch, {trailing}], got {shape}")


def verify_onnx_bytes(data: bytes, config: ModelConfig, *, ort_fusion: bool = False) -> None:
    """ONNX の graph・metadata・動的 batch を検査する。合わなければ :class:`ExportError`。"""
    model = onnx.load_from_string(data)
    onnx.checker.check_model(model)

    opsets = {entry.domain: entry.version for entry in model.opset_import}
    if opsets.get("", opsets.get("ai.onnx")) != ONNX_OPSET:
        raise ExportError(f"expected opset {ONNX_OPSET}, got {opsets}")

    graph = model.graph
    initializers = {initializer.name for initializer in graph.initializer}
    inputs = [value for value in graph.input if value.name not in initializers]
    if len(inputs) != 1 or len(graph.output) != 2:
        raise ExportError("expected exactly one input and two outputs")
    _check_tensor(inputs[0], ONNX_INPUT_NAME, onnx.TensorProto.INT64, TOKEN_COUNT)
    _check_tensor(graph.output[0], ONNX_POLICY_NAME, onnx.TensorProto.FLOAT, POLICY_SIZE)
    _check_tensor(graph.output[1], ONNX_VALUE_NAME, onnx.TensorProto.FLOAT, 1)

    if model.functions:
        raise ExportError("model-local functions are not expected")
    op_counts: dict[str, int] = {}
    for node in graph.node:
        if node.domain == "com.microsoft" and ort_fusion:
            if opsets.get(node.domain) != 1 or node.op_type not in {
                "BiasGelu",
                "Gelu",
                "SkipLayerNormalization",
            }:
                raise ExportError(f"unsupported ORT fusion: {node.op_type}")
        elif node.domain not in ("", "ai.onnx"):
            raise ExportError(f"non-standard operator domain: {node.domain}")
        op_counts[node.op_type] = op_counts.get(node.op_type, 0) + 1
    convolutions = sorted(op for op in op_counts if "Conv" in op)
    if convolutions:
        raise ExportError(f"convolution operators are not expected: {convolutions}")
    # attention が融合 op や定数に畳まれず、層ごとの Softmax と QK^T・重み×V の MatMul として
    # graph に残っていることを確かめる。
    if op_counts.get("Softmax", 0) != config.layers:
        raise ExportError(f"expected {config.layers} Softmax nodes, got {op_counts.get('Softmax', 0)}")
    if op_counts.get("MatMul", 0) < 2 * config.layers:
        raise ExportError("attention MatMul nodes are missing")

    metadata = {entry.key: entry.value for entry in model.metadata_props}
    if metadata != expected_metadata(config):
        raise ExportError(f"unexpected metadata: {metadata}")

    options = onnxruntime.SessionOptions()
    options.intra_op_num_threads = 1
    session = onnxruntime.InferenceSession(data, options, providers=["CPUExecutionProvider"])
    # export 時の例は batch 2。別の batch で実行して batch 軸が固定されていないことを確かめる。
    for batch_size in (1, 3):
        tokens = _example_tokens(batch_size).numpy()
        policy, value = session.run([ONNX_POLICY_NAME, ONNX_VALUE_NAME], {ONNX_INPUT_NAME: tokens})
        if policy.shape != (batch_size, POLICY_SIZE) or policy.dtype != np.float32:
            raise ExportError(f"unexpected policy output: {policy.shape} {policy.dtype}")
        if value.shape != (batch_size, 1) or value.dtype != np.float32:
            raise ExportError(f"unexpected value output: {value.shape} {value.dtype}")
        if not np.isfinite(policy).all() or not np.isfinite(value).all():
            raise ExportError("non-finite output")
        if (value < 0.0).any() or (value > 1.0).any():
            raise ExportError("value output is outside [0, 1]")


def _fuse_for_ort(model: onnx.ModelProto, config: ModelConfig) -> onnx.ModelProto:
    from onnxruntime.transformers.optimizer import optimize_model

    fused = optimize_model(
        model, model_type="vit", num_heads=config.heads, hidden_size=config.width, opt_level=0
    ).model
    if any(node.domain == "com.microsoft" for node in fused.graph.node) and not any(
        entry.domain == "com.microsoft" for entry in fused.opset_import
    ):
        fused.opset_import.append(onnx.helper.make_opsetid("com.microsoft", 1))
    return fused


def onnx_bytes(model: SquareTransformer, *, ort_fusion: bool = False) -> bytes:
    """model を metadata 付きの ONNX に変換し、検査済みの bytes を返す。"""
    wrapper = ExportModule(model).eval()
    buffer = io.BytesIO()
    with torch.no_grad():
        torch.onnx.export(
            wrapper,
            (_example_tokens(2),),
            buffer,
            dynamo=False,
            opset_version=ONNX_OPSET,
            input_names=[ONNX_INPUT_NAME],
            output_names=[ONNX_POLICY_NAME, ONNX_VALUE_NAME],
            dynamic_axes={
                ONNX_INPUT_NAME: {0: "batch"},
                ONNX_POLICY_NAME: {0: "batch"},
                ONNX_VALUE_NAME: {0: "batch"},
            },
        )
    exported = onnx.shape_inference.infer_shapes(onnx.load_from_string(buffer.getvalue()))
    onnx.helper.set_model_props(exported, expected_metadata(model.config))
    if ort_fusion:
        exported = _fuse_for_ort(exported, model.config)
    data = exported.SerializeToString()
    verify_onnx_bytes(data, model.config, ort_fusion=ort_fusion)
    return data


def export_onnx(
    model: SquareTransformer, output_path: str | Path, *, ort_fusion: bool = False
) -> Path:
    """検査を通った ONNX だけを新規 file に書く。既存 file は上書きしない。"""
    resolved = resolve_new_artifact(output_path)
    if resolved.suffix != ".onnx":
        raise ArtifactPathError("output file name must end with .onnx")
    data = onnx_bytes(model, ort_fusion=ort_fusion)
    resolved.parent.mkdir(parents=True, exist_ok=True)
    with open(resolved, "xb") as stream:
        stream.write(data)
    return resolved


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m rshogi_dl.export",
        description="checkpoint を ONNX (opset 18, 動的 batch) に書き出す。",
    )
    parser.add_argument("--checkpoint", required=True, help="smoke が保存した checkpoint")
    parser.add_argument(
        "--ort-fusion", action="store_true", help="ONNX Runtime 向けの演算融合を適用する"
    )
    parser.add_argument(
        "--output",
        required=True,
        help="新規に作る .onnx の絶対 path。$SHOGI_DATA/dl/transformer-v1-validation/ か "
        "$SHOGI_DATA/runs/rshogi-dl/ の下",
    )
    args = parser.parse_args(argv)
    try:
        # checkpoint の読み込みより先に出力先を確かめる。
        resolve_new_artifact(args.output)
        checkpoint = load_checkpoint(args.checkpoint)
        output = export_onnx(checkpoint.model, args.output, ort_fusion=args.ort_fusion)
    except (ArtifactPathError, CheckpointError, ExportError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    print(output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
