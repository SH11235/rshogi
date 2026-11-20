"""Training forward with compiled attention residual addition and LayerNorm."""

from collections.abc import Callable

import torch
from torch.nn import functional

from .model import SquareTransformer


def _add_normalize(residual, update, weight, bias, epsilon):
    residual = residual + update
    return residual, functional.layer_norm(residual, (residual.shape[-1],), weight, bias, epsilon)


def compiled_residual_forward(
    model: SquareTransformer,
) -> Callable[[torch.Tensor], tuple[torch.Tensor, torch.Tensor]]:
    add_normalize = torch.compile(_add_normalize, fullgraph=True, dynamic=True)

    def forward(tokens: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        residual = model._embed(tokens)
        for block in model.blocks:
            norm = block.ffn_norm
            residual, ffn_input = add_normalize(
                residual, block.attention(block.attention_norm(residual)),
                norm.weight, norm.bias, norm.eps,
            )
            residual = residual + block.ffn(ffn_input)
        return model._readout(model.final_norm(residual))

    return forward
