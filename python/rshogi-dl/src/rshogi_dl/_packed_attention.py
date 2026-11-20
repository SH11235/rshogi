"""CUDA training attention for 96 tokens and packed 32-wide heads."""

import torch
from torch.nn import functional
import triton
import triton.language as tl

from .contract import TOKEN_COUNT

_TOKENS = tl.constexpr(TOKEN_COUNT)
_PACKED_HEAD_WIDTH = tl.constexpr(3 * 32)
_SCALE_LOG2 = tl.constexpr((32 ** -0.5) * 1.4426950408889634)
_SCALE = tl.constexpr(32 ** -0.5)


@triton.jit
def _vectors(packed, base, rows, features, heads: tl.constexpr, projection: tl.constexpr):
    address = base + projection * heads * 32 + rows[:, None] * heads * _PACKED_HEAD_WIDTH + features[None, :]
    return tl.load(packed + address, rows[:, None] < _TOKENS, 0)


@triton.jit
def _output_vectors(pointer, batch, head, rows, features,
                    batch_stride: tl.constexpr, head_stride: tl.constexpr, row_stride: tl.constexpr,
                    feature_stride: tl.constexpr):
    address = batch * batch_stride + head * head_stride + rows[:, None] * row_stride
    return tl.load(pointer + address + features[None, :] * feature_stride, rows[:, None] < _TOKENS, 0)


@triton.jit
def _forward(packed, output, normalizer, heads: tl.constexpr, tile: tl.constexpr):
    group = tl.program_id(0)
    batch, head = group // heads, group % heads
    base = batch * _TOKENS * heads * _PACKED_HEAD_WIDTH + head * 32
    row = tl.program_id(1) * tile + tl.arange(0, tile)
    key_row = tl.arange(0, 128)
    col = tl.arange(0, 32)
    q = _vectors(packed, base, row, col, heads, 0)
    k = _vectors(packed, base, key_row, col, heads, 1)
    v = _vectors(packed, base, key_row, col, heads, 2)
    logits = tl.dot(q, tl.trans(k)) * _SCALE_LOG2
    logits = tl.where(key_row[None, :] < _TOKENS, logits, float("-inf"))
    maximum = tl.max(logits, 1)
    exponential = tl.exp2(logits - maximum[:, None])
    denominator = tl.sum(exponential, 1)
    prob = exponential / denominator[:, None]
    vectors = tl.dot(prob.to(tl.bfloat16), v)
    address = (batch * _TOKENS + row[:, None]) * (heads * 32) + head * 32 + col[None, :]
    tl.store(output + address, vectors, row[:, None] < _TOKENS)
    tl.store(normalizer + group * _TOKENS + row, maximum + tl.log2(denominator), row < _TOKENS)


@triton.jit
def _row_delta(output, grad, delta, heads: tl.constexpr,
               batch_stride: tl.constexpr, head_stride: tl.constexpr, row_stride: tl.constexpr,
               feature_stride: tl.constexpr):
    group = tl.program_id(0)
    batch, head = group // heads, group % heads
    row = tl.program_id(1) * 32 + tl.arange(0, 32)
    col = tl.arange(0, 32)
    vectors = _output_vectors(output, batch, head, row, col, _TOKENS * heads * 32, 32, heads * 32, 1)
    upstream = _output_vectors(grad, batch, head, row, col, batch_stride, head_stride, row_stride, feature_stride)
    tl.store(delta + group * _TOKENS + row, tl.sum(vectors.to(tl.float32) * upstream.to(tl.float32), 1))


