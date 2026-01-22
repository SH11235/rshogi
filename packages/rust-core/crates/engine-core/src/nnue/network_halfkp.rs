//! NetworkHalfKP - const generics ベースの HalfKP ネットワーク統一実装
//!
//! HalfKP 特徴量を使用し、L1/L2/L3 のサイズと活性化関数を型パラメータで切り替え可能にした実装。
//!
//! # 設計
//!
//! ```text
//! NetworkHalfKP<L1, L1_INPUT, L2, L3, A>
//!   L1: FT出力次元（片側）
//!   L1_INPUT: L1層の入力次元（= L1 * 2、両視点結合）
//!   L2: 隠れ層1の出力次元
//!   L3: 隠れ層2の出力次元
//!   A: FtActivation trait を実装する活性化関数型
//! ```
//!
//! # サポートするアーキテクチャ
//!
//! | 型エイリアス | L1 | L2 | L3 | 活性化 |
//! |-------------|-----|-----|-----|--------|
//! | HalfKP256CReLU | 256 | 32 | 32 | CReLU |
//! | HalfKP256SCReLU | 256 | 32 | 32 | SCReLU |
//! | HalfKP256Pairwise | 256 | 32 | 32 | PairwiseCReLU |
//! | HalfKP512CReLU | 512 | 8 | 96 | CReLU |
//!
//! # 特徴量
//!
//! - 入力次元: 125,388 (81キング位置 × 1,548 BonaPiece)
//! - 従来の classic NNUE（水匠/tanuki互換）

use std::io::{self, Read, Seek};
use std::marker::PhantomData;

use super::accumulator::{
    Aligned as AlignedGeneric, AlignedBox, DirtyPiece, IndexList, MAX_PATH_LENGTH,
};
use super::activation::FtActivation;
use super::constants::{FV_SCALE, HALFKP_DIMENSIONS, MAX_ARCH_LEN, NNUE_VERSION};
use super::features::{FeatureSet, HalfKPFeatureSet};
use super::network::get_fv_scale_override;
use crate::position::Position;
use crate::types::{Color, Value};

// =============================================================================
// SIMD ヘルパー関数
// =============================================================================

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline]
unsafe fn m256_add_dpbusd_epi32(
    acc: &mut std::arch::x86_64::__m256i,
    a: std::arch::x86_64::__m256i,
    b: std::arch::x86_64::__m256i,
) {
    use std::arch::x86_64::*;
    let product = _mm256_maddubs_epi16(a, b);
    let product32 = _mm256_madd_epi16(product, _mm256_set1_epi16(1));
    *acc = _mm256_add_epi32(*acc, product32);
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline]
unsafe fn hsum_i32_avx2(v: std::arch::x86_64::__m256i) -> i32 {
    use std::arch::x86_64::*;
    let hi = _mm256_extracti128_si256(v, 1);
    let lo = _mm256_castsi256_si128(v);
    let sum128 = _mm_add_epi32(lo, hi);
    let hi64 = _mm_unpackhi_epi64(sum128, sum128);
    let sum64 = _mm_add_epi32(sum128, hi64);
    let hi32 = _mm_shuffle_epi32(sum64, 1);
    let sum32 = _mm_add_epi32(sum64, hi32);
    _mm_cvtsi128_si32(sum32)
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "ssse3",
    not(target_feature = "avx2")
))]
#[inline]
unsafe fn hsum_i32_sse2(v: std::arch::x86_64::__m128i) -> i32 {
    use std::arch::x86_64::*;
    let hi64 = _mm_unpackhi_epi64(v, v);
    let sum64 = _mm_add_epi32(v, hi64);
    let hi32 = _mm_shuffle_epi32(sum64, 1);
    let sum32 = _mm_add_epi32(sum64, hi32);
    _mm_cvtsi128_si32(sum32)
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "ssse3",
    not(target_feature = "avx2")
))]
#[inline]
unsafe fn m128_add_dpbusd_epi32(
    acc: &mut std::arch::x86_64::__m128i,
    a: std::arch::x86_64::__m128i,
    b: std::arch::x86_64::__m128i,
) {
    use std::arch::x86_64::*;
    let product = _mm_maddubs_epi16(a, b);
    let product32 = _mm_madd_epi16(product, _mm_set1_epi16(1));
    *acc = _mm_add_epi32(*acc, product32);
}

// =============================================================================
// AccumulatorHalfKP - const generics 版アキュムレータ
// =============================================================================

/// 64バイトアラインされた固定サイズ i16 配列（static版Alignedと同等）
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct AlignedI16<const N: usize>(pub [i16; N]);

impl<const N: usize> Default for AlignedI16<N> {
    fn default() -> Self {
        Self([0i16; N])
    }
}

