"""Training forward with compiled attention residual addition and LayerNorm."""

from collections.abc import Callable
from functools import partial

import torch
from torch.nn import functional

from .model import FeedForward, SelfAttention, SquareTransformer


def _add_normalize(residual, update, weight, bias, epsilon):
    residual = residual + update
    return residual, functional.layer_norm(residual, (residual.shape[-1],), weight, bias, epsilon)


def _add_normalize_bf16(residual, update, weight, bias, epsilon):
    residual, normalized = _add_normalize(residual, update, weight, bias, epsilon)
    return residual, normalized.to(torch.bfloat16)


def compiled_residual_forward(
    model: SquareTransformer,
    *,
    run_ffn: Callable[[FeedForward, torch.Tensor], torch.Tensor] | None = None,
    run_attention: Callable[[SelfAttention, torch.Tensor], torch.Tensor] | None = None,
    cast_output: bool = False,
    fuse_ffn_residual_norm: bool = False,
    fuse_ffn_residual_norm_backward: bool = False,
    fuse_attention_residual_norm: bool = False,
) -> Callable[[torch.Tensor], tuple[torch.Tensor, torch.Tensor]]:
    normalize_ffn = None
    if fuse_ffn_residual_norm or fuse_attention_residual_norm:
        try:
            from ._ffn_residual_norm import ffn_residual_norm
        except ImportError as error:
            option = "fuse_attention_residual_norm" if fuse_attention_residual_norm else "fuse_ffn_residual_norm"
            raise ValueError(f"{option} requires CUDA Triton") from error
    if fuse_ffn_residual_norm:
        normalize_ffn = (
            partial(ffn_residual_norm, fuse_backward=True)
            if fuse_ffn_residual_norm_backward else ffn_residual_norm
        )

    if fuse_attention_residual_norm:
        def add_normalize(residual, update, weight, bias, epsilon):
            residual, normalized = ffn_residual_norm(residual, update, weight, bias, epsilon, fuse_backward=True)
            # The kernel fallback returns its normalized output in FP32.
            return residual, normalized.to(torch.bfloat16)
    else:
        normalize = _add_normalize_bf16 if cast_output else _add_normalize
        add_normalize = torch.compile(normalize, fullgraph=True, dynamic=True)

    def forward(tokens: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        residual = model._embed(tokens)
        normalized = None
        for index, block in enumerate(model.blocks):
            attention_input = block.attention_norm(residual) if normalized is None else normalized
            attention = (
                block.attention(attention_input) if run_attention is None else run_attention(block.attention, attention_input)
            )
            norm = block.ffn_norm
            residual, ffn_input = add_normalize(
                residual, attention, norm.weight, norm.bias, norm.eps,
            )
            update = block.ffn(ffn_input) if run_ffn is None else run_ffn(block.ffn, ffn_input)
            if normalize_ffn is not None and index + 1 < len(model.blocks):
                norm = model.blocks[index + 1].attention_norm
                residual, normalized = normalize_ffn(residual, update, norm.weight, norm.bias, norm.eps)
            else:
                residual = residual + update
        return model._readout(model.final_norm(residual))

    return forward
