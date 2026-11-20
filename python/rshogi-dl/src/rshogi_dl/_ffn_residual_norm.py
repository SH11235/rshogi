"""Fused FFN residual and LayerNorm for width-128 CUDA training."""

import torch
import triton
import triton.language as tl

from ._residual_norm import _add_normalize


@triton.jit
def _shuffle_xor(value, offset: tl.constexpr):
    return tl.inline_asm_elementwise(
        "shfl.sync.bfly.b32 $0, $1, $2, 31, -1;",
        constraints="=f,f,r", args=[value, offset], dtype=tl.float32, is_pure=True, pack=1,
    )


@triton.jit
def _forward(residual, update, weight, bias, added, normalized, means, inverse_devs, eps: tl.constexpr):
    lane = tl.arange(0, 32)
    row = tl.program_id(0)
    start = 128 * row + 4 * lane
    values = ()
    mean = tl.full((32,), 0.0, tl.float32)
    squares = tl.full((32,), 0.0, tl.float32)
    for i in tl.static_range(4):
        value = tl.load(residual + start + i) + tl.load(update + start + i).to(tl.float32)
        values += (value,)
        tl.store(added + start + i, value)
        delta = value - mean
        mean = tl.fma(delta, 1.0 / (i + 1), mean)
        squares = tl.fma(delta, value - mean, squares)
    # Keeping the native Welford reduction order preserves BF16 rounding at midpoints.
    for stage in tl.static_range(5):
        peer_mean = _shuffle_xor(mean, 16 >> stage)
        peer_squares = _shuffle_xor(squares, 16 >> stage)
        delta = mean - peer_mean
        mean = mean * 0.5 + peer_mean * 0.5
        squares = tl.fma(delta * delta, 2.0 * (1 << stage), squares + peer_squares)
    inverse = tl.rsqrt(squares * (1.0 / 128.0) + eps)
    tl.store(means + row + lane, mean, lane == 0)
    tl.store(inverse_devs + row + lane, inverse, lane == 0)
    for i in tl.static_range(4):
        scale = tl.load(weight + 4 * lane + i)
        shift = tl.load(bias + 4 * lane + i)
        output = tl.fma(scale, inverse * (values[i] - mean), shift)
        tl.store(normalized + start + i, output)


@triton.jit
def _input_gradient(data, means, inverse_devs, scale, residual_upstream, norm_upstream, residual_grad, update_grad):
    row = tl.program_id(0)
    col = tl.arange(0, 128)
    offsets = row * 128 + col
    average = tl.load(means + row)
    inverse = tl.load(inverse_devs + row)
    normalized = (tl.load(data + offsets) - average) * inverse
    weighted = tl.load(norm_upstream + offsets).to(tl.float32) * tl.load(scale + col)
    center = tl.sum(weighted) / 128.0
    stretch = tl.sum(weighted * normalized) / 128.0
    gradient = ((weighted - center) - normalized * stretch) * inverse + tl.load(residual_upstream + offsets)
    tl.store(residual_grad + offsets, gradient)
    tl.store(update_grad + offsets, gradient)


@triton.jit
def _affine_partial(data, means, inverse_devs, norm_upstream, out, rows: tl.constexpr):
    row_block = tl.program_id(0)
    cols = tl.program_id(1) * 32 + tl.arange(0, 32)
    positions = row_block * 64 + tl.arange(0, 64)
    offsets = positions[:, None] * 128 + cols[None, :]
    valid = positions < rows
    upstream = tl.load(norm_upstream + offsets, valid[:, None], 0).to(tl.float32)
    x = tl.load(data + offsets, valid[:, None], 0)
    avg = tl.load(means + positions, valid, 0)
    inverse = tl.load(inverse_devs + positions, valid, 0)
    scale_grad = tl.sum(((x - avg[:, None]) * inverse[:, None]) * upstream, 0)
    shift_grad = tl.sum(upstream, 0)
    tl.store(out + row_block * 256 + cols, scale_grad)
    tl.store(out + row_block * 256 + 128 + cols, shift_grad)


class _ResidualNorm(torch.autograd.Function):
    @staticmethod
    def forward(ctx, residual, update, weight, bias, eps):
        added = torch.empty_like(residual)
        normalized = torch.empty_like(update)
        means = torch.empty(residual.shape[:-1], device=residual.device, dtype=torch.float32)
        inverse = torch.empty_like(means)
        _forward[(means.numel(),)](
            residual, update, weight, bias, added, normalized, means, inverse, eps, num_warps=1,
        )
        ctx.save_for_backward(added, means, inverse, weight)
        return added, normalized

    @staticmethod
    def backward(ctx, residual_upstream, norm_upstream):
        data, means, inverse, weight = ctx.saved_tensors
        norm_upstream = norm_upstream.contiguous()
        residual_grad = torch.empty_like(data)
        update_grad = torch.empty_like(data, dtype=torch.bfloat16)
        rows = means.numel()
        _input_gradient[(rows,)](
            data, means, inverse, weight, residual_upstream.contiguous(), norm_upstream, residual_grad, update_grad,
            num_warps=4,
        )
        tiles = triton.cdiv(rows, 64)
        partial = torch.empty((tiles, 256), dtype=torch.float32, device=data.device)
        _affine_partial[(tiles, 4)](data, means, inverse, norm_upstream, partial, rows, num_warps=4)
        scale, shift = partial.sum(0).chunk(2)
        return residual_grad, update_grad, scale, shift, None


def ffn_residual_norm(residual, update, weight, bias, eps):
    if (
        not residual.is_cuda
        or not residual.ndim
        or residual.shape[-1] != 128
        or not residual.numel()
        or update.shape != residual.shape
        or weight.shape != (128,)
        or bias.shape != (128,)
        or residual.dtype != torch.float32
        or update.dtype != torch.bfloat16
        or weight.dtype != torch.float32
        or bias.dtype != torch.float32
        or any(t.device != residual.device for t in (update, weight, bias))
        or not all(t.is_contiguous() for t in (residual, update, weight, bias))
    ):
        return _add_normalize(residual, update, weight, bias, eps)
    return _ResidualNorm.apply(residual, update, weight, bias, eps)