/// HalfKP アキュムレータ
///
/// static版Accumulatorと同じくインライン配列を使用し、ポインタ間接参照を排除。
/// - 境界チェックのコンパイル時排除
/// - ループの完全展開
/// - SIMD命令の最適な配置
/// - キャッシュ効率の向上
#[repr(C, align(64))]
pub struct AccumulatorHalfKP<const L1: usize> {
    /// アキュムレータバッファ [perspective][L1]（64バイトアライン、インライン）
    pub accumulation: [AlignedI16<L1>; 2],
    /// 計算済みフラグ
    pub computed_accumulation: bool,
}

impl<const L1: usize> AccumulatorHalfKP<L1> {
    /// 新規作成
    pub fn new() -> Self {
        Self {
            accumulation: [AlignedI16::default(), AlignedI16::default()],
            computed_accumulation: false,
        }
    }

    /// クリア
    pub fn clear(&mut self) {
        self.accumulation[0].0.fill(0);
        self.accumulation[1].0.fill(0);
        self.computed_accumulation = false;
    }
}

impl<const L1: usize> Default for AccumulatorHalfKP<L1> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const L1: usize> Clone for AccumulatorHalfKP<L1> {
    fn clone(&self) -> Self {
        Self {
            accumulation: self.accumulation,
            computed_accumulation: self.computed_accumulation,
        }
    }
}

// =============================================================================
// AccumulatorStackHalfKP - アキュムレータスタック
// =============================================================================

/// スタックエントリ
pub struct AccumulatorEntryHalfKP<const L1: usize> {
    pub accumulator: AccumulatorHalfKP<L1>,
    pub dirty_piece: DirtyPiece,
    pub previous: Option<usize>,
}

/// アキュムレータスタック
pub struct AccumulatorStackHalfKP<const L1: usize> {
    entries: Vec<AccumulatorEntryHalfKP<L1>>,
    current_idx: usize,
}

impl<const L1: usize> AccumulatorStackHalfKP<L1> {
    /// 新規作成
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(128);
        entries.push(AccumulatorEntryHalfKP {
            accumulator: AccumulatorHalfKP::new(),
            dirty_piece: DirtyPiece::default(),
            previous: None,
        });
        Self {
            entries,
            current_idx: 0,
        }
    }

    /// 現在のエントリを取得
    pub fn current(&self) -> &AccumulatorEntryHalfKP<L1> {
        &self.entries[self.current_idx]
    }

    /// 現在のエントリを取得（可変）
    pub fn current_mut(&mut self) -> &mut AccumulatorEntryHalfKP<L1> {
        &mut self.entries[self.current_idx]
    }

    /// プッシュ
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        let prev_idx = self.current_idx;
        self.current_idx = self.entries.len();
        self.entries.push(AccumulatorEntryHalfKP {
            accumulator: AccumulatorHalfKP::new(),
            dirty_piece,
            previous: Some(prev_idx),
        });
    }

    /// ポップ
    pub fn pop(&mut self) {
        if let Some(prev) = self.entries[self.current_idx].previous {
            self.current_idx = prev;
        }
        self.entries.truncate(self.current_idx + 1);
    }

    /// 探索開始時のリセット
    pub fn reset(&mut self) {
        self.current_idx = 0;
        self.entries.truncate(1);
        self.entries[0].accumulator.computed_accumulation = false;
        self.entries[0].dirty_piece.clear();
        self.entries[0].previous = None;
    }

    /// 祖先を辿って使用可能なアキュムレータを探す
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        const MAX_DEPTH: usize = 8;

        let current = &self.entries[self.current_idx];
        if current.dirty_piece.king_moved[0] || current.dirty_piece.king_moved[1] {
            return None;
        }

        let mut prev_idx = current.previous?;
        let mut depth = 1;

        loop {
            let prev = &self.entries[prev_idx];
            if prev.accumulator.computed_accumulation {
                return Some((prev_idx, depth));
            }
            if depth >= MAX_DEPTH {
                return None;
            }
            let next_prev_idx = prev.previous?;
            if prev.dirty_piece.king_moved[0] || prev.dirty_piece.king_moved[1] {
                return None;
            }
            prev_idx = next_prev_idx;
            depth += 1;
        }
    }

    /// 指定インデックスのエントリを取得
    pub fn entry_at(&self, idx: usize) -> &AccumulatorEntryHalfKP<L1> {
        &self.entries[idx]
    }

    /// 指定インデックスのエントリを取得（可変）
    pub fn entry_at_mut(&mut self, idx: usize) -> &mut AccumulatorEntryHalfKP<L1> {
        &mut self.entries[idx]
    }

    /// 前回と現在のアキュムレータを取得（可変）
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&AccumulatorHalfKP<L1>, &mut AccumulatorHalfKP<L1>) {
        let current_idx = self.current_idx;
        debug_assert!(
            prev_idx < current_idx,
            "prev_idx ({prev_idx}) must be < cur_idx ({current_idx})"
        );
        let (left, right) = self.entries.split_at_mut(current_idx);
        (&left[prev_idx].accumulator, &mut right[0].accumulator)
    }

    /// 現在のインデックスを取得
    pub fn current_index(&self) -> usize {
        self.current_idx
    }

    /// 指定インデックスから現在位置までのパスを収集
    pub fn collect_path(&self, source_idx: usize) -> Option<IndexList<MAX_PATH_LENGTH>> {
        let mut path = IndexList::new();
        let mut idx = self.current_idx;

        while idx != source_idx {
            if !path.push(idx) {
                return None;
            }
            match self.entries[idx].previous {
                Some(prev) => idx = prev,
                None => return None,
            }
        }

        path.reverse();
        Some(path)
    }
}

