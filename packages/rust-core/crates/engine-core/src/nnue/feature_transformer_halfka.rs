//! FeatureTransformerHalfKA - HalfKA_hm^用の入力特徴量変換器
//!
//! HalfKA_hm^ 特徴量（キングバケット×BonaPiece）から、
//! 片側 256 次元×両視点 = 512 次元の中間表現を生成する。
//!
//! # モデル形式
//!
//! **coalesced（畳み込み済み）モデル専用**
//!
//! - 入力次元: 73,305 (45キングバケット × 1,629駒入力)
//! - Factorization重みは訓練時にのみ使用
//! - nnue-pytorch serialize.py で自動的にcoalesceされる
//!
//! 未coalesceモデル（74,934次元）はサポートしない。
//! `NetworkHalfKA::read()` で検出し、エラーメッセージを表示する。

use super::accumulator::{
    Accumulator, AccumulatorStack, Aligned, AlignedBox, DirtyPiece, IndexList, MAX_ACTIVE_FEATURES,
};
use super::constants::{
    HALFKA_HM_DIMENSIONS, NUM_REFRESH_TRIGGERS, TRANSFORMED_FEATURE_DIMENSIONS,
};
use super::features::{FeatureSet, HalfKA_hmFeatureSet};
use crate::position::Position;
use crate::types::Color;
use std::io::{self, Read};

/// 特徴インデックスの範囲外アクセス時のパニック
///
/// cold属性により、この関数は分岐予測で「呼ばれない」と判断され、
/// 通常経路の性能に影響しない。
#[cold]
#[inline(never)]
fn feature_index_oob(index: usize, max: usize) -> ! {
    panic!("Feature index out of range: {index} (max: {max})")
}

/// HalfKA_hm^用のFeatureTransformer
#[repr(C, align(64))]
pub struct FeatureTransformerHalfKA {
    /// バイアス [half_dimensions]
    pub biases: Aligned<[i16; TRANSFORMED_FEATURE_DIMENSIONS]>,

    /// 重み [input_dimensions][half_dimensions]
    /// 64バイトアラインメントで確保（aligned load/store用）
    pub weights: AlignedBox<i16>,
}

