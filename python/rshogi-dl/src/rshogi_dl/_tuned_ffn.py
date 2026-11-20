"""Autotune the feed-forward computation for the full training batch."""

from collections.abc import Callable

import torch

from .model import FeedForward, SquareTransformer
from ._residual_norm import compiled_residual_forward


def tuned_ffn_forward(
    model: SquareTransformer, batch_size: int, *, fused_norm: bool,
) -> Callable[[torch.Tensor], tuple[torch.Tensor, torch.Tensor]]:
    tuned = torch.compile(
        FeedForward.forward, fullgraph=True, dynamic=False, mode="max-autotune-no-cudagraphs",
    )

    def run_ffn(layer: FeedForward, inputs: torch.Tensor) -> torch.Tensor:
        if inputs.shape[0] == batch_size:
            return tuned(layer, inputs)
        return layer(inputs)

    if fused_norm:
        return compiled_residual_forward(model, run_ffn=run_ffn)

    def forward(tokens: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        residual = model._embed(tokens)
        for block in model.blocks:
            residual = residual + block.attention(block.attention_norm(residual))
            residual = residual + run_ffn(block.ffn, block.ffn_norm(residual))
        return model._readout(model.final_norm(residual))

    return forward