impl<const L1: usize> Default for AccumulatorStackHalfKP<L1> {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// FeatureTransformerHalfKP - const generics 版 Feature Transformer
// =============================================================================

/// HalfKP Feature Transformer
///
/// static版と同じくbiasesをインラインで保持し、ポインタ間接参照を排除。
#[repr(C, align(64))]
pub struct FeatureTransformerHalfKP<const L1: usize> {
    /// バイアス [L1]（64バイトアラインメント、インライン）
    pub biases: AlignedI16<L1>,
    /// 重み [input_dimensions][L1]
    pub weights: AlignedBox<i16>,
}

impl<const L1: usize> FeatureTransformerHalfKP<L1> {
    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let input_dim = HALFKP_DIMENSIONS;

        // バイアスを読み込み（64バイトアラインメント、インライン）
        let mut biases = AlignedI16::<L1>::default();
        let mut buf = [0u8; 2];
        for bias in biases.0.iter_mut() {
            reader.read_exact(&mut buf)?;
            *bias = i16::from_le_bytes(buf);
        }

        // 重みを読み込み
        let weight_size = input_dim * L1;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf)?;
            *weight = i16::from_le_bytes(buf);
        }

        Ok(Self { biases, weights })
    }

    /// Accumulatorをリフレッシュ
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKP<L1>) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = &mut acc.accumulation[p].0;

            // biases を accumulation にコピー（SIMD 最適化版）
            self.copy_biases_to_accumulation(accumulation);

            let active_indices = HalfKPFeatureSet::collect_active_indices(pos, perspective);
            for &index in active_indices.iter() {
                self.add_weights(accumulation, index);
            }
        }

        acc.computed_accumulation = true;
    }

    /// biases を accumulation にコピー（SIMD 最適化版）
    ///
    /// AVX2: 16 i16/レジスタ → L1 / 16 チャンク
    /// SSE2: 8 i16/レジスタ → L1 / 8 チャンク
    #[inline]
    fn copy_biases_to_accumulation(&self, accumulation: &mut [i16; L1]) {
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let dst_ptr = accumulation.as_mut_ptr() as *mut __m256i;
                let src_ptr = self.biases.0.as_ptr() as *const __m256i;

                // L1 / 16 はコンパイル時に定数展開される
                for i in 0..(L1 / 16) {
                    let src_vec = _mm256_load_si256(src_ptr.add(i));
                    _mm256_store_si256(dst_ptr.add(i), src_vec);
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
                let dst_ptr = accumulation.as_mut_ptr() as *mut __m128i;
                let src_ptr = self.biases.0.as_ptr() as *const __m128i;

                for i in 0..(L1 / 8) {
                    let src_vec = _mm_load_si128(src_ptr.add(i));
                    _mm_store_si128(dst_ptr.add(i), src_vec);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        {
            accumulation.copy_from_slice(&self.biases.0);
        }
    }

    /// 差分更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorHalfKP<L1>,
        prev_acc: &AccumulatorHalfKP<L1>,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = HalfKPFeatureSet::needs_refresh(dirty_piece, perspective);

            if reset {
                self.copy_biases_to_accumulation(&mut acc.accumulation[p].0);
                let active_indices = HalfKPFeatureSet::collect_active_indices(pos, perspective);
                for &index in active_indices.iter() {
                    self.add_weights(&mut acc.accumulation[p].0, index);
                }
            } else {
                let (removed, added) = HalfKPFeatureSet::collect_changed_indices(
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                );

                acc.accumulation[p].0.copy_from_slice(&prev_acc.accumulation[p].0);

                for &index in removed.iter() {
                    self.sub_weights(&mut acc.accumulation[p].0, index);
                }
                for &index in added.iter() {
                    self.add_weights(&mut acc.accumulation[p].0, index);
                }
            }
        }

        acc.computed_accumulation = true;
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKP<L1>,
        source_idx: usize,
    ) -> bool {
        let Some(path) = stack.collect_path(source_idx) else {
            return false;
        };

        // source から current へコピー
        {
            let (source_acc, current_acc) = stack.get_prev_and_current_accumulators(source_idx);
            for p in 0..2 {
                current_acc.accumulation[p].0.copy_from_slice(&source_acc.accumulation[p].0);
            }
        }

        let current_idx = stack.current_index();
        for &entry_idx in path.iter() {
            let dirty_piece = stack.entry_at(entry_idx).dirty_piece;

            for perspective in [Color::Black, Color::White] {
                debug_assert!(
                    !dirty_piece.king_moved[perspective.index()],
                    "King moved between source and current"
                );

                let king_sq = pos.king_square(perspective);
                let (removed, added) =
                    HalfKPFeatureSet::collect_changed_indices(&dirty_piece, perspective, king_sq);

                let p = perspective as usize;
                let accumulation =
                    &mut stack.entry_at_mut(current_idx).accumulator.accumulation[p].0;

                for &index in removed.iter() {
                    self.sub_weights(accumulation, index);
                }
                for &index in added.iter() {
                    self.add_weights(accumulation, index);
                }
            }
        }

        stack.entry_at_mut(current_idx).accumulator.computed_accumulation = true;
        true
    }

    /// 重みを加算（SIMD最適化版）
    ///
    /// 元の feature_transformer.rs と同じパターンを使用。
    /// AVX2: 16 i16/レジスタ → L1 / 16 チャンク
    /// SSE2: 8 i16/レジスタ → L1 / 8 チャンク
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16; L1], index: usize) {
        let offset = index * L1;
        let weights = &self.weights[offset..offset + L1];

        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                // L1 / 16 はコンパイル時に定数展開される
                for i in 0..(L1 / 16) {
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

                for i in 0..(L1 / 8) {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for (acc, &w) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_add(w);
        }
    }

    /// 重みを減算（SIMD最適化版）
    ///
    /// 元の feature_transformer.rs と同じパターンを使用。
    /// AVX2: 16 i16/レジスタ → L1 / 16 チャンク
    /// SSE2: 8 i16/レジスタ → L1 / 8 チャンク
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16; L1], index: usize) {
        let offset = index * L1;
        let weights = &self.weights[offset..offset + L1];

        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                // L1 / 16 はコンパイル時に定数展開される
                for i in 0..(L1 / 16) {
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

                for i in 0..(L1 / 8) {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_sub_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for (acc, &w) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_sub(w);
        }
    }

    /// 変換（生の i16 出力）
    ///
    /// 活性化関数は呼び出し側で適用する。
    pub fn transform_raw(
        &self,
        acc: &AccumulatorHalfKP<L1>,
        side_to_move: Color,
        output: &mut [i16],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        for (p, &perspective) in perspectives.iter().enumerate() {
            let out_offset = L1 * p;
            let accumulation = &acc.accumulation[perspective as usize].0;
            output[out_offset..out_offset + L1].copy_from_slice(accumulation);
        }
    }
}

