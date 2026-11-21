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
def _tile_normalize(q, k0, k1, v0, v1):
    logits0 = tl.dot(q, tl.trans(k0)) * _SCALE_LOG2
    logits1 = tl.dot(q, tl.trans(k1)) * _SCALE_LOG2
    maximum = tl.maximum(tl.max(logits0, 1), tl.max(logits1, 1))
    exponential0 = tl.exp2(logits0 - maximum[:, None])
    exponential1 = tl.exp2(logits1 - maximum[:, None])
    denominator = tl.sum(exponential0, 1) + tl.sum(exponential1, 1)
    prob0 = (exponential0 / denominator[:, None]).to(tl.bfloat16)
    prob1 = (exponential1 / denominator[:, None]).to(tl.bfloat16)
    y = tl.dot(prob1, v1, tl.dot(prob0, v0))
    return y, maximum + tl.log2(denominator)


@triton.jit
def _tiled_forward(packed, output, normalizer, heads: tl.constexpr):
    group = tl.program_id(0)
    batch, head = group // heads, group % heads
    base = batch * _TOKENS * heads * _PACKED_HEAD_WIDTH + head * 32
    r0 = tl.arange(0, 64)
    r1 = 64 + tl.arange(0, 32)
    col = tl.arange(0, 32)
    k0 = _vectors(packed, base, r0, col, heads, 1)
    v0 = _vectors(packed, base, r0, col, heads, 2)
    k1 = _vectors(packed, base, r1, col, heads, 1)
    v1 = _vectors(packed, base, r1, col, heads, 2)
    q0 = _vectors(packed, base, r0, col, heads, 0)
    y0, lse0 = _tile_normalize(q0, k0, k1, v0, v1)
    a0 = (batch * _TOKENS + r0[:, None]) * heads * 32 + head * 32 + col[None, :]
    tl.store(output + a0, y0)
    tl.store(normalizer + group * _TOKENS + r0, lse0)
    q1 = _vectors(packed, base, r1, col, heads, 0)
    y1, lse1 = _tile_normalize(q1, k0, k1, v0, v1)
    a1 = (batch * _TOKENS + r1[:, None]) * heads * 32 + head * 32 + col[None, :]
    tl.store(output + a1, y1)
    tl.store(normalizer + group * _TOKENS + r1, lse1)


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


@triton.jit
def _shared_backward(packed, output, grad, normalizer, result, heads: tl.constexpr,
                    batch_stride: tl.constexpr, head_stride: tl.constexpr, row_stride: tl.constexpr,
                    feature_stride: tl.constexpr):
    group = tl.program_id(0)
    batch, head = group // heads, group % heads
    base = batch * _TOKENS * heads * _PACKED_HEAD_WIDTH + head * 32
    row = tl.arange(0, 128)
    col = tl.arange(0, 32)
    mask = row < _TOKENS
    q = _vectors(packed, base, row, col, heads, 0)
    k = _vectors(packed, base, row, col, heads, 1)
    v = _vectors(packed, base, row, col, heads, 2)
    y = _output_vectors(output, batch, head, row, col, _TOKENS * heads * 32, 32, heads * 32, 1)
    dy = _output_vectors(grad, batch, head, row, col, batch_stride, head_stride, row_stride, feature_stride)
    lse = tl.load(normalizer + group * _TOKENS + row, mask, 0)
    row_delta = tl.sum(y.to(tl.float32) * dy.to(tl.float32), 1)
    logits = tl.dot(q, tl.trans(k)) * _SCALE_LOG2
    prob = tl.where(mask[:, None] & mask[None, :], tl.exp2(logits - lse[:, None]), 0.0)
    change = (prob * (tl.dot(dy, tl.trans(v)) - row_delta[:, None]) * _SCALE).to(tl.bfloat16)
    dq = tl.dot(change, k)
    dk = tl.dot(tl.trans(change), q)
    dv = tl.dot(tl.trans(prob.to(tl.bfloat16)), dy)
    address = base + row[:, None] * heads * _PACKED_HEAD_WIDTH + col[None, :]
    tl.store(result + address, dq, mask[:, None])
    tl.store(result + address + heads * 32, dk, mask[:, None])
    tl.store(result + address + heads * 64, dv, mask[:, None])


@triton.jit
def _tile_gradients(q, k, v, dy, lse, delta):
    logits = tl.dot(q, tl.trans(k)) * _SCALE_LOG2
    probability = tl.exp2(logits - lse[:, None])
    change = probability * (tl.dot(dy, tl.trans(v)) - delta[:, None]) * _SCALE
    return probability.to(tl.bfloat16), change.to(tl.bfloat16)


