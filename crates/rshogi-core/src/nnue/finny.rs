//! Finny cache の駒スロット差分と、2 出力への tile 更新。

use super::accumulator::IndexList;
use super::bona_piece::BonaPiece;
use super::piece_list::PieceNumber;

/// HalfKX 探索で Finny cache を使う最小 FT 幅。
pub(crate) const HALFKX_FINNY_MIN_L1: usize = 0;

/// 探索時の確保と const generics の FT 経路で同じ閾値を使う。
#[inline]
pub(crate) fn halfkx_finny_enabled(l1: usize) -> bool {
    (HALFKX_FINNY_MIN_L1..).contains(&l1)
}

/// 異なる駒スロットを下位から順に表すビットマスク。
#[inline]
pub(super) fn piece_list_diff_mask(
    cached: &[BonaPiece; PieceNumber::NB],
    current: &[BonaPiece; PieceNumber::NB],
) -> u64 {
    const { assert!(PieceNumber::NB == 40) };
    #[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
    {
        // SAFETY: BonaPiece は repr(transparent) の u16 で、両配列は40要素ある。
        // 先頭32要素と末尾8要素だけを非整列loadで読み、配列外にはアクセスしない。
        // 命令に必要なAVX-512BW/VLは各cfgで保証する。
        unsafe {
            use std::arch::x86_64::*;
            let lo = _mm512_cmpneq_epi16_mask(
                _mm512_loadu_si512(cached.as_ptr().cast()),
                _mm512_loadu_si512(current.as_ptr().cast()),
            );
            let cached_hi = _mm_loadu_si128(cached.as_ptr().add(32).cast());
            let current_hi = _mm_loadu_si128(current.as_ptr().add(32).cast());
            #[cfg(target_feature = "avx512vl")]
            let hi = _mm_cmpneq_epi16_mask(cached_hi, current_hi);
            #[cfg(not(target_feature = "avx512vl"))]
            let hi = {
                let equal = _mm_cmpeq_epi16(cached_hi, current_hi);
                !(_mm_movemask_epi8(_mm_packs_epi16(equal, equal)) as u8)
            };
            u64::from(lo) | (u64::from(hi) << 32)
        }
    }
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        not(target_feature = "avx512bw")
    ))]
    {
        // SAFETY: cfg が AVX2 を保証し、repr(transparent) の u16 配列を
        // 16 + 16 + 8 要素ずつ非整列loadする。全loadが40要素の範囲内に収まる。
        unsafe {
            use std::arch::x86_64::*;
            let lo = _mm256_cmpeq_epi16(
                _mm256_loadu_si256(cached.as_ptr().cast()),
                _mm256_loadu_si256(current.as_ptr().cast()),
            );
            let mid = _mm256_cmpeq_epi16(
                _mm256_loadu_si256(cached.as_ptr().add(16).cast()),
                _mm256_loadu_si256(current.as_ptr().add(16).cast()),
            );
            // pack後の128-bit lane順を戻し、1スロットにつき1bitにする。
            let packed = _mm256_permute4x64_epi64::<0xd8>(_mm256_packs_epi16(lo, mid));
            let lo_mask = !(_mm256_movemask_epi8(packed) as u32);
            let hi = _mm_cmpeq_epi16(
                _mm_loadu_si128(cached.as_ptr().add(32).cast()),
                _mm_loadu_si128(current.as_ptr().add(32).cast()),
            );
            let hi = _mm_movemask_epi8(_mm_packs_epi16(hi, hi)) as u8;
            u64::from(lo_mask) | (u64::from(!hi) << 32)
        }
    }
    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_feature = "avx2", target_feature = "avx512bw")
    )))]
    {
        cached
            .iter()
            .zip(current)
            .enumerate()
            .fold(0, |mask, (slot, (old, new))| mask | (u64::from(old != new) << slot))
    }
}