// =============================================================================
// AffineTransformHalfKP - const generics 版アフィン変換（ループ逆転最適化版）
// =============================================================================

/// アフィン変換層（ループ逆転最適化 + スクランブル重み形式）
///
/// YaneuraOu/Stockfish スタイルの SIMD 最適化を実装。
/// 重みはスクランブル形式 `weights[input_chunk][output][4]` で保持し、
/// ループ逆転により入力をブロードキャストして全出力に同時適用する。
pub struct AffineTransformHalfKP<const INPUT: usize, const OUTPUT: usize> {
    /// バイアス [OUTPUT]
    pub biases: [i32; OUTPUT],
    /// 重み（スクランブル形式、64バイトアライン）
    pub weights: AlignedBox<i8>,
}

impl<const INPUT: usize, const OUTPUT: usize> AffineTransformHalfKP<INPUT, OUTPUT> {
    /// パディング済み入力次元（32の倍数）
    const PADDED_INPUT: usize = INPUT.div_ceil(32) * 32;

    /// チャンクサイズ（u8×4 = i32として読む単位）
    const CHUNK_SIZE: usize = 4;

    /// 入力チャンク数
    const NUM_INPUT_CHUNKS: usize = Self::PADDED_INPUT / Self::CHUNK_SIZE;

    /// スクランブル形式を使用するかどうか
    /// AVX2: OUTPUT % 8 == 0、SSSE3: OUTPUT % 4 == 0
    #[inline]
    const fn should_use_scrambled_weights() -> bool {
        if cfg!(all(target_arch = "x86_64", target_feature = "avx2")) {
            OUTPUT.is_multiple_of(8)
        } else if cfg!(all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        )) {
            OUTPUT.is_multiple_of(4)
        } else {
            false
        }
    }

    /// 重みインデックスのスクランブル変換
    ///
    /// 元のレイアウト: weights[output][input]
    /// 変換後: weights[input_chunk][output][4]
    #[inline]
    const fn get_weight_index_scrambled(i: usize) -> usize {
        // i = output * PADDED_INPUT + input
        (i / Self::CHUNK_SIZE) % Self::NUM_INPUT_CHUNKS * OUTPUT * Self::CHUNK_SIZE
            + i / Self::PADDED_INPUT * Self::CHUNK_SIZE
            + i % Self::CHUNK_SIZE
    }

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i32; OUTPUT];
        let mut buf4 = [0u8; 4];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // 重みを読み込み（スクランブル形式で格納）
        let weight_size = OUTPUT * Self::PADDED_INPUT;
        let mut weights = AlignedBox::new_zeroed(weight_size);

        // スクランブル形式の場合は変換しながら格納
        #[cfg(any(
            all(target_arch = "x86_64", target_feature = "avx2"),
            all(
                target_arch = "x86_64",
                target_feature = "ssse3",
                not(target_feature = "avx2")
            )
        ))]
        {
            let mut buf1 = [0u8; 1];
            for i in 0..weight_size {
                reader.read_exact(&mut buf1)?;
                let idx = if Self::should_use_scrambled_weights() {
                    Self::get_weight_index_scrambled(i)
                } else {
                    i
                };
                weights[idx] = buf1[0] as i8;
            }
        }

        // スカラー環境: 標準形式で格納
        #[cfg(not(any(
            all(target_arch = "x86_64", target_feature = "avx2"),
            all(
                target_arch = "x86_64",
                target_feature = "ssse3",
                not(target_feature = "avx2")
            )
        )))]
        {
            let mut row_buf = vec![0u8; Self::PADDED_INPUT];
            for o in 0..OUTPUT {
                reader.read_exact(&mut row_buf)?;
                for i in 0..Self::PADDED_INPUT {
                    weights[o * Self::PADDED_INPUT + i] = row_buf[i] as i8;
                }
            }
        }

        Ok(Self { biases, weights })
    }

    /// 順伝播（SIMD最適化版 - ループ逆転）
    pub fn propagate(&self, input: &[u8], output: &mut [i32; OUTPUT]) {
        // AVX2: ループ逆転最適化版
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                self.propagate_avx2_loop_inverted(input, output);
            }
            return;
        }

        // SSSE3: ループ逆転最適化版
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        ))]
        {
            unsafe {
                self.propagate_ssse3_loop_inverted(input, output);
            }
            return;
        }

        // スカラー fallback
        #[allow(unreachable_code)]
        {
            output.copy_from_slice(&self.biases);
            for (j, out) in output.iter_mut().enumerate() {
                let weight_offset = j * Self::PADDED_INPUT;
                for (i, &in_val) in input.iter().enumerate().take(INPUT) {
                    *out += self.weights[weight_offset + i] as i32 * in_val as i32;
                }
            }
        }
    }

    /// AVX2 ループ逆転最適化版
    ///
    /// 外側ループ: 入力チャンク（4バイト単位）
    /// 内側ループ: 全出力レジスタ（8出力/レジスタ）
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    #[inline]
    #[allow(clippy::needless_range_loop)]
    unsafe fn propagate_avx2_loop_inverted(&self, input: &[u8], output: &mut [i32; OUTPUT]) {
        use std::arch::x86_64::*;

        // OUTPUT % 8 == 0 の場合のみループ逆転を使用
        if OUTPUT.is_multiple_of(8) {
            const MAX_REGS: usize = 128; // 最大 1024 出力まで対応
            let num_regs = OUTPUT / 8;
            debug_assert!(num_regs <= MAX_REGS);

            // アキュムレータをバイアスで初期化
            let mut acc = [_mm256_setzero_si256(); MAX_REGS];
            let bias_ptr = self.biases.as_ptr() as *const __m256i;
            for k in 0..num_regs {
                acc[k] = _mm256_loadu_si256(bias_ptr.add(k));
            }

            let input32 = input.as_ptr() as *const i32;
            let weights_ptr = self.weights.as_ptr();

            // 外側ループ: 入力チャンク
            for i in 0..Self::NUM_INPUT_CHUNKS {
                // 入力4バイトを全レーンにブロードキャスト
                let in_val = _mm256_set1_epi32(*input32.add(i));

                // この入力チャンクに対応する重みの開始位置
                // スクランブル形式: weights[input_chunk][output][4]
                let col = weights_ptr.add(i * OUTPUT * Self::CHUNK_SIZE) as *const __m256i;

                // 内側ループ: 全出力レジスタに積和演算
                for k in 0..num_regs {
                    m256_add_dpbusd_epi32(&mut acc[k], in_val, _mm256_load_si256(col.add(k)));
                }
            }

            // 結果を出力
            let out_ptr = output.as_mut_ptr() as *mut __m256i;
            for k in 0..num_regs {
                _mm256_storeu_si256(out_ptr.add(k), acc[k]);
            }
        } else {
            // フォールバック: 標準ループ
            output.copy_from_slice(&self.biases);
            let num_chunks = Self::PADDED_INPUT / 32;
            let input_ptr = input.as_ptr();
            let weight_ptr = self.weights.as_ptr();

            for (j, out) in output.iter_mut().enumerate() {
                let mut acc_simd = _mm256_setzero_si256();
                let row_offset = j * Self::PADDED_INPUT;

                for chunk in 0..num_chunks {
                    let in_vec = _mm256_loadu_si256(input_ptr.add(chunk * 32) as *const __m256i);
                    let w_vec = _mm256_load_si256(
                        weight_ptr.add(row_offset + chunk * 32) as *const __m256i
                    );
                    m256_add_dpbusd_epi32(&mut acc_simd, in_vec, w_vec);
                }

                *out += hsum_i32_avx2(acc_simd);
            }
        }
    }

    /// SSSE3 ループ逆転最適化版
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "ssse3",
        not(target_feature = "avx2")
    ))]
    #[inline]
    unsafe fn propagate_ssse3_loop_inverted(&self, input: &[u8], output: &mut [i32; OUTPUT]) {
        use std::arch::x86_64::*;

        // OUTPUT % 4 == 0 の場合のみループ逆転を使用
        if OUTPUT % 4 == 0 && OUTPUT > 0 {
            const MAX_REGS: usize = 256; // 最大 1024 出力まで対応
            let num_regs = OUTPUT / 4;
            debug_assert!(num_regs <= MAX_REGS);

            // アキュムレータをバイアスで初期化
            let mut acc = [_mm_setzero_si128(); MAX_REGS];
            let bias_ptr = self.biases.as_ptr() as *const __m128i;
            for k in 0..num_regs {
                acc[k] = _mm_loadu_si128(bias_ptr.add(k));
            }

            let input32 = input.as_ptr() as *const i32;
            let weights_ptr = self.weights.as_ptr();

            // 外側ループ: 入力チャンク
            for i in 0..Self::NUM_INPUT_CHUNKS {
                let in_val = _mm_set1_epi32(*input32.add(i));
                let col = weights_ptr.add(i * OUTPUT * Self::CHUNK_SIZE) as *const __m128i;

                for k in 0..num_regs {
                    m128_add_dpbusd_epi32(&mut acc[k], in_val, _mm_load_si128(col.add(k)));
                }
            }

            let out_ptr = output.as_mut_ptr() as *mut __m128i;
            for k in 0..num_regs {
                _mm_storeu_si128(out_ptr.add(k), acc[k]);
            }
        } else {
            // フォールバック
            output.copy_from_slice(&self.biases);
            let num_chunks = Self::PADDED_INPUT / 16;
            let input_ptr = input.as_ptr();
            let weight_ptr = self.weights.as_ptr();

            for (j, out) in output.iter_mut().enumerate() {
                let mut acc_simd = _mm_setzero_si128();
                let row_offset = j * Self::PADDED_INPUT;

                for chunk in 0..num_chunks {
                    let in_vec = _mm_loadu_si128(input_ptr.add(chunk * 16) as *const __m128i);
                    let w_vec =
                        _mm_load_si128(weight_ptr.add(row_offset + chunk * 16) as *const __m128i);
                    m128_add_dpbusd_epi32(&mut acc_simd, in_vec, w_vec);
                }

                *out += hsum_i32_sse2(acc_simd);
            }
        }
    }
}

