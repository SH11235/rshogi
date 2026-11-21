"""Fused expansion bias gradients for BF16 CUDA feed-forward training."""

import torch
from torch.nn import functional
import triton
import triton.language as tl
from triton.language.extra.cuda import libdevice

from .contract import TOKEN_COUNT


@triton.jit
def _gelu_derivative(value):
    derivative = (1 + libdevice.erf(value * 0.7071067811865476)) * 0.5
    return derivative + value * libdevice.exp(-0.5 * value * value) * 0.3989422804014327


@triton.jit
def _fill_gelu_derivatives(table):
    bits = tl.program_id(0) * 256 + tl.arange(0, 256)
    value = bits.to(tl.uint16).to(tl.bfloat16, bitcast=True).to(tl.float32)
    tl.store(table + bits, _gelu_derivative(value))


def gelu_derivative_table(device):
    table = torch.empty(65_536, device=device, dtype=torch.float32)
    _fill_gelu_derivatives[(256,)](table, num_warps=4)
    return table


@triton.jit
def _gelu_bias_gradient(upstream, project_weight, preactivation, gradient, partials, table=None):
    tile = tl.program_id(0)
    column_tiles: tl.constexpr = 16
    group_size: tl.constexpr = 8
    group = tile // (group_size * column_tiles)
    tile_row = group * group_size + tile % group_size
    tile_column = tile % (group_size * column_tiles) // group_size
    row = tile_row * 128 + tl.arange(0, 128)
    column = tile_column * 64 + tl.arange(0, 64)
    reduction = tl.arange(0, 32)
    accumulator = tl.full((128, 64), 0, tl.float32)
    for block in range(8):
        inner = block * 32 + reduction
        left = tl.load(upstream + row[:, None] * 256 + inner[None, :])
        right = tl.load(project_weight + inner[:, None] * 1024 + column[None, :])
        accumulator += tl.dot(left, right)
    index = row[:, None] * 1024 + column[None, :]
    value = tl.load(preactivation + index)
    if table is None:
        derivative = _gelu_derivative(value.to(tl.float32))
    else:
        bits = value.to(tl.uint16, bitcast=True).to(tl.int32)
        derivative = tl.load(table + bits)
    rounded = (accumulator.to(tl.bfloat16).to(tl.float32) * derivative).to(tl.bfloat16)
    tl.store(gradient + index, rounded)
    tl.store(partials + tile_row * 1024 + column, tl.sum(rounded.to(tl.float32), axis=0))


@torch.library.custom_op("rshogi_dl::ffn_project_weight_gradient", mutates_args=())
def _project_weight_gradient(upstream: torch.Tensor, activation: torch.Tensor) -> torch.Tensor:
    # Inductor can otherwise fuse the weight gradient cast into a different GEMM.
    return torch.mm(upstream.mT, activation)


@_project_weight_gradient.register_fake
def _fake_project_weight_gradient(upstream, activation):
    return upstream.new_empty((upstream.shape[1], activation.shape[1]))


class _ExpandBiasGradient(torch.autograd.Function):
    @staticmethod
    def forward(ctx, inputs, expand_weight, expand_bias, project_weight, project_bias, gelu_derivatives):
        preactivation = functional.linear(inputs, expand_weight, expand_bias)
        activation = functional.gelu(preactivation)
        output = functional.linear(activation, project_weight, project_bias)
        ctx.save_for_backward(inputs, expand_weight, project_weight, preactivation, activation, gelu_derivatives)
        return output

    @staticmethod
    def backward(ctx, upstream):
        inputs, expand_weight, project_weight, preactivation, activation, gelu_derivatives = ctx.saved_tensors
        incoming = upstream.reshape(-1, 256).contiguous()
        expanded = preactivation.reshape(-1, 1024).contiguous()
        gradient = torch.empty_like(expanded)
        partials = torch.empty((192, 1024), device=gradient.device, dtype=torch.float32)
        _gelu_bias_gradient[(192 * 16,)](
            incoming, project_weight, expanded, gradient, partials, gelu_derivatives, num_warps=4, num_stages=3,
        )
        grad_inputs = (gradient @ expand_weight).reshape_as(inputs)
        grad_expand_weight = gradient.mT @ inputs.reshape(-1, 256)
        grad_expand_bias = partials.sum(dim=0).to(torch.bfloat16)
        grad_project_weight = _project_weight_gradient(incoming, activation.reshape(-1, 1024))
        grad_project_bias = incoming.sum(dim=0)
        return grad_inputs, grad_expand_weight, grad_expand_bias, grad_project_weight, grad_project_bias, None


def fused_ffn_forward(layer, inputs, *, gelu_derivatives=None):
    if (inputs.shape != (256, TOKEN_COUNT, 256) or inputs.dtype != torch.bfloat16 or not inputs.is_cuda
            or layer.expand.out_features != 1024 or not layer.project.weight.is_contiguous()):
        return layer(inputs)
    return _ExpandBiasGradient.apply(
        inputs, layer.expand.weight.to(inputs.dtype), layer.expand.bias.to(inputs.dtype),
        layer.project.weight.to(inputs.dtype), layer.project.bias.to(inputs.dtype), gelu_derivatives,
    )