/// 差分スロットを昇順に列挙し、ZEROを除いた特徴量indexを集める。
///
/// 呼び出し元は `removed` と `added` を空にしておく必要がある。
#[inline]
pub(super) fn collect_piece_list_diff<FI: Fn(BonaPiece) -> usize>(
    cached: &[BonaPiece; PieceNumber::NB],
    current: &[BonaPiece; PieceNumber::NB],
    idx_fn: FI,
    removed: &mut IndexList<{ PieceNumber::NB }>,
    added: &mut IndexList<{ PieceNumber::NB }>,
) {
    debug_assert!(removed.is_empty() && added.is_empty());
    let removed_ptr = removed.as_mut_ptr();
    let added_ptr = added.as_mut_ptr();
    let mut removed_len = 0;
    let mut added_len = 0;
    let mut mask = piece_list_diff_mask(cached, current);
    while mask != 0 {
        let slot = mask.trailing_zeros() as usize;
        mask &= mask - 1;
        let cached_bp = cached[slot];
        let current_bp = current[slot];
        if cached_bp != BonaPiece::ZERO {
            let index = idx_fn(cached_bp);
            debug_assert!(u32::try_from(index).is_ok());
            // SAFETY: mask は40スロットだけを含み、各スロットから最大1件を書く。
            // removed_len < PieceNumber::NB を保ち、最終リストの領域内だけへ書く。
            unsafe { removed_ptr.add(removed_len).write(index as u32) };
            removed_len += 1;
        }
        if current_bp != BonaPiece::ZERO {
            let index = idx_fn(current_bp);
            debug_assert!(u32::try_from(index).is_ok());
            // SAFETY: removed と同様、各スロットから最大1件、容量40の範囲内へ書く。
            unsafe { added_ptr.add(added_len).write(index as u32) };
            added_len += 1;
        }
    }
    // SAFETY: 両リストの先頭 len 要素は上の走査で初期化済みで、len <= 40。
    unsafe {
        removed.set_len(removed_len);
        added.set_len(added_len);
    }
}

