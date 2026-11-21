"""Autotune selected encoder computations for full training batches."""

from collections.abc import Callable
from functools import partial

import torch

from .model import FeedForward, SelfAttention, SquareTransformer
from ._residual_norm import compiled_residual_forward


def tuned_training_forward(
    model: SquareTransformer, batch_size: int, *, fused_norm: bool, cast_norm: bool = False,
    compile_ffn: bool = False, compile_attention: bool = False, fuse_ffn_residual_norm: bool = False,
    fuse_ffn_residual_norm_backward: bool = False,
    fuse_packed_attention: bool = False,
    fuse_packed_attention_backward: bool = False,
    tile_packed_attention_backward: bool = False,
    tile_packed_attention_forward: bool = False,
    fuse_attention_residual_norm: bool = False,
    fuse_final_residual_norm: bool = False,
    fuse_packed_qkv_bias_gradient: bool = False,
    fuse_ffn_expand_bias_gradient: bool = False,
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

    ffn_forward = FeedForward.forward
    if fuse_ffn_expand_bias_gradient:
        try:
            from ._ffn_expand_bias import fused_ffn_forward
        except ImportError as error:
            raise ValueError("fuse_ffn_expand_bias_gradient requires CUDA Triton") from error
        ffn_forward = fused_ffn_forward
    run_ffn = select_forward(ffn_forward, compile_ffn)
    attention_operation = SelfAttention.forward
    if fuse_packed_attention:
        try:
            from ._packed_attention import packed_attention_forward
        except ImportError as error:
            raise ValueError("fuse_packed_attention requires CUDA Triton") from error
        attention_operation = packed_attention_forward
        if fuse_packed_attention_backward or tile_packed_attention_forward:
            attention_operation = partial(packed_attention_forward, shared_backward=fuse_packed_attention_backward,
                                          tiled_backward=tile_packed_attention_backward,
                                          tiled_forward=tile_packed_attention_forward,
                                          fuse_qkv_bias_gradient=fuse_packed_qkv_bias_gradient)
    run_attention = select_forward(attention_operation, compile_attention)
    if run_attention is None and fuse_packed_attention:
        run_attention = attention_operation

    if fused_norm:
        return compiled_residual_forward(
            model, run_ffn=run_ffn, run_attention=run_attention, cast_output=cast_norm,
            fuse_ffn_residual_norm=fuse_ffn_residual_norm,
            fuse_ffn_residual_norm_backward=fuse_ffn_residual_norm_backward,
            fuse_attention_residual_norm=fuse_attention_residual_norm,
            fuse_final_residual_norm=fuse_final_residual_norm,
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