// =============================================================================
// NetworkHalfKP - const generics 版統一ネットワーク
// =============================================================================

/// HalfKP ネットワーク（const generics 版）
///
/// # 型パラメータ
/// - `L1`: FT出力次元（片側）
/// - `L1_INPUT`: L1層の入力次元（= L1 * 2、両視点結合）
/// - `L2`: 隠れ層1の出力次元
/// - `L3`: 隠れ層2の出力次元
/// - `A`: 活性化関数（FtActivation trait を実装する型）
///
/// # 注意
/// `L1_INPUT` は `L1 * 2` である必要がある。
pub struct NetworkHalfKP<
    const L1: usize,
    const L1_INPUT: usize,
    const L2: usize,
    const L3: usize,
    A: FtActivation,
> {
    /// Feature Transformer (入力 → L1)
    pub feature_transformer: FeatureTransformerHalfKP<L1>,
    /// 隠れ層1: L1_INPUT → L2
    pub l1: AffineTransformHalfKP<L1_INPUT, L2>,
    /// 隠れ層2: L2 → L3
    pub l2: AffineTransformHalfKP<L2, L3>,
    /// 出力層: L3 → 1
    pub output: AffineTransformHalfKP<L3, 1>,
    /// 評価値スケーリング係数
    pub fv_scale: i32,
    /// QA値（クリッピング閾値）
    pub qa: i16,
    /// 活性化関数（型情報のみ）
    _activation: PhantomData<A>,
}