/// bias または cache を読み、全差分を tile 内で適用して両出力へ書く。
///
/// slice の長さを検証し、非整列命令で Vec の bias と任意の出力配置にも対応する。
/// i16 の加減算は全 backend で wrapping とする。
#[inline]
pub(super) fn apply_weight_changes_to_two<const L1: usize>(
    cache: &mut [i16],
    biases: Option<&[i16]>,
    accumulation: &mut [i16],
    weights: &[i16],
    removed: &IndexList<{ PieceNumber::NB }>,
    added: &IndexList<{ PieceNumber::NB }>,
) {
    assert_eq!(cache.len(), L1);
    assert_eq!(accumulation.len(), L1);
    if let Some(biases) = biases {
        assert_eq!(biases.len(), L1);
    }

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    if L1.is_multiple_of(32) {
        use std::arch::x86_64::*;
        // 幅が tile に満たない場合も SIMD で処理する。
        let tile_regs = if L1.is_multiple_of(32 * 8) { 8 } else { 1 };
        let cache_ptr = cache.as_mut_ptr();
        let output_ptr = accumulation.as_mut_ptr();
        let source = biases.map_or(cache_ptr.cast_const(), |bias| bias.as_ptr());
        // SAFETY: cfg が命令セットを保証する。入出力の長さは L1 と検証済みで、
        // L1 は tile 幅の倍数。重み行は slice で境界を検証してから読む。
        // 非整列 load/store を用い、各ポインタは該当 tile の範囲内のみ参照する。
        // source が cache を指す場合も tile 全体を読み終えてから同じ範囲へ書く。
        unsafe {
            for offset in (0..L1).step_by(32 * tile_regs) {
                let mut tile = [_mm512_setzero_si512(); 8];
                for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                    *value = _mm512_loadu_si512(source.add(offset + k * 32).cast());
                }
                for index in removed.iter() {
                    let row = &weights[index * L1..][..L1];
                    for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                        *value = _mm512_sub_epi16(
                            *value,
                            _mm512_loadu_si512(row.as_ptr().add(offset + k * 32).cast()),
                        );
                    }
                }
                for index in added.iter() {
                    let row = &weights[index * L1..][..L1];
                    for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                        *value = _mm512_add_epi16(
                            *value,
                            _mm512_loadu_si512(row.as_ptr().add(offset + k * 32).cast()),
                        );
                    }
                }
                for (k, &value) in tile[..tile_regs].iter().enumerate() {
                    _mm512_storeu_si512(cache_ptr.add(offset + k * 32).cast(), value);
                    _mm512_storeu_si512(output_ptr.add(offset + k * 32).cast(), value);
                }
            }
        }
        return;
    }

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        not(target_feature = "avx512bw")
    ))]
    if L1.is_multiple_of(16) {
        use std::arch::x86_64::*;
        // 幅が tile に満たない場合も SIMD で処理する。
        let tile_regs = if L1.is_multiple_of(16 * 8) { 8 } else { 1 };
        let cache_ptr = cache.as_mut_ptr();
        let output_ptr = accumulation.as_mut_ptr();
        let source = biases.map_or(cache_ptr.cast_const(), |bias| bias.as_ptr());
        // SAFETY: cfg が命令セットを保証する。入出力の長さは L1 と検証済みで、
        // L1 は tile 幅の倍数。重み行は slice で境界を検証してから読む。
        // 非整列 load/store を用い、各ポインタは該当 tile の範囲内のみ参照する。
        // source が cache を指す場合も tile 全体を読み終えてから同じ範囲へ書く。
        unsafe {
            for offset in (0..L1).step_by(16 * tile_regs) {
                let mut tile = [_mm256_setzero_si256(); 8];
                for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                    *value = _mm256_loadu_si256(source.add(offset + k * 16).cast());
                }
                for index in removed.iter() {
                    let row = &weights[index * L1..][..L1];
                    for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                        *value = _mm256_sub_epi16(
                            *value,
                            _mm256_loadu_si256(row.as_ptr().add(offset + k * 16).cast()),
                        );
                    }
                }
                for index in added.iter() {
                    let row = &weights[index * L1..][..L1];
                    for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                        *value = _mm256_add_epi16(
                            *value,
                            _mm256_loadu_si256(row.as_ptr().add(offset + k * 16).cast()),
                        );
                    }
                }
                for (k, &value) in tile[..tile_regs].iter().enumerate() {
                    _mm256_storeu_si256(cache_ptr.add(offset + k * 16).cast(), value);
                    _mm256_storeu_si256(output_ptr.add(offset + k * 16).cast(), value);
                }
            }
        }
        return;
    }

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "sse2",
        not(any(target_feature = "avx2", target_feature = "avx512bw"))
    ))]
    if L1.is_multiple_of(8) {
        use std::arch::x86_64::*;
        // 幅が tile に満たない場合も SIMD で処理する。
        let tile_regs = if L1.is_multiple_of(8 * 8) { 8 } else { 1 };
        let cache_ptr = cache.as_mut_ptr();
        let output_ptr = accumulation.as_mut_ptr();
        let source = biases.map_or(cache_ptr.cast_const(), |bias| bias.as_ptr());
        // SAFETY: cfg が命令セットを保証する。入出力の長さは L1 と検証済みで、
        // L1 は tile 幅の倍数。重み行は slice で境界を検証してから読む。
        // 非整列 load/store を用い、各ポインタは該当 tile の範囲内のみ参照する。
        // source が cache を指す場合も tile 全体を読み終えてから同じ範囲へ書く。
        unsafe {
            for offset in (0..L1).step_by(8 * tile_regs) {
                let mut tile = [_mm_setzero_si128(); 8];
                for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                    *value = _mm_loadu_si128(source.add(offset + k * 8).cast());
                }
                for index in removed.iter() {
                    let row = &weights[index * L1..][..L1];
                    for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                        *value = _mm_sub_epi16(
                            *value,
                            _mm_loadu_si128(row.as_ptr().add(offset + k * 8).cast()),
                        );
                    }
                }
                for index in added.iter() {
                    let row = &weights[index * L1..][..L1];
                    for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                        *value = _mm_add_epi16(
                            *value,
                            _mm_loadu_si128(row.as_ptr().add(offset + k * 8).cast()),
                        );
                    }
                }
                for (k, &value) in tile[..tile_regs].iter().enumerate() {
                    _mm_storeu_si128(cache_ptr.add(offset + k * 8).cast(), value);
                    _mm_storeu_si128(output_ptr.add(offset + k * 8).cast(), value);
                }
            }
        }
        return;
    }

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    if L1.is_multiple_of(8) {
        use std::arch::wasm32::*;
        // 幅が tile に満たない場合も SIMD で処理する。
        let tile_regs = if L1.is_multiple_of(8 * 8) { 8 } else { 1 };
        let cache_ptr = cache.as_mut_ptr();
        let output_ptr = accumulation.as_mut_ptr();
        let source = biases.map_or(cache_ptr.cast_const(), |bias| bias.as_ptr());
        // SAFETY: cfg が命令セットを保証する。入出力の長さは L1 と検証済みで、
        // L1 は tile 幅の倍数。重み行は slice で境界を検証してから読む。
        // 非整列 load/store を用い、各ポインタは該当 tile の範囲内のみ参照する。
        // source が cache を指す場合も tile 全体を読み終えてから同じ範囲へ書く。
        unsafe {
            for offset in (0..L1).step_by(8 * tile_regs) {
                let mut tile = [i16x8_splat(0); 8];
                for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                    *value = v128_load(source.add(offset + k * 8).cast());
                }
                for index in removed.iter() {
                    let row = &weights[index * L1..][..L1];
                    for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                        *value =
                            i16x8_sub(*value, v128_load(row.as_ptr().add(offset + k * 8).cast()));
                    }
                }
                for index in added.iter() {
                    let row = &weights[index * L1..][..L1];
                    for (k, value) in tile[..tile_regs].iter_mut().enumerate() {
                        *value =
                            i16x8_add(*value, v128_load(row.as_ptr().add(offset + k * 8).cast()));
                    }
                }
                for (k, &value) in tile[..tile_regs].iter().enumerate() {
                    v128_store(cache_ptr.add(offset + k * 8).cast(), value);
                    v128_store(output_ptr.add(offset + k * 8).cast(), value);
                }
            }
        }
        return;
    }

    for i in 0..L1 {
        let mut value = biases.map_or(cache[i], |bias| bias[i]);
        for index in removed.iter() {
            value = value.wrapping_sub(weights[index * L1 + i]);
        }
        for index in added.iter() {
            value = value.wrapping_add(weights[index * L1 + i]);
        }
        cache[i] = value;
        accumulation[i] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngCore, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    fn check_tiles<const L1: usize>() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x717e);
        // 1 要素ずらし、SIMD の整列を仮定しない API 契約も検証する。
        let weights: Vec<_> = (0..L1 * 40 + 1).map(|_| rng.next_u32() as i16).collect();
        let biases: Vec<_> = (0..L1 + 1).map(|_| rng.next_u32() as i16).collect();
        let initial: Vec<_> = (0..L1 + 1).map(|_| rng.next_u32() as i16).collect();
        for (sub, add) in [(0, 0), (0, 40), (40, 0), (40, 40), (1, 2), (2, 1)] {
            let mut removed = IndexList::new();
            let mut added = IndexList::new();
            for i in 0..sub {
                assert!(removed.push(i));
            }
            for i in 0..add {
                assert!(added.push(39 - i));
            }
            for cold in [false, true] {
                let mut cache = initial.clone();
                let mut output = vec![0; L1 + 1];
                let mut expected = if cold {
                    biases[1..].to_vec()
                } else {
                    initial[1..].to_vec()
                };
                for index in removed.iter() {
                    for (value, &weight) in
                        expected.iter_mut().zip(&weights[1 + index * L1..][..L1])
                    {
                        *value = value.wrapping_sub(weight);
                    }
                }
                for index in added.iter() {
                    for (value, &weight) in
                        expected.iter_mut().zip(&weights[1 + index * L1..][..L1])
                    {
                        *value = value.wrapping_add(weight);
                    }
                }
                apply_weight_changes_to_two::<L1>(
                    &mut cache[1..],
                    cold.then_some(&biases[1..]),
                    &mut output[1..],
                    &weights[1..],
                    &removed,
                    &added,
                );
                assert_eq!(&cache[1..], expected);
                assert_eq!(&output[1..], expected);
                assert_eq!(cache[0], initial[0]);
                assert_eq!(output[0], 0);
            }
        }
    }

    #[test]
    fn finny_two_outputs_match_sequential_wrapping_all_tile_sizes() {
        check_tiles::<7>();
        check_tiles::<32>();
        check_tiles::<96>();
        check_tiles::<256>();
        check_tiles::<512>();
        check_tiles::<768>();
        check_tiles::<1024>();
        check_tiles::<1536>();
    }
}