@triton.jit
def _store_tiled_gradient(result, address, gradient, partial_bias, partial_address,
                          col, tile: tl.constexpr, collect_bias: tl.constexpr):
    if collect_bias:
        rounded = gradient.to(tl.bfloat16)
        tl.store(result + address, rounded)
        batch_stride = tl.num_programs(0) * _PACKED_HEAD_WIDTH
        totals = tl.sum(rounded.to(tl.float32), axis=0)
        tl.store(partial_bias + tile * batch_stride + partial_address + col, totals)
    else:
        tl.store(result + address, gradient)


@triton.jit
def _tiled_backward(packed, output, grad, normalizer, result, heads: tl.constexpr,
             batch_stride: tl.constexpr, head_stride: tl.constexpr, row_stride: tl.constexpr,
             feature_stride: tl.constexpr, partial_bias, collect_bias: tl.constexpr):
    group = tl.program_id(0)
    batch, head = group // heads, group % heads
    base = batch * _TOKENS * heads * _PACKED_HEAD_WIDTH + head * 32
    r0 = tl.arange(0, 64)
    r1 = 64 + tl.arange(0, 32)
    col = tl.arange(0, 32)
    q0 = _vectors(packed, base, r0, col, heads, 0)
    k0 = _vectors(packed, base, r0, col, heads, 1)
    v0 = _vectors(packed, base, r0, col, heads, 2)
    q1 = _vectors(packed, base, r1, col, heads, 0)
    k1 = _vectors(packed, base, r1, col, heads, 1)
    v1 = _vectors(packed, base, r1, col, heads, 2)
    y0 = _output_vectors(output, batch, head, r0, col, _TOKENS * heads * 32, 32, heads * 32, 1)
    y1 = _output_vectors(output, batch, head, r1, col, _TOKENS * heads * 32, 32, heads * 32, 1)
    dy0 = _output_vectors(grad, batch, head, r0, col, batch_stride, head_stride, row_stride, feature_stride)
    dy1 = _output_vectors(grad, batch, head, r1, col, batch_stride, head_stride, row_stride, feature_stride)
    lse0 = tl.load(normalizer + group * _TOKENS + r0)
    lse1 = tl.load(normalizer + group * _TOKENS + r1)
    delta0 = tl.sum(y0.to(tl.float32) * dy0.to(tl.float32), 1)
    delta1 = tl.sum(y1.to(tl.float32) * dy1.to(tl.float32), 1)
    a0 = base + r0[:, None] * heads * _PACKED_HEAD_WIDTH + col[None, :]
    a1 = base + r1[:, None] * heads * _PACKED_HEAD_WIDTH + col[None, :]
    bias_base = batch * heads * _PACKED_HEAD_WIDTH + head * 32
    key_bias = bias_base + heads * 32
    value_bias = bias_base + heads * 64

    p00, c00 = _tile_gradients(q0, k0, v0, dy0, lse0, delta0)
    dq0 = tl.dot(c00, k0)
    dk0 = tl.dot(tl.trans(c00), q0)
    dv0 = tl.dot(tl.trans(p00), dy0)
    p01, c01 = _tile_gradients(q0, k1, v1, dy0, lse0, delta0)
    dq0 = tl.dot(c01, k1, dq0)
    _store_tiled_gradient(result, a0, dq0, partial_bias, bias_base, col, 0, collect_bias)
    dk1 = tl.dot(tl.trans(c01), q0)
    dv1 = tl.dot(tl.trans(p01), dy0)

    p10, c10 = _tile_gradients(q1, k0, v0, dy1, lse1, delta1)
    dq1 = tl.dot(c10, k0)
    dk0 = tl.dot(tl.trans(c10), q1, dk0)
    dv0 = tl.dot(tl.trans(p10), dy1, dv0)
    _store_tiled_gradient(result, a0 + heads * 32, dk0, partial_bias, key_bias, col, 0, collect_bias)
    _store_tiled_gradient(result, a0 + heads * 64, dv0, partial_bias, value_bias, col, 0, collect_bias)
    p11, c11 = _tile_gradients(q1, k1, v1, dy1, lse1, delta1)
    dq1 = tl.dot(c11, k1, dq1)
    dk1 = tl.dot(tl.trans(c11), q1, dk1)
    dv1 = tl.dot(tl.trans(p11), dy1, dv1)
    _store_tiled_gradient(result, a1, dq1, partial_bias, bias_base, col, 1, collect_bias)
    _store_tiled_gradient(result, a1 + heads * 32, dk1, partial_bias, key_bias, col, 1, collect_bias)
    _store_tiled_gradient(result, a1 + heads * 64, dv1, partial_bias, value_bias, col, 1, collect_bias)