impl<const L1: usize, const L1_INPUT: usize, const L2: usize, const L3: usize, A: FtActivation>
    NetworkHalfKP<L1, L1_INPUT, L2, L3, A>
{
    /// コンパイル時制約: L1_INPUT == L1 * 2
    const _ASSERT_L1_INPUT: () = assert!(L1_INPUT == L1 * 2, "L1_INPUT must equal L1 * 2");

    /// ファイルから読み込み
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        // ヘッダを読み込み
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        if version != NNUE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unknown NNUE version: {version:#x}"),
            ));
        }

        // 構造ハッシュ
        reader.read_exact(&mut buf4)?;

        // アーキテクチャ文字列
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid arch string length: {arch_len}"),
            ));
        }
        let mut arch = vec![0u8; arch_len];
        reader.read_exact(&mut arch)?;

        let arch_str = String::from_utf8_lossy(&arch);

        // FV_SCALE 検出
        let fv_scale = parse_fv_scale_from_arch(&arch_str).unwrap_or(FV_SCALE);

        // QA 検出（デフォルト 127）
        let qa = parse_qa_from_arch(&arch_str).unwrap_or(127);

        // Feature Transformer ハッシュ
        reader.read_exact(&mut buf4)?;

        // Feature Transformer
        let feature_transformer = FeatureTransformerHalfKP::read(reader)?;

        // FC layers ハッシュ
        reader.read_exact(&mut buf4)?;

        // l1: L1_INPUT → L2
        let l1 = AffineTransformHalfKP::read(reader)?;

        // l2: L2 → L3
        let l2 = AffineTransformHalfKP::read(reader)?;

        // output: L3 → 1
        let output = AffineTransformHalfKP::read(reader)?;

        Ok(Self {
            feature_transformer,
            l1,
            l2,
            output,
            fv_scale,
            qa,
            _activation: PhantomData,
        })
    }

    /// Accumulator をリフレッシュ
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKP<L1>) {
        self.feature_transformer.refresh_accumulator(pos, acc);
    }

    /// Accumulator を差分更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorHalfKP<L1>,
        prev_acc: &AccumulatorHalfKP<L1>,
    ) {
        self.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc);
    }

    /// 複数手分の差分を適用
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKP<L1>,
        source_idx: usize,
    ) -> bool {
        self.feature_transformer.forward_update_incremental(pos, stack, source_idx)
    }

    /// 評価値を計算
    ///
    /// 最適化: スタック配列 + 64バイトアラインメントで SIMD 効率を最大化
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorHalfKP<L1>) -> Value {
        // Feature Transformer 出力（生のi16値）- 64バイトアライン
        let mut ft_out_i16 = AlignedGeneric([0i16; L1_INPUT]);
        self.feature_transformer
            .transform_raw(acc, pos.side_to_move(), &mut ft_out_i16.0);

        // 活性化関数適用 (i16 → u8) - 64バイトアライン
        let mut transformed = AlignedGeneric([0u8; L1_INPUT]);
        A::activate_i16_to_u8(&ft_out_i16.0, &mut transformed.0, self.qa);

        // l1 層 - 64バイトアライン
        let mut l1_out = AlignedGeneric([0i32; L2]);
        self.l1.propagate(&transformed.0, &mut l1_out.0);

        // デバッグ: L1出力の範囲チェック
        #[cfg(debug_assertions)]
        for (i, &v) in l1_out.0.iter().enumerate() {
            debug_assert!(
                v.abs() < 1_000_000,
                "L1 output[{i}] = {v} is out of expected range (NetworkHalfKP<{}, {}, {}, {}>)",
                L1,
                L2,
                L3,
                A::name()
            );
        }

        // 活性化関数適用 (i32 → u8) - 64バイトアライン
        let mut l1_relu = AlignedGeneric([0u8; L2]);
        A::activate_i32_to_u8(&l1_out.0, &mut l1_relu.0);

        // l2 層 - 64バイトアライン
        let mut l2_out = AlignedGeneric([0i32; L3]);
        self.l2.propagate(&l1_relu.0, &mut l2_out.0);

        // デバッグ: L2出力の範囲チェック
        #[cfg(debug_assertions)]
        for (i, &v) in l2_out.0.iter().enumerate() {
            debug_assert!(
                v.abs() < 1_000_000,
                "L2 output[{i}] = {v} is out of expected range (NetworkHalfKP<{}, {}, {}, {}>)",
                L1,
                L2,
                L3,
                A::name()
            );
        }

        // 活性化関数適用 (i32 → u8) - 64バイトアライン
        let mut l2_relu = AlignedGeneric([0u8; L3]);
        A::activate_i32_to_u8(&l2_out.0, &mut l2_relu.0);

        // output 層
        let mut output = [0i32; 1];
        self.output.propagate(&l2_relu.0, &mut output);

        // スケーリング
        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        let eval = output[0] / fv_scale;

        // デバッグ: 最終評価値の範囲チェック
        #[cfg(debug_assertions)]
        debug_assert!(
            eval.abs() < 50_000,
            "Final evaluation {eval} is out of expected range (NetworkHalfKP<{}, {}, {}, {}>). Raw output: {}",
            L1,
            L2,
            L3,
            A::name(),
            output[0]
        );

        Value::new(eval)
    }

    /// 活性化関数の名前を取得
    pub fn activation_name(&self) -> &'static str {
        A::name()
    }

    /// 新しい Accumulator を作成
    pub fn new_accumulator(&self) -> AccumulatorHalfKP<L1> {
        AccumulatorHalfKP::new()
    }

    /// 新しい AccumulatorStack を作成
    pub fn new_accumulator_stack(&self) -> AccumulatorStackHalfKP<L1> {
        AccumulatorStackHalfKP::new()
    }

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> String {
        format!("HalfKP{}x2-{}-{}-{}", L1, L2, L3, A::name())
    }
}

