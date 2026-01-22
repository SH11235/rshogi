//! FeatureTransformer - 入力特徴量を変換する最初の層
//!
//! HalfKP 特徴量（自玉×BonaPiece）の活性なインデックス集合から、
//! 片側 256 次元×両視点 = 512 次元の中間表現を生成する。
//! 盤上駒および手駒を特徴量として扱い、`DirtyPiece` に基づく差分更新にも対応する。

use super::accumulator::{
    Accumulator, AccumulatorStack, Aligned, AlignedBox, DirtyPiece, IndexList, MAX_ACTIVE_FEATURES,
};
use super::constants::{HALFKP_DIMENSIONS, NUM_REFRESH_TRIGGERS, TRANSFORMED_FEATURE_DIMENSIONS};
use super::diff::get_features_from_dirty_piece;
use super::features::{FeatureSet, HalfKPFeatureSet};
use crate::position::Position;
use crate::types::Color;
use std::io::{self, Read};

/// FeatureTransformerのパラメータ
#[repr(C, align(64))]
pub struct FeatureTransformer {
    /// バイアス [half_dimensions]
    pub biases: Aligned<[i16; TRANSFORMED_FEATURE_DIMENSIONS]>,

    /// 重み [input_dimensions][half_dimensions]
    /// 64バイトアラインメントで確保（aligned load/store用）
    pub weights: AlignedBox<i16>,
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

        // 重みを読み込み（64バイトアラインメント）
        let weight_size = HALFKP_DIMENSIONS * TRANSFORMED_FEATURE_DIMENSIONS;
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