class _PackedAttention(torch.autograd.Function):
    @staticmethod
    def forward(ctx, packed, heads, shared_backward, tiled_backward, tiled_forward):
        batch = packed.shape[0]
        groups = batch * heads
        tile = 64
        output = torch.empty((batch, TOKEN_COUNT, heads, 32), device=packed.device, dtype=packed.dtype)
        normalizer = torch.empty((groups, TOKEN_COUNT), device=packed.device, dtype=torch.float32)
        if tiled_forward:
            _tiled_forward[(groups,)](packed, output, normalizer, heads, num_warps=4)
        else:
            _forward[(groups, triton.cdiv(TOKEN_COUNT, tile))](packed, output, normalizer, heads, tile, num_warps=4)
        ctx.save_for_backward(packed, output, normalizer)
        ctx.heads = heads
        ctx.shared_backward = shared_backward
        ctx.tiled_backward = tiled_backward
        return output.transpose(1, 2)

    @staticmethod
    def backward(ctx, grad):
        packed, output, normalizer = ctx.saved_tensors
        heads, tile = ctx.heads, 64
        groups = packed.shape[0] * heads
        gradient = torch.empty_like(packed)
        strides = grad.stride()
        if ctx.tiled_backward:
            _tiled_backward[(groups,)](packed, output, grad, normalizer, gradient, heads, *strides,
                                      gradient, False, num_warps=4)
            return gradient, None, None, None, None
        if ctx.shared_backward:
            _shared_backward[(groups,)](packed, output, grad, normalizer, gradient, heads, *strides, num_warps=8)
            return gradient, None, None, None, None
        delta = torch.empty_like(normalizer)
        _row_delta[(groups, TOKEN_COUNT // 32)](output, grad, delta, heads, *strides, num_warps=4)
        for query in (True, False):
            _backward[(groups, triton.cdiv(TOKEN_COUNT, tile))](
                packed, grad, normalizer, delta, gradient, heads, tile, query, *strides, num_warps=4,
            )
        return gradient, None, None, None, None


def packed_attention_forward(layer, inputs, *, shared_backward=False, tiled_backward=False, tiled_forward=False,
                             fuse_qkv_bias_gradient=False):
    if (fuse_qkv_bias_gradient and shared_backward and tiled_backward and tiled_forward
            and inputs.is_cuda and inputs.dtype == torch.bfloat16 and inputs.is_contiguous()
            and inputs.shape == (256, TOKEN_COUNT, 256) and layer.heads == 8):
        return _packed_attention_with_bias_gradient(layer, inputs)
    packed = layer.qkv(inputs)
    if not packed.is_cuda or packed.dtype != torch.bfloat16 or not packed.is_contiguous() or packed.numel() >= 2**31:
        query, key, value = (layer._split_heads(projection) for projection in packed.chunk(3, dim=-1))
        attended = functional.scaled_dot_product_attention(
            query, key, value, dropout_p=0.0, is_causal=False, scale=layer.scale,
        )
    else:
        attended = _PackedAttention.apply(packed, layer.heads, shared_backward, tiled_backward, tiled_forward)
    mixed = attended.transpose(1, 2)
    return layer.output(mixed.reshape(-1, TOKEN_COUNT, layer.heads * 32))


class _ProjectionWithBiasGradient(torch.autograd.Function):
    @staticmethod
    def forward(ctx, inputs, weight, bias, heads):
        packed = functional.linear(inputs, weight, bias)
        batch = inputs.shape[0]
        output = torch.empty((batch, TOKEN_COUNT, heads, 32), device=packed.device, dtype=packed.dtype)
        normalizer = torch.empty((batch * heads, TOKEN_COUNT), device=packed.device, dtype=torch.float32)
        _tiled_forward[(batch * heads,)](packed, output, normalizer, heads, num_warps=4)
        ctx.save_for_backward(inputs, weight, packed, output, normalizer)
        ctx.heads = heads
        return output.reshape(batch, TOKEN_COUNT, heads * 32)

    @staticmethod
    def backward(ctx, grad):
        inputs, weight, packed, output, normalizer = ctx.saved_tensors
        batch, tokens, width = inputs.shape
        heads = ctx.heads
        gradient = torch.empty_like(packed)
        partial_bias = torch.empty((2 * batch, weight.shape[0]), device=packed.device, dtype=torch.float32)
        grad_heads = grad.reshape(batch, tokens, heads, 32).transpose(1, 2)
        _tiled_backward[(batch * heads,)](packed, output, grad_heads, normalizer, gradient, heads,
                                          *grad_heads.stride(), partial_bias, True, num_warps=4)
        matrix = gradient.reshape(-1, weight.shape[0])
        grad_inputs = (matrix @ weight).reshape_as(inputs)
        grad_weight = matrix.mT @ inputs.reshape(-1, width)
        grad_bias = partial_bias.sum(dim=0).to(weight.dtype)
        return grad_inputs, grad_weight, grad_bias, None


def _packed_attention_with_bias_gradient(layer, inputs):
    projection = layer.qkv
    weight = projection.weight.to(inputs.dtype)
    bias = projection.bias.to(inputs.dtype)
    mixed = _ProjectionWithBiasGradient.apply(inputs, weight, bias, layer.heads)
    return layer.output(mixed)