// =============================================================================
// ヘルパー関数
// =============================================================================

/// アーキテクチャ文字列から QA 値をパース
fn parse_qa_from_arch(arch_str: &str) -> Option<i16> {
    if let Some(start) = arch_str.find("qa=") {
        let rest = &arch_str[start + 3..];
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest[..end].parse().ok()
    } else {
        None
    }
}

/// アーキテクチャ文字列から FV_SCALE をパース
fn parse_fv_scale_from_arch(arch_str: &str) -> Option<i32> {
    if let Some(start) = arch_str.find("fv_scale=") {
        let rest = &arch_str[start + 9..];
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest[..end].parse().ok()
    } else {
        None
    }
}

// =============================================================================
// 型エイリアス
// =============================================================================

use super::activation::{CReLU, PairwiseCReLU, SCReLU};

// L1=256: L1_INPUT=512
/// HalfKP 256x2-32-32 CReLU
pub type HalfKP256CReLU = NetworkHalfKP<256, 512, 32, 32, CReLU>;
/// HalfKP 256x2-32-32 SCReLU
pub type HalfKP256SCReLU = NetworkHalfKP<256, 512, 32, 32, SCReLU>;
/// HalfKP 256x2-32-32 PairwiseCReLU
pub type HalfKP256Pairwise = NetworkHalfKP<256, 512, 32, 32, PairwiseCReLU>;

