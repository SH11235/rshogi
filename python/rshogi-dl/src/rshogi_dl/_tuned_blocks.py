"""Autotune selected encoder computations for full training batches."""

from collections.abc import Callable

import torch

from .model import FeedForward, SelfAttention, SquareTransformer
from ._residual_norm import compiled_residual_forward


def tuned_training_forward(
    model: SquareTransformer, batch_size: int, *, fused_norm: bool, cast_norm: bool = False,
    compile_ffn: bool = False, compile_attention: bool = False, fuse_ffn_residual_norm: bool = False,
    fuse_packed_attention: bool = False,
) -> Callable[[torch.Tensor], tuple[torch.Tensor, torch.Tensor]]:
    def select_forward(operation, enabled):
        if not enabled:
            return None
        tuned = torch.compile(operation, fullgraph=True, dynamic=False, mode="max-autotune-no-cudagraphs")

        def execute(layer, inputs):
            if inputs.shape[0] == batch_size:
                return tuned(layer, inputs)
            return layer(inputs)

        return execute

    run_ffn = select_forward(FeedForward.forward, compile_ffn)
    attention_operation = SelfAttention.forward
    if fuse_packed_attention:
        try:
            from ._packed_attention import packed_attention_forward
        except ImportError as error:
            raise ValueError("fuse_packed_attention requires CUDA Triton") from error
        attention_operation = packed_attention_forward
    run_attention = select_forward(attention_operation, compile_attention)
    if run_attention is None and fuse_packed_attention:
        run_attention = attention_operation

    if fused_norm:
        return compiled_residual_forward(
            model, run_ffn=run_ffn, run_attention=run_attention, cast_output=cast_norm,
            fuse_ffn_residual_norm=fuse_ffn_residual_norm,
        )

    def forward(tokens: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        residual = model._embed(tokens)
        for block in model.blocks:
            attention_input = block.attention_norm(residual)
            attention = (
                block.attention(attention_input) if run_attention is None else run_attention(block.attention, attention_input)
            )
            residual = residual + attention
            ffn_input = block.ffn_norm(residual)
            update = block.ffn(ffn_input) if run_ffn is None else run_ffn(block.ffn, ffn_input)
            residual = residual + update
        return model._readout(model.final_norm(residual))

    return forward