impl FeatureTransformerHalfKA {
    /// ファイルから読み込み（非圧縮形式）
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i16; TRANSFORMED_FEATURE_DIMENSIONS];
        let mut buf = [0u8; 2];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf)?;
            *bias = i16::from_le_bytes(buf);
        }

        // 重みを読み込み（64バイトアラインメント）
        let weight_size = HALFKA_HM_DIMENSIONS * TRANSFORMED_FEATURE_DIMENSIONS;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf)?;
            *weight = i16::from_le_bytes(buf);
        }

        Ok(Self {
            biases: Aligned(biases),
            weights,
        })
    }

    /// LEB128圧縮形式から読み込み
    pub fn read_leb128<R: Read>(reader: &mut R) -> io::Result<Self> {
        use super::leb128::read_signed_leb128;

        // バイアスを読み込み
        let mut biases = [0i16; TRANSFORMED_FEATURE_DIMENSIONS];
        for bias in biases.iter_mut() {
            let val = read_signed_leb128(reader)?;
            *bias = val as i16;
        }

        // 重みを読み込み（64バイトアラインメント）
        let weight_size = HALFKA_HM_DIMENSIONS * TRANSFORMED_FEATURE_DIMENSIONS;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        for weight in weights.iter_mut() {
            let val = read_signed_leb128(reader)?;
            *weight = val as i16;
        }

        Ok(Self {
            biases: Aligned(biases),
            weights,
        })
    }

    /// 差分計算を使わずにAccumulatorを計算
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut Accumulator) {
        for trigger in 0..NUM_REFRESH_TRIGGERS {
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                let accumulation = acc.get_mut(p, trigger);

                if trigger == 0 {
                    accumulation.copy_from_slice(&self.biases.0);
                } else {
                    accumulation.fill(0);
                }

                let active_indices = self.get_active_features(pos, perspective);
                for &index in active_indices.iter() {
                    self.add_weights(accumulation, index);
                }
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 差分計算でAccumulatorを更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut Accumulator,
        prev_acc: &Accumulator,
    ) {
        for trigger in 0..NUM_REFRESH_TRIGGERS {
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                let reset = HalfKA_hmFeatureSet::needs_refresh(dirty_piece, perspective);

                if reset {
                    let accumulation = acc.get_mut(p, trigger);
                    if trigger == 0 {
                        accumulation.copy_from_slice(&self.biases.0);
                    } else {
                        accumulation.fill(0);
                    }

                    let active_indices = self.get_active_features(pos, perspective);
                    for &index in active_indices.iter() {
                        self.add_weights(accumulation, index);
                    }
                } else {
                    let (removed, added) = HalfKA_hmFeatureSet::collect_changed_indices(
                        dirty_piece,
                        perspective,
                        pos.king_square(perspective),
                    );

                    let prev = prev_acc.get(p, trigger);
                    let curr = acc.get_mut(p, trigger);
                    curr.copy_from_slice(prev);

                    for &index in removed.iter() {
                        self.sub_weights(curr, index);
                    }

                    for &index in added.iter() {
                        self.add_weights(curr, index);
                    }
                }
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStack,
        source_idx: usize,
    ) -> bool {
        let path = stack.collect_path(source_idx);
        if path.is_empty() && stack.current_index() != source_idx {
            return false;
        }

        let source_acc = stack.entry_at(source_idx).accumulator.clone();
        {
            let current_acc = &mut stack.current_mut().accumulator;
            for trigger in 0..NUM_REFRESH_TRIGGERS {
                for perspective in [Color::Black, Color::White] {
                    let p = perspective as usize;
                    current_acc.get_mut(p, trigger).copy_from_slice(source_acc.get(p, trigger));
                }
            }
        }

        for &entry_idx in path.iter() {
            let dirty_piece = stack.entry_at(entry_idx).dirty_piece;

            for trigger in 0..NUM_REFRESH_TRIGGERS {
                for perspective in [Color::Black, Color::White] {
                    debug_assert!(
                        !dirty_piece.king_moved[perspective.index()],
                        "King moved between source and current"
                    );

                    let king_sq = pos.king_square(perspective);
                    let (removed, added) = HalfKA_hmFeatureSet::collect_changed_indices(
                        &dirty_piece,
                        perspective,
                        king_sq,
                    );

                    let p = perspective as usize;
                    let accumulation = stack.current_mut().accumulator.get_mut(p, trigger);

                    for &index in removed.iter() {
                        self.sub_weights(accumulation, index);
                    }
                    for &index in added.iter() {
                        self.add_weights(accumulation, index);
                    }
                }
            }
        }

        stack.current_mut().accumulator.computed_accumulation = true;
        stack.current_mut().accumulator.computed_score = false;
        true
    }

    /// アクティブな特徴量のインデックスリストを取得
    #[inline]
    fn get_active_features(
        &self,
        pos: &Position,
        perspective: Color,
    ) -> IndexList<MAX_ACTIVE_FEATURES> {
        HalfKA_hmFeatureSet::collect_active_indices(pos, perspective)
    }

    /// 重みを累積値に加算
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS], index: usize) {
        let offset = index * TRANSFORMED_FEATURE_DIMENSIONS;
        // OOBは即座にパニック（debug/release両方で検知）
        // 通常経路では分岐予測が当たり、性能影響はほぼゼロ
        if offset + TRANSFORMED_FEATURE_DIMENSIONS > self.weights.len() {
            feature_index_oob(index, self.weights.len() / TRANSFORMED_FEATURE_DIMENSIONS);
        }

        let weights = &self.weights[offset..offset + TRANSFORMED_FEATURE_DIMENSIONS];

        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..16 {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..32 {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..32 {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let weight_vec = v128_load(weight_ptr.add(i * 8) as *const v128);
                    let result = i16x8_add(acc_vec, weight_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_add(weight);
        }
    }

    /// 重みを累積値から減算
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS], index: usize) {
        let offset = index * TRANSFORMED_FEATURE_DIMENSIONS;
        // OOBは即座にパニック（debug/release両方で検知）
        // 通常経路では分岐予測が当たり、性能影響はほぼゼロ
        if offset + TRANSFORMED_FEATURE_DIMENSIONS > self.weights.len() {
            feature_index_oob(index, self.weights.len() / TRANSFORMED_FEATURE_DIMENSIONS);
        }

        let weights = &self.weights[offset..offset + TRANSFORMED_FEATURE_DIMENSIONS];

        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..16 {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_sub_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..32 {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_sub_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..32 {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let weight_vec = v128_load(weight_ptr.add(i * 8) as *const v128);
                    let result = i16x8_sub(acc_vec, weight_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_sub(weight);
        }
    }

    /// Accumulatorの値を変換して出力
    pub fn transform(
        &self,
        acc: &Accumulator,
        side_to_move: Color,
        output: &mut [u8; TRANSFORMED_FEATURE_DIMENSIONS * 2],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        for (p, &perspective) in perspectives.iter().enumerate() {
            let out_offset = TRANSFORMED_FEATURE_DIMENSIONS * p;
            let accumulation = acc.get(perspective as usize, 0);

            #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
            {
                unsafe {
                    use std::arch::x86_64::*;
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output[out_offset..].as_mut_ptr();
                    let zero = _mm256_setzero_si256();
                    let max_val = _mm256_set1_epi8(127);

                    for i in 0..8 {
                        let v0 = _mm256_loadu_si256(acc_ptr.add(i * 32) as *const __m256i);
                        let v1 = _mm256_loadu_si256(acc_ptr.add(i * 32 + 16) as *const __m256i);

                        let packed = _mm256_packs_epi16(v0, v1);
                        let packed = _mm256_permute4x64_epi64(packed, 0xD8);

                        let clipped = _mm256_max_epi8(packed, zero);
                        let clipped = _mm256_min_epi8(clipped, max_val);

                        _mm256_storeu_si256(out_ptr.add(i * 32) as *mut __m256i, clipped);
                    }
                }
                continue;
            }

            #[cfg(all(
                target_arch = "x86_64",
                target_feature = "sse2",
                not(target_feature = "avx2")
            ))]
            {
                unsafe {
                    use std::arch::x86_64::*;
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output[out_offset..].as_mut_ptr();
                    let zero = _mm_setzero_si128();
                    let max_val = _mm_set1_epi8(127);

                    for i in 0..16 {
                        let v0 = _mm_loadu_si128(acc_ptr.add(i * 16) as *const __m128i);
                        let v1 = _mm_loadu_si128(acc_ptr.add(i * 16 + 8) as *const __m128i);

                        let packed = _mm_packs_epi16(v0, v1);

                        let offset_128 = _mm_set1_epi8(-128i8);
                        let packed_unsigned = _mm_add_epi8(packed, offset_128);
                        let zero_unsigned = _mm_add_epi8(zero, offset_128);
                        let max_unsigned = _mm_add_epi8(max_val, offset_128);
                        let clipped = _mm_max_epu8(packed_unsigned, zero_unsigned);
                        let clipped = _mm_min_epu8(clipped, max_unsigned);
                        let clipped = _mm_sub_epi8(clipped, offset_128);

                        _mm_storeu_si128(out_ptr.add(i * 16) as *mut __m128i, clipped);
                    }
                }
                continue;
            }

            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            {
                unsafe {
                    use std::arch::wasm32::*;
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output[out_offset..].as_mut_ptr();

                    for i in 0..16 {
                        let v0 = v128_load(acc_ptr.add(i * 16) as *const v128);
                        let v1 = v128_load(acc_ptr.add(i * 16 + 8) as *const v128);

                        let packed = i8x16_narrow_i16x8(v0, v1);

                        let zero = i8x16_splat(0);
                        let max_val = i8x16_splat(127);
                        let clipped = i8x16_max(packed, zero);
                        let clipped = i8x16_min(clipped, max_val);

                        v128_store(out_ptr.add(i * 16) as *mut v128, clipped);
                    }
                }
                continue;
            }

            #[allow(unreachable_code)]
            for i in 0..TRANSFORMED_FEATURE_DIMENSIONS {
                output[out_offset + i] = accumulation[i].clamp(0, 127) as u8;
            }
        }
    }
}
