//! uProf のコールスタックで LayerStacks accumulator のコピー元を区別する。
//!
//! copy 後の異なるタグは同一関数の畳み込みと memcpy への末尾呼び出しを防ぐため。
//! コピーするバイト列・範囲は元の copy_from_slice / clone と同じ。

use super::accumulator_layer_stacks::AccumulatorLayerStacks;

#[inline(never)]
pub(super) fn finny_entry_to_accumulator(dst: &mut [i16], src: &[i16]) {
    dst.copy_from_slice(src);
    std::hint::black_box(1u8);
}

#[inline(never)]
pub(super) fn one_move_prev_to_current(dst: &mut [i16], src: &[i16]) {
    dst.copy_from_slice(src);
    std::hint::black_box(2u8);
}

#[inline(never)]
pub(super) fn forward_source_clone<const L1: usize>(
    src: &AccumulatorLayerStacks<L1>,
) -> AccumulatorLayerStacks<L1> {
    let result = src.clone();
    std::hint::black_box(3u8);
    result
}

#[inline(never)]
pub(super) fn forward_source_to_current(dst: &mut [i16], src: &[i16]) {
    dst.copy_from_slice(src);
    std::hint::black_box(4u8);
}

#[inline(never)]
pub(super) fn null_move_child_copy(dst: &mut [i16], src: &[i16]) {
    dst.copy_from_slice(src);
    std::hint::black_box(5u8);
}