@triton.jit
def _backward(packed, grad, normalizer, delta, packed_gradient,
              heads: tl.constexpr, tile: tl.constexpr, is_query: tl.constexpr,
              batch_stride: tl.constexpr, head_stride: tl.constexpr, row_stride: tl.constexpr,
              feature_stride: tl.constexpr):
    group = tl.program_id(0)
    batch, head = group // heads, group % heads
    base = batch * _TOKENS * heads * _PACKED_HEAD_WIDTH + head * 32
    tile_row = tl.program_id(1) * tile + tl.arange(0, tile)
    all_rows = tl.arange(0, 128)
    col = tl.arange(0, 32)
    if is_query:
        qrow, krow = tile_row, all_rows
    else:
        qrow, krow = all_rows, tile_row
    q = _vectors(packed, base, qrow, col, heads, 0)
    k = _vectors(packed, base, krow, col, heads, 1)
    v = _vectors(packed, base, krow, col, heads, 2)
    upstream = _output_vectors(grad, batch, head, qrow, col, batch_stride, head_stride, row_stride, feature_stride)
    lse = tl.load(normalizer + group * _TOKENS + qrow, qrow < _TOKENS, 0)
    correction = tl.load(delta + group * _TOKENS + qrow, qrow < _TOKENS, 0)
    logits = tl.dot(q, tl.trans(k)) * _SCALE_LOG2
    prob = tl.where((qrow[:, None] < _TOKENS) & (krow[None, :] < _TOKENS), tl.exp2(logits - lse[:, None]), 0.0)
    changes = tl.dot(upstream, tl.trans(v))
    weights = (prob * (changes - correction[:, None]) * _SCALE).to(tl.bfloat16)
    address = base + tile_row[:, None] * heads * _PACKED_HEAD_WIDTH + col[None, :]
    if is_query:
        d_query = tl.dot(weights, k)
        tl.store(packed_gradient + address, d_query, tile_row[:, None] < _TOKENS)
    else:
        d_key = tl.dot(tl.trans(weights), q)
        d_value = tl.dot(tl.trans(prob.to(tl.bfloat16)), upstream)
        tl.store(packed_gradient + address + heads * 32, d_key, tile_row[:, None] < _TOKENS)
        tl.store(packed_gradient + address + heads * 64, d_value, tile_row[:, None] < _TOKENS)


class _PackedAttention(torch.autograd.Function):
    @staticmethod
    def forward(ctx, packed, heads):
        batch = packed.shape[0]
        groups = batch * heads
        tile = 64
        output = torch.empty((batch, TOKEN_COUNT, heads, 32), device=packed.device, dtype=packed.dtype)
        normalizer = torch.empty((groups, TOKEN_COUNT), device=packed.device, dtype=torch.float32)
        _forward[(groups, triton.cdiv(TOKEN_COUNT, tile))](packed, output, normalizer, heads, tile, num_warps=4)
        ctx.save_for_backward(packed, output, normalizer)
        ctx.heads = heads
        return output.transpose(1, 2)

    @staticmethod
    def backward(ctx, grad):
        packed, output, normalizer = ctx.saved_tensors
        heads, tile = ctx.heads, 64
        groups = packed.shape[0] * heads
        delta = torch.empty_like(normalizer)
        gradient = torch.empty_like(packed)
        strides = grad.stride()
        _row_delta[(groups, TOKEN_COUNT // 32)](output, grad, delta, heads, *strides, num_warps=4)
        for query in (True, False):
            _backward[(groups, triton.cdiv(TOKEN_COUNT, tile))](
                packed, grad, normalizer, delta, gradient, heads, tile, query, *strides, num_warps=4,
            )
        return gradient, None


def packed_attention_forward(layer, inputs):
    packed = layer.qkv(inputs)
    if not packed.is_cuda or packed.dtype != torch.bfloat16 or not packed.is_contiguous() or packed.numel() >= 2**31:
        query, key, value = (layer._split_heads(projection) for projection in packed.chunk(3, dim=-1))
        attended = functional.scaled_dot_product_attention(
            query, key, value, dropout_p=0.0, is_causal=False, scale=layer.scale,
        )
    else:
        attended = _PackedAttention.apply(packed, layer.heads)
    mixed = attended.transpose(1, 2)
    return layer.output(mixed.reshape(-1, TOKEN_COUNT, layer.heads * 32))
