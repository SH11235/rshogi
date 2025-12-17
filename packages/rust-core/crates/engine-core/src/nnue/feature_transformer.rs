//! FeatureTransformer - 入力特徴量を変換する最初の層
//!
//! HalfKP 特徴量（自玉×BonaPiece）の活性なインデックス集合から、
//! 片側 256 次元×両視点 = 512 次元の中間表現を生成する。
//! 盤上駒および手駒を特徴量として扱い、`DirtyPiece` に基づく差分更新にも対応する。

use super::accumulator::{Accumulator, Aligned};
use super::constants::{HALFKP_DIMENSIONS, TRANSFORMED_FEATURE_DIMENSIONS};
use super::get_changed_features;
use crate::position::Position;
use crate::types::Color;
use std::io::{self, Read};

/// FeatureTransformerのパラメータ
#[repr(C, align(64))]
pub struct FeatureTransformer {
    /// バイアス [half_dimensions]
    pub biases: Aligned<[i16; TRANSFORMED_FEATURE_DIMENSIONS]>,

    /// 重み [input_dimensions][half_dimensions]
    /// 大きいのでBox化
    pub weights: Box<[i16]>,
}

impl FeatureTransformer {
    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i16; TRANSFORMED_FEATURE_DIMENSIONS];
        let mut buf = [0u8; 2];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf)?;
            *bias = i16::from_le_bytes(buf);
        }

        // 重みを読み込み
        let weight_size = HALFKP_DIMENSIONS * TRANSFORMED_FEATURE_DIMENSIONS;
        let mut weights = vec![0i16; weight_size].into_boxed_slice();
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf)?;
            *weight = i16::from_le_bytes(buf);
        }

        Ok(Self {
            biases: Aligned(biases),
            weights,
        })
    }

    /// 差分計算を使わずにAccumulatorを計算
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut Accumulator) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = acc.get_mut(p);

            // バイアスで初期化
            accumulation.copy_from_slice(&self.biases.0);

            // アクティブな特徴量の重みを加算
            let active_indices = self.get_active_features(pos, perspective);
            for index in active_indices {
                self.add_weights(accumulation, index);
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 差分計算でAccumulatorを更新
    ///
    /// 戻り値: 差分更新が成功したらtrue、全計算が必要ならfalse。
    pub fn update_accumulator(
        &self,
        pos: &Position,
        acc: &mut Accumulator,
        prev_acc: &Accumulator,
    ) -> bool {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;

            let (removed, added) = get_changed_features(pos, perspective);

            // 差分が取れない場合は全計算が必要
            if removed.is_empty() && added.is_empty() {
                return false;
            }

            // 前の値をコピー
            let prev = prev_acc.get(p);
            let curr = acc.get_mut(p);
            curr.copy_from_slice(prev);

            // 削除された特徴量の重みを減算
            for index in removed {
                self.sub_weights(curr, index);
            }

            // 追加された特徴量の重みを加算
            for index in added {
                self.add_weights(curr, index);
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
        true
    }

    /// アクティブな特徴量のインデックスリストを取得
    ///
    /// 盤上駒および手駒を HalfKP 特徴量に写像する。
    fn get_active_features(&self, pos: &Position, perspective: Color) -> Vec<usize> {
        let king_sq = pos.king_square(perspective);
        let mut features = Vec::with_capacity(38); // 最大38駒（玉2つ以外）

        // 盤上の駒
        for sq in pos.occupied().iter() {
            let pc = pos.piece_on(sq);
            if pc.is_none() {
                continue;
            }
            // 玉は特徴量に含めない
            if pc.piece_type() == crate::types::PieceType::King {
                continue;
            }

            let bp = super::bona_piece::BonaPiece::from_piece_square(pc, sq, perspective);
            if bp != super::bona_piece::BonaPiece::ZERO {
                let index = super::bona_piece::halfkp_index(king_sq, bp);
                features.push(index);
            }
        }

        // 手駒の特徴量
        for owner in [Color::Black, Color::White] {
            for pt in [
                crate::types::PieceType::Pawn,
                crate::types::PieceType::Lance,
                crate::types::PieceType::Knight,
                crate::types::PieceType::Silver,
                crate::types::PieceType::Gold,
                crate::types::PieceType::Bishop,
                crate::types::PieceType::Rook,
            ] {
                let count = pos.hand(owner).count(pt) as u8;
                if count == 0 {
                    continue;
                }
                let bp =
                    super::bona_piece::BonaPiece::from_hand_piece(perspective, owner, pt, count);
                if bp != super::bona_piece::BonaPiece::ZERO {
                    let index = super::bona_piece::halfkp_index(king_sq, bp);
                    features.push(index);
                }
            }
        }

        features
    }

    /// 重みを累積値に加算
    ///
    /// AVX2/SSE2/WASMのSIMD最適化版。256要素を一度に処理。
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS], index: usize) {
        let offset = index * TRANSFORMED_FEATURE_DIMENSIONS;
        if offset + TRANSFORMED_FEATURE_DIMENSIONS > self.weights.len() {
            return; // 範囲外チェック
        }

        let weights = &self.weights[offset..offset + TRANSFORMED_FEATURE_DIMENSIONS];

        // AVX2: 256bit = 16 x i16
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // SAFETY:
            // - accumulation は 64バイトアライメント保証（Aligned<T>経由）
            // - weights は上の境界チェック済み
            // - 256要素 = 16要素 × 16回のループで完全にカバー
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..16 {
                    let acc_vec = _mm256_loadu_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_loadu_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_adds_epi16(acc_vec, weight_vec);
                    _mm256_storeu_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        // SSE2: 128bit = 8 x i16
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY: 同上
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..32 {
                    let acc_vec = _mm_loadu_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_loadu_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_adds_epi16(acc_vec, weight_vec);
                    _mm_storeu_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // WASM SIMD128: 128bit = 8 x i16
        #[cfg(target_arch = "wasm32")]
        {
            // SAFETY: 同上
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..32 {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let weight_vec = v128_load(weight_ptr.add(i * 8) as *const v128);
                    let result = i16x8_add_sat(acc_vec, weight_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        // スカラーフォールバック
        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.saturating_add(weight);
        }
    }

    /// 重みを累積値から減算
    ///
    /// AVX2/SSE2/WASMのSIMD最適化版。256要素を一度に処理。
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS], index: usize) {
        let offset = index * TRANSFORMED_FEATURE_DIMENSIONS;
        if offset + TRANSFORMED_FEATURE_DIMENSIONS > self.weights.len() {
            return;
        }

        let weights = &self.weights[offset..offset + TRANSFORMED_FEATURE_DIMENSIONS];

        // AVX2: 256bit = 16 x i16
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // SAFETY:
            // - accumulation は 64バイトアライメント保証（Aligned<T>経由）
            // - weights は上の境界チェック済み
            // - 256要素 = 16要素 × 16回のループで完全にカバー
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..16 {
                    let acc_vec = _mm256_loadu_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_loadu_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_subs_epi16(acc_vec, weight_vec);
                    _mm256_storeu_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        // SSE2: 128bit = 8 x i16
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY: 同上
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..32 {
                    let acc_vec = _mm_loadu_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_loadu_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_subs_epi16(acc_vec, weight_vec);
                    _mm_storeu_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // WASM SIMD128: 128bit = 8 x i16
        #[cfg(target_arch = "wasm32")]
        {
            // SAFETY: 同上
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..32 {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let weight_vec = v128_load(weight_ptr.add(i * 8) as *const v128);
                    let result = i16x8_sub_sat(acc_vec, weight_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        // スカラーフォールバック
        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.saturating_sub(weight);
        }
    }

    /// Accumulatorの値を変換して出力
    /// ClippedReLU(clamp(0, 127))を適用し、両視点を結合
    ///
    /// AVX2/SSE2/WASMのSIMD最適化版。
    pub fn transform(
        &self,
        acc: &Accumulator,
        side_to_move: Color,
        output: &mut [u8; TRANSFORMED_FEATURE_DIMENSIONS * 2],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        for (p, &perspective) in perspectives.iter().enumerate() {
            let out_offset = TRANSFORMED_FEATURE_DIMENSIONS * p;
            let accumulation = acc.get(perspective as usize);

            // AVX2: 256bit = 16 x i16 → 32 x i8（パック後）
            #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
            {
                // SAFETY:
                // - accumulation は 64バイトアライメント保証
                // - 256要素 = 32要素 × 8回のループで完全にカバー
                unsafe {
                    use std::arch::x86_64::*;
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output[out_offset..].as_mut_ptr();
                    let zero = _mm256_setzero_si256();
                    let max_val = _mm256_set1_epi8(127);

                    for i in 0..8 {
                        // 32個のi16を読み込み（2つの__m256i）
                        let v0 = _mm256_loadu_si256(acc_ptr.add(i * 32) as *const __m256i);
                        let v1 = _mm256_loadu_si256(acc_ptr.add(i * 32 + 16) as *const __m256i);

                        // i16 → i8 にパック（符号付き飽和）
                        // 結果のレーン順序: [v0_lo, v1_lo, v0_hi, v1_hi]
                        let packed = _mm256_packs_epi16(v0, v1);
                        // レーン順序を修正: 0xD8 = 11_01_10_00 → [0, 2, 1, 3]
                        let packed = _mm256_permute4x64_epi64(packed, 0xD8);

                        // ClippedReLU: max(0, min(127, x))
                        let clipped = _mm256_max_epi8(packed, zero);
                        let clipped = _mm256_min_epi8(clipped, max_val);

                        _mm256_storeu_si256(out_ptr.add(i * 32) as *mut __m256i, clipped);
                    }
                }
                continue;
            }

            // SSE2: 128bit = 8 x i16 → 16 x i8（パック後）
            #[cfg(all(
                target_arch = "x86_64",
                target_feature = "sse2",
                not(target_feature = "avx2")
            ))]
            {
                // SAFETY: 同上
                unsafe {
                    use std::arch::x86_64::*;
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output[out_offset..].as_mut_ptr();
                    let zero = _mm_setzero_si128();
                    let max_val = _mm_set1_epi8(127);

                    for i in 0..16 {
                        // 16個のi16を読み込み（2つの__m128i）
                        let v0 = _mm_loadu_si128(acc_ptr.add(i * 16) as *const __m128i);
                        let v1 = _mm_loadu_si128(acc_ptr.add(i * 16 + 8) as *const __m128i);

                        // i16 → i8 にパック（符号付き飽和）
                        let packed = _mm_packs_epi16(v0, v1);

                        // ClippedReLU: max(0, min(127, x))
                        // SSE2には_mm_max_epi8がないので、別の方法を使う
                        // 方法: 0x80を加算して符号なしmax/minを使い、戻す
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

            // WASM SIMD128: 128bit = 8 x i16 → 16 x i8
            #[cfg(target_arch = "wasm32")]
            {
                // SAFETY: 同上
                unsafe {
                    use std::arch::wasm32::*;
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output[out_offset..].as_mut_ptr();

                    for i in 0..16 {
                        // 16個のi16を読み込み（2つのv128）
                        let v0 = v128_load(acc_ptr.add(i * 16) as *const v128);
                        let v1 = v128_load(acc_ptr.add(i * 16 + 8) as *const v128);

                        // i16 → i8 にパック（符号付き飽和）
                        let packed = i8x16_narrow_i16x8(v0, v1);

                        // ClippedReLU: max(0, min(127, x))
                        let zero = i8x16_splat(0);
                        let max_val = i8x16_splat(127);
                        let clipped = i8x16_max(packed, zero);
                        let clipped = i8x16_min(clipped, max_val);

                        v128_store(out_ptr.add(i * 16) as *mut v128, clipped);
                    }
                }
                continue;
            }

            // スカラーフォールバック
            #[allow(unreachable_code)]
            for i in 0..TRANSFORMED_FEATURE_DIMENSIONS {
                output[out_offset + i] = accumulation[i].clamp(0, 127) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_weights() {
        // ダミーのFeatureTransformerを作成
        let ft = FeatureTransformer {
            biases: Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]),
            weights: vec![1i16; HALFKP_DIMENSIONS * TRANSFORMED_FEATURE_DIMENSIONS]
                .into_boxed_slice(),
        };

        let mut acc = [0i16; TRANSFORMED_FEATURE_DIMENSIONS];
        ft.add_weights(&mut acc, 0);

        // 全て1が加算されているはず
        for &val in acc.iter() {
            assert_eq!(val, 1);
        }
    }
}
