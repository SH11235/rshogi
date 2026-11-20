"""Training forward with compiled residual additions and LayerNorm."""

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
        normalized = model.blocks[0].attention_norm(residual)
        for index, block in enumerate(model.blocks):
            norm = block.ffn_norm
            residual, ffn_input = add_normalize(
                residual, block.attention(normalized), norm.weight, norm.bias, norm.eps,
            )
            norm = (
                model.blocks[index + 1].attention_norm
                if index + 1 < len(model.blocks) else model.final_norm
            )
            residual, normalized = add_normalize(
                residual, block.ffn(ffn_input), norm.weight, norm.bias, norm.eps,
            )
        return model._readout(normalized)

    return forward
