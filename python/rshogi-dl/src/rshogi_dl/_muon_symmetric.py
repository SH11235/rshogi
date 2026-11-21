"""Compute the symmetric BF16 polynomial in the batched Muon iteration."""

import torch
import triton
import triton.language as tl


@triton.jit
def _symmetric_tiles(
    source, destination, width: tl.constexpr, reduction: tl.constexpr,
    batch_stride: tl.constexpr, row_stride: tl.constexpr, col_stride: tl.constexpr,
    tile: tl.constexpr, chunk: tl.constexpr,
    linear: tl.constexpr, quadratic: tl.constexpr,
):
    triangle = tl.program_id(0)
    matrix = tl.program_id(1)
    col_tile = ((tl.sqrt(1.0 + 8.0 * triangle) - 1.0) * 0.5).to(tl.int32)
    row_tile = triangle - col_tile * (col_tile + 1) // 2
    row = row_tile * tile + tl.arange(0, tile)
    col = col_tile * tile + tl.arange(0, tile)
    start = tl.arange(0, chunk)
    x = source + matrix * batch_stride
    accum = tl.full((tile, tile), 0.0, tl.float32)
    for block in range(reduction // chunk):
        k = block * chunk + start
        a = tl.load(x + row[:, None] * row_stride + k[None, :] * col_stride)
        b = tl.load(x + col[None, :] * row_stride + k[:, None] * col_stride)
        accum = tl.dot(a, b, accum)
    previous = tl.load(x + row[:, None] * row_stride + col[None, :] * col_stride).to(tl.float32)
    accum = tl.fma(accum, quadratic, previous * linear)
    out = destination + matrix * width * width
    tl.store(out + row[:, None] * width + col[None, :], accum)
    if row_tile != col_tile:
        tl.store(out + col[None, :] * width + row[:, None], accum)


def symmetric_polynomial(source, *, linear, quadratic):
    batch, width, reduction = source.shape
    output = torch.empty((batch, width, width), dtype=source.dtype, device=source.device)
    tile = 64
    tiles = width // tile
    _symmetric_tiles[(tiles * (tiles + 1) // 2, batch)](
        source, output, width, reduction, *source.stride(), tile, 32, linear, quadratic,
        num_warps=4, num_stages=3,
    )
    return output
