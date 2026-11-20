"""Training forward with compiled attention residual addition and LayerNorm."""

from collections.abc import Callable

import torch
from torch.nn import functional

from .model import FeedForward, SquareTransformer


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
    cast_output: bool = False,
) -> Callable[[torch.Tensor], tuple[torch.Tensor, torch.Tensor]]:
    normalize = _add_normalize_bf16 if cast_output else _add_normalize
    add_normalize = torch.compile(normalize, fullgraph=True, dynamic=True)

    def forward(tokens: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        residual = model._embed(tokens)
        for block in model.blocks:
            norm = block.ffn_norm
            residual, ffn_input = add_normalize(
                residual, block.attention(block.attention_norm(residual)),
                norm.weight, norm.bias, norm.eps,
            )
            update = block.ffn(ffn_input) if run_ffn is None else run_ffn(block.ffn, ffn_input)
            residual = residual + update
        return model._readout(model.final_norm(residual))

    return forward