    /// 差分計算を使わずにAccumulatorを計算
    ///
    /// YaneuraOu の classic NNUE と同様に、トリガーごとにアキュムレータを計算する。
    /// 現在は NUM_REFRESH_TRIGGERS=1 なので trigger=0 のみ処理。
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut Accumulator) {
        // トリガーごとにループ（現在は trigger=0 のみ）
        for trigger in 0..NUM_REFRESH_TRIGGERS {
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                let accumulation = acc.get_mut(p, trigger);

                // trigger=0 はバイアスで初期化、それ以外はゼロ初期化
                if trigger == 0 {
                    accumulation.copy_from_slice(&self.biases.0);
                } else {
                    accumulation.fill(0);
                }

                // アクティブな特徴量の重みを加算
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
    ///
    /// YaneuraOu classic と同様に、視点ごとに reset 判定を行う。
    /// reset が必要な視点は biases から再構築、それ以外は差分更新。
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut Accumulator,
        prev_acc: &Accumulator,
    ) {
        // トリガーごとにループ（現在は trigger=0 のみ）
        for trigger in 0..NUM_REFRESH_TRIGGERS {
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                let reset = HalfKPFeatureSet::needs_refresh(dirty_piece, perspective);

                if reset {
                    // reset が必要な場合: biases で初期化してアクティブ特徴量を加算
                    let accumulation = acc.get_mut(p, trigger);
                    if trigger == 0 {
                        accumulation.copy_from_slice(&self.biases.0);
                    } else {
                        accumulation.fill(0);
                    }

                    // アクティブな特徴量の重みを加算
                    let active_indices = self.get_active_features(pos, perspective);
                    for &index in active_indices.iter() {
                        self.add_weights(accumulation, index);
                    }
                } else {
                    // 差分更新
                    // 玉のマスを取得（後手視点では反転）
                    let raw_king_sq = pos.king_square(perspective);
                    let king_sq = if perspective == Color::Black {
                        raw_king_sq
                    } else {
                        raw_king_sq.inverse()
                    };
                    let (removed, added) =
                        get_features_from_dirty_piece(dirty_piece, perspective, king_sq);

                    // 前の値をコピー
                    let prev = prev_acc.get(p, trigger);
                    let curr = acc.get_mut(p, trigger);
                    curr.copy_from_slice(prev);

                    // 削除された特徴量の重みを減算
                    for &index in removed.iter() {
                        self.sub_weights(curr, index);
                    }

                    // 追加された特徴量の重みを加算
                    for &index in added.iter() {
                        self.add_weights(curr, index);
                    }
                }
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 複数手分の差分を適用してアキュムレータを更新（遅延評価パターン）
    ///
    /// source_idx から current_idx までの差分を積み重ねてアキュムレータを構築する。
    /// AccumulatorStack.find_usable_accumulator() で玉移動がないことを確認済みの前提。
    ///
    /// 戻り値: true=成功、false=差分更新不可
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStack,
        source_idx: usize,
    ) -> bool {
        // 1. source → current のパスを収集
        let Some(path) = stack.collect_path(source_idx) else {
            // パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
            return false;
        };

        // 2. ソースのアキュムレータをコピー
        // Note: clone() + copy_from_slice による二重コピーを避ける最適化を試みたが、
        // NPSに改善が見られなかった。YaneuraOu の C++ 実装でも同様のパターンを使用。
        let source_acc = stack.entry_at(source_idx).accumulator.clone();
        {
            let current_acc = &mut stack.current_mut().accumulator;
            // トリガーごとにコピー
            for trigger in 0..NUM_REFRESH_TRIGGERS {
                for perspective in [Color::Black, Color::White] {
                    let p = perspective as usize;
                    current_acc.get_mut(p, trigger).copy_from_slice(source_acc.get(p, trigger));
                }
            }
        }

        // 3. 各手の差分を順番に適用
        for &entry_idx in path.iter() {
            let dirty_piece = stack.entry_at(entry_idx).dirty_piece;

            // トリガーごとにループ（現在は trigger=0 のみ）
            for trigger in 0..NUM_REFRESH_TRIGGERS {
                for perspective in [Color::Black, Color::White] {
                    // find_usable_accumulator() で玉移動がないことを確認済み
                    debug_assert!(
                        !dirty_piece.king_moved[perspective.index()],
                        "King moved between source and current - should have been caught by find_usable_accumulator"
                    );

                    // 現局面の玉位置を使用（玉移動なしなので祖先と同じ）
                    // 後手視点では反転
                    let raw_king_sq = pos.king_square(perspective);
                    let king_sq = if perspective == Color::Black {
                        raw_king_sq
                    } else {
                        raw_king_sq.inverse()
                    };
                    let (removed, added) =
                        get_features_from_dirty_piece(&dirty_piece, perspective, king_sq);

                    // 差分が空の場合はスキップ（何も変化なし）
                    // 注: 通常の駒移動では差分が発生するが、null move では空になる
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
    ///
    /// 盤上駒および手駒を HalfKP 特徴量に写像する。
    /// FeatureSet 経由で特徴量を取得する。
    #[inline]
    fn get_active_features(
        &self,
        pos: &Position,
        perspective: Color,
    ) -> IndexList<MAX_ACTIVE_FEATURES> {
        HalfKPFeatureSet::collect_active_indices(pos, perspective)
    }

    /// 重みを累積値に加算
    ///
    /// AVX2/SSE2/WASMのSIMD最適化版。256要素を一度に処理。
    /// YaneuraOu classic と同様に非飽和演算を使用する。
    /// weightsとaccumulationは64バイトアラインされている前提でaligned load/storeを使用。
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS], index: usize) {
        // オーバーフロー安全なオフセット計算
        let Some(offset) = index.checked_mul(TRANSFORMED_FEATURE_DIMENSIONS) else {
            debug_assert!(false, "HalfKP add_weights: index overflow (index={index})");
            return; // オーバーフロー → 不正なindex
        };
        let Some(end) = offset.checked_add(TRANSFORMED_FEATURE_DIMENSIONS) else {
            debug_assert!(false, "HalfKP add_weights: offset+dim overflow (offset={offset})");
            return;
        };
        if end > self.weights.len() {
            debug_assert!(
                false,
                "HalfKP add_weights: OOB (index={index}, end={end}, weights_len={})",
                self.weights.len()
            );
            return; // 範囲外チェック
        }

        let weights = &self.weights[offset..offset + TRANSFORMED_FEATURE_DIMENSIONS];

        // AVX2: 256bit = 16 x i16
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // SAFETY:
            // - weights: AlignedBoxで64バイトアライン、各行は512バイト(64の倍数)
            // - accumulation: Aligned<[i16; 256]>で64バイトアライン
            // - 256要素 = 16要素 × 16回のループで完全にカバー
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                #[allow(clippy::manual_is_multiple_of)]
                {
                    debug_assert!(acc_ptr as usize % 64 == 0, "accumulation not 64-byte aligned");
                    debug_assert!(weight_ptr as usize % 64 == 0, "weights not 64-byte aligned");
                }

                for i in 0..16 {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
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
            // SAFETY: 同上（16バイトアライン）
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                #[allow(clippy::manual_is_multiple_of)]
                {
                    debug_assert!(acc_ptr as usize % 64 == 0, "accumulation not 64-byte aligned");
                    debug_assert!(weight_ptr as usize % 64 == 0, "weights not 64-byte aligned");
                }

                for i in 0..32 {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // WASM SIMD128: 128bit = 8 x i16
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY: 同上
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

        // スカラーフォールバック（非飽和演算 - YO classic と同等）
        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_add(weight);
        }
    }

    /// 重みを累積値から減算
    ///
    /// AVX2/SSE2/WASMのSIMD最適化版。256要素を一度に処理。
    /// YaneuraOu classic と同様に非飽和演算を使用する。
    /// weightsとaccumulationは64バイトアラインされている前提でaligned load/storeを使用。
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS], index: usize) {
        // オーバーフロー安全なオフセット計算
        let Some(offset) = index.checked_mul(TRANSFORMED_FEATURE_DIMENSIONS) else {
            debug_assert!(false, "HalfKP sub_weights: index overflow (index={index})");
            return; // オーバーフロー → 不正なindex
        };
        let Some(end) = offset.checked_add(TRANSFORMED_FEATURE_DIMENSIONS) else {
            debug_assert!(false, "HalfKP sub_weights: offset+dim overflow (offset={offset})");
            return;
        };
        if end > self.weights.len() {
            debug_assert!(
                false,
                "HalfKP sub_weights: OOB (index={index}, end={end}, weights_len={})",
                self.weights.len()
            );
            return;
        }

        let weights = &self.weights[offset..offset + TRANSFORMED_FEATURE_DIMENSIONS];

        // AVX2: 256bit = 16 x i16
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // SAFETY:
            // - weights: AlignedBoxで64バイトアライン、各行は512バイト(64の倍数)
            // - accumulation: Aligned<[i16; 256]>で64バイトアライン
            // - 256要素 = 16要素 × 16回のループで完全にカバー
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                #[allow(clippy::manual_is_multiple_of)]
                {
                    debug_assert!(acc_ptr as usize % 64 == 0, "accumulation not 64-byte aligned");
                    debug_assert!(weight_ptr as usize % 64 == 0, "weights not 64-byte aligned");
                }

                for i in 0..16 {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_sub_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
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
            // SAFETY: 同上（16バイトアライン）
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                #[allow(clippy::manual_is_multiple_of)]
                {
                    debug_assert!(acc_ptr as usize % 64 == 0, "accumulation not 64-byte aligned");
                    debug_assert!(weight_ptr as usize % 64 == 0, "weights not 64-byte aligned");
                }

                for i in 0..32 {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_sub_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // WASM SIMD128: 128bit = 8 x i16
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY: 同上
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

        // スカラーフォールバック（非飽和演算 - YO classic と同等）
        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_sub(weight);
        }
    }

    /// Accumulatorの値を変換して出力
    /// ClippedReLU(clamp(0, 127))を適用し、両視点を結合
    ///
    /// YaneuraOu の classic NNUE と同様に、トリガーごとの accumulation を合算して
    /// ClippedReLU を適用する。現在は NUM_REFRESH_TRIGGERS=1 なので trigger=0 のみ使用。
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
            // NUM_TRIGGERS=1 の場合は trigger=0 のみ、将来 trigger が増えた場合は合算が必要
            let accumulation = acc.get(perspective as usize, 0);

            // AVX2: 256bit = 16 x i16 → 32 x i8（パック後）
            #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
            {
                // SAFETY:
                // - loadu/storeu を使用しているためアライメント要件なし
                // - 256要素 = 32要素 × 8回のループで完全にカバー
                unsafe {
                    use std::arch::x86_64::*;
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output[out_offset..].as_mut_ptr();
                    let zero = _mm256_setzero_si256();

                    for i in 0..8 {
                        // 32個のi16を読み込み（2つの__m256i）
                        let v0 = _mm256_loadu_si256(acc_ptr.add(i * 32) as *const __m256i);
                        let v1 = _mm256_loadu_si256(acc_ptr.add(i * 32 + 16) as *const __m256i);

                        // i16 → i8 にパック（符号付き飽和: -128〜127）
                        // 結果のレーン順序: [v0_lo, v1_lo, v0_hi, v1_hi]
                        let packed = _mm256_packs_epi16(v0, v1);
                        // レーン順序を修正: 0xD8 = 11_01_10_00 → [0, 2, 1, 3]
                        let packed = _mm256_permute4x64_epi64(packed, 0xD8);

                        // ClippedReLU: max(0, x)
                        // 注: min(127, x) は不要。飽和パックで既に -128〜127 にクランプ済み
                        let clipped = _mm256_max_epi8(packed, zero);

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

                    // SSE2には_mm_max_epi8がないため、符号付きi8を符号なしu8に変換して処理
                    // [-128, 127] → [0, 255] に変換してから符号なしmaxを適用し、最後に戻す
                    let offset_128 = _mm_set1_epi8(-128i8);
                    let zero_unsigned = _mm_set1_epi8(-128i8); // 0 + (-128) = -128 (as u8: 128)

                    for i in 0..16 {
                        // 16個のi16を読み込み（2つの__m128i）
                        let v0 = _mm_loadu_si128(acc_ptr.add(i * 16) as *const __m128i);
                        let v1 = _mm_loadu_si128(acc_ptr.add(i * 16 + 8) as *const __m128i);

                        // i16 → i8 にパック（符号付き飽和: -128〜127）
                        let packed = _mm_packs_epi16(v0, v1);

                        // ClippedReLU: max(0, x)
                        // 注: min(127, x) は不要。飽和パックで既に -128〜127 にクランプ済み
                        let packed_unsigned = _mm_add_epi8(packed, offset_128);
                        let clipped = _mm_max_epu8(packed_unsigned, zero_unsigned);
                        let clipped = _mm_sub_epi8(clipped, offset_128);

                        _mm_storeu_si128(out_ptr.add(i * 16) as *mut __m128i, clipped);
                    }
                }
                continue;
            }

            // WASM SIMD128: 128bit = 8 x i16 → 16 x i8
            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            {
                // SAFETY: 同上
                unsafe {
                    use std::arch::wasm32::*;
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output[out_offset..].as_mut_ptr();

                    let zero = i8x16_splat(0);

                    for i in 0..16 {
                        // 16個のi16を読み込み（2つのv128）
                        let v0 = v128_load(acc_ptr.add(i * 16) as *const v128);
                        let v1 = v128_load(acc_ptr.add(i * 16 + 8) as *const v128);

                        // i16 → i8 にパック（符号付き飽和: -128〜127）
                        let packed = i8x16_narrow_i16x8(v0, v1);

                        // ClippedReLU: max(0, x)
                        // 注: min(127, x) は不要。飽和パックで既に -128〜127 にクランプ済み
                        let clipped = i8x16_max(packed, zero);

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

    /// テスト用にAlignedBoxを作成し、値を設定するヘルパー
    fn create_weights_with_value(value: i16) -> AlignedBox<i16> {
        let mut weights =
            AlignedBox::new_zeroed(HALFKP_DIMENSIONS * TRANSFORMED_FEATURE_DIMENSIONS);
        for w in weights.iter_mut() {
            *w = value;
        }
        weights
    }

    #[test]
    fn test_add_weights() {
        // ダミーのFeatureTransformerを作成（AlignedBox使用）
        let ft = FeatureTransformer {
            biases: Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]),
            weights: create_weights_with_value(1),
        };

        // Accumulator経由でアラインされた配列を使用
        let mut acc_storage = Accumulator::new();
        let acc = acc_storage.get_mut(0, 0);
        ft.add_weights(acc, 0);

        // 全て1が加算されているはず
        for &val in acc.iter() {
            assert_eq!(val, 1);
        }
    }

    #[test]
    fn test_sub_weights() {
        // ダミーのFeatureTransformerを作成（AlignedBox使用）
        let ft = FeatureTransformer {
            biases: Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]),
            weights: create_weights_with_value(1),
        };

        // Accumulator経由でアラインされた配列を使用
        let mut acc_storage = Accumulator::new();
        let acc = acc_storage.get_mut(0, 0);
        // 初期値を10に設定
        for v in acc.iter_mut() {
            *v = 10;
        }
        ft.sub_weights(acc, 0);

        // 全て10 - 1 = 9になっているはず
        for &val in acc.iter() {
            assert_eq!(val, 9);
        }
    }

    #[test]
    fn test_transform() {
        use crate::nnue::accumulator::Accumulator;
        use crate::types::Color;

        // ダミーのFeatureTransformerを作成（AlignedBox使用）
        let ft = FeatureTransformer {
            biases: Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]),
            weights: create_weights_with_value(0),
        };

        // Accumulatorを設定（各視点で異なる値）
        // [perspective][trigger][dimension] 構造
        let mut acc = Accumulator::new();
        // 先手視点: 64（ClippedReLU後も64、127以下なのでクランプされない）
        for i in 0..TRANSFORMED_FEATURE_DIMENSIONS {
            acc.accumulation[Color::Black.index()][0].0[i] = 64;
        }
        // 後手視点: 100（ClippedReLU後も100、127以下なのでクランプされない）
        for i in 0..TRANSFORMED_FEATURE_DIMENSIONS {
            acc.accumulation[Color::White.index()][0].0[i] = 100;
        }
        acc.computed_accumulation = true;

        let mut output = [0u8; TRANSFORMED_FEATURE_DIMENSIONS * 2];

        // 先手番での変換
        ft.transform(&acc, Color::Black, &mut output);

        // 前半256バイト: 先手視点 → 64 (i16→u8、クランプ0-127)
        for &val in output[..TRANSFORMED_FEATURE_DIMENSIONS].iter() {
            assert_eq!(val, 64, "Black perspective should be 64");
        }
        // 後半256バイト: 後手視点 → 100 (i16→u8、クランプ0-127)
        for &val in output[TRANSFORMED_FEATURE_DIMENSIONS..].iter() {
            assert_eq!(val, 100, "White perspective should be 100");
        }
    }

    #[test]
    fn test_transform_clipping() {
        use crate::nnue::accumulator::Accumulator;
        use crate::types::Color;

        // ダミーのFeatureTransformerを作成（AlignedBox使用）
        let ft = FeatureTransformer {
            biases: Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]),
            weights: create_weights_with_value(0),
        };

        let mut acc = Accumulator::new();
        // クリッピングテスト: 負の値→0、127超→127
        // [perspective][trigger][dimension] 構造
        for i in 0..TRANSFORMED_FEATURE_DIMENSIONS {
            if i < 64 {
                acc.accumulation[Color::Black.index()][0].0[i] = -100; // 負→0にクランプ
            } else if i < 128 {
                acc.accumulation[Color::Black.index()][0].0[i] = 200; // 127超→127にクランプ
            } else {
                acc.accumulation[Color::Black.index()][0].0[i] = 50; // 範囲内→そのまま
            }
            acc.accumulation[Color::White.index()][0].0[i] = 0;
        }
        acc.computed_accumulation = true;

        let mut output = [0u8; TRANSFORMED_FEATURE_DIMENSIONS * 2];
        ft.transform(&acc, Color::Black, &mut output);

        // 負の値は0にクランプ
        for &val in output[..64].iter() {
            assert_eq!(val, 0, "Negative should be clamped to 0");
        }
        // 127超は127にクランプ
        for &val in output[64..128].iter() {
            assert_eq!(val, 127, "Values > 127 should be clamped to 127");
        }
        // 範囲内はそのまま
        for &val in output[128..TRANSFORMED_FEATURE_DIMENSIONS].iter() {
            assert_eq!(val, 50, "Values in range should pass through");
        }
    }
}
