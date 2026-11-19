//! AVX-512 の駒 FT 行カーネル。各 i8 を符号拡張し、係数を 2 倍して累積する。

use std::arch::x86_64::*;

use super::accumulator::IndexList;
use super::piece_list::PieceNumber;

#[inline(always)]
fn row(weights: &[i8], width: usize, index: usize) -> &[i8] {
    let offset = index.checked_mul(width).expect("FT row offset overflow");
    &weights[offset..][..width]
}

#[inline(always)]
pub(super) fn update<const R: usize, const A: usize>(
    weights: &[i8],
    source: Option<&[i16]>,
    output: &mut [i16],
    removed: [usize; R],
    added: [usize; A],
) {
    let width = output.len();
    let removed = removed.map(|index| row(weights, width, index).as_ptr());
    let added = added.map(|index| row(weights, width, index).as_ptr());
    apply_tiles::<1>(source, output, None, &removed, &added);
}

#[inline]
pub(super) fn refresh<const W: usize>(
    cache: &mut [i16; W],
    biases: Option<&[i16; W]>,
    output: &mut [i16; W],
    weights: &[i8],
    removed: &IndexList<{ PieceNumber::NB }>,
    added: &IndexList<{ PieceNumber::NB }>,
) {
    let pointers = |indices: &IndexList<{ PieceNumber::NB }>| {
        let mut rows = [std::ptr::null(); PieceNumber::NB];
        for (slot, index) in rows.iter_mut().zip(indices.iter()) {
            *slot = row(weights, W, index).as_ptr();
        }
        rows
    };
    let sub = pointers(removed);
    let add = pointers(added);
    let removed = &sub[..removed.len()];
    let added = &add[..added.len()];
    let biases = biases.map(|row| row.as_slice());
    if W.is_multiple_of(32 * 8) {
        apply_tiles::<8>(biases, cache, Some(output), removed, added);
    } else {
        apply_tiles::<1>(biases, cache, Some(output), removed, added);
    }
}

#[inline(always)]
fn apply_tiles<const REGS: usize>(
    source: Option<&[i16]>,
    output: &mut [i16],
    second: Option<&mut [i16]>,
    removed: &[*const i8],
    added: &[*const i8],
) {
    let width = output.len();
    assert!(width.is_multiple_of(32 * REGS));
    assert!(source.is_none_or(|row| row.len() == width));
    assert!(second.as_ref().is_none_or(|row| row.len() == width));
    let output = output.as_mut_ptr();
    let source = source.map_or(output.cast_const(), |s| s.as_ptr());
    let second = second.map(|s| s.as_mut_ptr());
    // SAFETY: cfg が AVX-512F/BW を保証する。呼び出し元は各ポインタを幅分の
    // 検証済みスライスから作る。入出力は長さを検証し、非整列 load/store を使う。
    // source が output と同じ場合も tile 内を読み終えてから上書きする。
    unsafe {
        for offset in (0..width).step_by(32 * REGS) {
            let mut delta = [_mm512_setzero_si512(); REGS];
            for &row in removed {
                for (lane, value) in delta.iter_mut().enumerate() {
                    let weight = _mm512_cvtepi8_epi16(_mm256_loadu_si256(
                        row.add(offset + lane * 32).cast(),
                    ));
                    *value = _mm512_sub_epi16(*value, weight);
                }
            }
            for &row in added {
                for (lane, value) in delta.iter_mut().enumerate() {
                    let weight = _mm512_cvtepi8_epi16(_mm256_loadu_si256(
                        row.add(offset + lane * 32).cast(),
                    ));
                    *value = _mm512_add_epi16(*value, weight);
                }
            }
            // i16 の wrapping 演算では、差分をまとめてから倍にしても同じ結果になる。
            for (lane, delta) in delta.into_iter().enumerate() {
                let column = offset + lane * 32;
                let base = _mm512_loadu_si512(source.add(column).cast());
                let result = _mm512_add_epi16(base, _mm512_slli_epi16::<1>(delta));
                _mm512_storeu_si512(output.add(column).cast(), result);
                if let Some(second) = second {
                    _mm512_storeu_si512(second.add(column).cast(), result);
                }
            }
        }
    }
}