// L1=512: L1_INPUT=1024
/// HalfKP 512x2-8-96 CReLU
pub type HalfKP512CReLU = NetworkHalfKP<512, 1024, 8, 96, CReLU>;
/// HalfKP 512x2-8-96 SCReLU
pub type HalfKP512SCReLU = NetworkHalfKP<512, 1024, 8, 96, SCReLU>;
/// HalfKP 512x2-8-96 PairwiseCReLU
pub type HalfKP512Pairwise = NetworkHalfKP<512, 1024, 8, 96, PairwiseCReLU>;

// L1=512, L2=32, L3=32: L1_INPUT=1024
/// HalfKP 512x2-32-32 CReLU
pub type HalfKP512_32_32CReLU = NetworkHalfKP<512, 1024, 32, 32, CReLU>;

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_halfkp_256() {
        let mut acc = AccumulatorHalfKP::<256>::new();
        assert_eq!(acc.accumulation[0].0.len(), 256);
        assert!(!acc.computed_accumulation);

        acc.accumulation[0].0[0] = 100;
        acc.computed_accumulation = true;

        let cloned = acc.clone();
        assert_eq!(cloned.accumulation[0].0[0], 100);
        assert!(cloned.computed_accumulation);
    }

    #[test]
    fn test_accumulator_halfkp_512() {
        let acc = AccumulatorHalfKP::<512>::new();
        assert_eq!(acc.accumulation[0].0.len(), 512);
    }

    #[test]
    fn test_padded_input() {
        assert_eq!(AffineTransformHalfKP::<512, 32>::PADDED_INPUT, 512);
        assert_eq!(AffineTransformHalfKP::<32, 32>::PADDED_INPUT, 32);
        assert_eq!(AffineTransformHalfKP::<32, 1>::PADDED_INPUT, 32);
    }

    #[test]
    fn test_parse_qa_from_arch() {
        assert_eq!(parse_qa_from_arch("HalfKP256x2-32-32-qa=255"), Some(255));
        assert_eq!(parse_qa_from_arch("HalfKP256x2-32-32-qa=127"), Some(127));
        assert_eq!(parse_qa_from_arch("HalfKP256x2-32-32"), None);
    }

    #[test]
    fn test_parse_fv_scale_from_arch() {
        assert_eq!(parse_fv_scale_from_arch("HalfKP256x2-32-32-fv_scale=24"), Some(24));
        assert_eq!(parse_fv_scale_from_arch("HalfKP256x2-32-32-fv_scale=16"), Some(16));
        assert_eq!(parse_fv_scale_from_arch("HalfKP256x2-32-32"), None);
    }

    #[test]
    fn test_type_aliases() {
        // 型エイリアスがコンパイルできることを確認
        fn _check_halfkp_256_crelu(_: HalfKP256CReLU) {}
        fn _check_halfkp_512_screlu(_: HalfKP512SCReLU) {}
    }
}
