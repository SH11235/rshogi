//! NetworkHalfKAStatic - 静的サイズのNNUEネットワーク
//!
//! HalfKA_hm^ 特徴量を使用し、L1/L2/L3 のサイズをコンパイル時に固定した静的実装。
//! 代表的なアーキテクチャ（512x2-8-96, 1024x2-8-96）に対してパフォーマンスを最適化。
//!
//! # サポートするアーキテクチャ
//!
//! | 型名 | L1 | L2 | L3 | 備考 |
//! |------|-----|-----|-----|------|
//! | `NetworkHalfKA512` | 512 | 8 | 96 | nnue-pytorch標準 |
//! | `NetworkHalfKA1024` | 1024 | 8 | 96 | 大規模ネットワーク |
//!
//! # 動的実装との使い分け
//!
//! - 512x2-8-96, 1024x2-8-96: 静的実装（このモジュール）
//! - その他（256x2-32-32 など）: `NetworkHalfKADynamic`（フォールバック）
//!
//! # 入力次元とFactorization
//!
//! - 入力次元: 73,305 (45キングバケット × 1,629駒入力)
//! - coalesce済みモデル専用（nnue-pytorch serialize.py でエクスポート）
//! - Factorization重みは訓練時にのみ使用（74,934次元）
//! - 訓練中のcheckpoint（Factorizer含む）は自動検出してエラー
//!
//! **重要**: 自己対局評価で誤った結果を得ないために、必ずcoalesce済みモデルを使用すること。
//! serialize.py を使わずにckptから直接変換したモデルは正しく評価されない。

use super::accumulator::{AlignedBox, DirtyPiece, IndexList, MAX_PATH_LENGTH};
use super::constants::{
    FV_SCALE_HALFKA, HALFKA_HM_DIMENSIONS, MAX_ARCH_LEN, NNUE_VERSION_HALFKA, WEIGHT_SCALE_BITS,
};
use super::features::{FeatureSet, HalfKA_hmFeatureSet};
use super::network::{get_fv_scale_override, parse_fv_scale_from_arch};
use crate::position::Position;
use crate::types::{Color, Value};
use std::io::{self, Read, Seek};

// =============================================================================
// SIMD ヘルパー関数（network_halfka_dynamic.rs から移植）
// =============================================================================

/// AVX2用 DPBUSD エミュレーション（u8×i8→i32積和演算）
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

/// AVX2: 8×i32 の水平加算
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

/// SSE2: 4×i32 の水平加算（SSSE3フォールバック用）
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

/// SSSE3用 DPBUSD エミュレーション
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
// ClippedReLU（静的サイズ版）
// =============================================================================

/// 静的サイズ版 ClippedReLU
#[inline]
fn clipped_relu_static<const N: usize>(input: &[i32; N], output: &mut [u8; N]) {
    let mut processed: usize = 0;

    // === AVX2: 32要素ずつ処理 ===
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        let num_chunks = N / 32;
        if num_chunks > 0 {
            unsafe {
                use std::arch::x86_64::*;
                let zero = _mm256_setzero_si256();
                let offsets = _mm256_set_epi32(7, 3, 6, 2, 5, 1, 4, 0);
                let in_ptr = input.as_ptr() as *const __m256i;
                let out_ptr = output.as_mut_ptr() as *mut __m256i;

                for i in 0..num_chunks {
                    let in0 = _mm256_loadu_si256(in_ptr.add(i * 4));
                    let in1 = _mm256_loadu_si256(in_ptr.add(i * 4 + 1));
                    let in2 = _mm256_loadu_si256(in_ptr.add(i * 4 + 2));
                    let in3 = _mm256_loadu_si256(in_ptr.add(i * 4 + 3));

                    let words0 =
                        _mm256_srai_epi16(_mm256_packs_epi32(in0, in1), WEIGHT_SCALE_BITS as i32);
                    let words1 =
                        _mm256_srai_epi16(_mm256_packs_epi32(in2, in3), WEIGHT_SCALE_BITS as i32);

                    let bytes = _mm256_max_epi8(_mm256_packs_epi16(words0, words1), zero);
                    let result = _mm256_permutevar8x32_epi32(bytes, offsets);

                    _mm256_storeu_si256(out_ptr.add(i), result);
                }
            }
            processed = num_chunks * 32;
        }
    }

    // === SSE2: 16要素ずつ処理 ===
    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    {
        let remaining = N - processed;
        let num_chunks = remaining / 16;
        if num_chunks > 0 {
            unsafe {
                use std::arch::x86_64::*;

                #[cfg(target_feature = "sse4.1")]
                let zero = _mm_setzero_si128();
                #[cfg(not(target_feature = "sse4.1"))]
                let k0x80s = _mm_set1_epi8(-128i8);

                let in_ptr = input.as_ptr().add(processed) as *const __m128i;
                let out_ptr = output.as_mut_ptr().add(processed) as *mut __m128i;

                for i in 0..num_chunks {
                    let in0 = _mm_loadu_si128(in_ptr.add(i * 4));
                    let in1 = _mm_loadu_si128(in_ptr.add(i * 4 + 1));
                    let in2 = _mm_loadu_si128(in_ptr.add(i * 4 + 2));
                    let in3 = _mm_loadu_si128(in_ptr.add(i * 4 + 3));

                    let words0 =
                        _mm_srai_epi16(_mm_packs_epi32(in0, in1), WEIGHT_SCALE_BITS as i32);
                    let words1 =
                        _mm_srai_epi16(_mm_packs_epi32(in2, in3), WEIGHT_SCALE_BITS as i32);

                    let packedbytes = _mm_packs_epi16(words0, words1);

                    #[cfg(target_feature = "sse4.1")]
                    let result = _mm_max_epi8(packedbytes, zero);
                    #[cfg(not(target_feature = "sse4.1"))]
                    let result = _mm_subs_epi8(_mm_adds_epi8(packedbytes, k0x80s), k0x80s);

                    _mm_storeu_si128(out_ptr.add(i), result);
                }
            }
            processed += num_chunks * 16;
        }
    }

    // === スカラーフォールバック ===
    for i in processed..N {
        let shifted = input[i] >> WEIGHT_SCALE_BITS;
        output[i] = shifted.clamp(0, 127) as u8;
    }
}

// =============================================================================
// AccumulatorHalfKAStatic - 静的サイズのアキュムレータ
// =============================================================================

/// 静的サイズのアキュムレータ
pub struct AccumulatorHalfKAStatic<const L1: usize> {
    /// アキュムレータバッファ [perspective][L1]
    pub accumulation: [AlignedBox<i16>; 2],
    /// 計算済みフラグ
    pub computed_accumulation: bool,
}

impl<const L1: usize> AccumulatorHalfKAStatic<L1> {
    /// 新規作成
    pub fn new() -> Self {
        Self {
            accumulation: [AlignedBox::new_zeroed(L1), AlignedBox::new_zeroed(L1)],
            computed_accumulation: false,
        }
    }

    /// クリア
    pub fn clear(&mut self) {
        self.accumulation[0].fill(0);
        self.accumulation[1].fill(0);
        self.computed_accumulation = false;
    }
}

impl<const L1: usize> Default for AccumulatorHalfKAStatic<L1> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const L1: usize> Clone for AccumulatorHalfKAStatic<L1> {
    fn clone(&self) -> Self {
        Self {
            accumulation: [self.accumulation[0].clone(), self.accumulation[1].clone()],
            computed_accumulation: self.computed_accumulation,
        }
    }
}

/// 512次元用アキュムレータの型エイリアス
pub type AccumulatorHalfKA512 = AccumulatorHalfKAStatic<512>;

/// 1024次元用アキュムレータの型エイリアス
pub type AccumulatorHalfKA1024 = AccumulatorHalfKAStatic<1024>;

// =============================================================================
// AccumulatorStackHalfKAStatic - アキュムレータスタック
// =============================================================================

/// スタックエントリ
pub struct AccumulatorEntryHalfKAStatic<const L1: usize> {
    pub accumulator: AccumulatorHalfKAStatic<L1>,
    pub dirty_piece: DirtyPiece,
    pub previous: Option<usize>,
}

/// アキュムレータスタック
pub struct AccumulatorStackHalfKAStatic<const L1: usize> {
    entries: Vec<AccumulatorEntryHalfKAStatic<L1>>,
    current_idx: usize,
}

impl<const L1: usize> AccumulatorStackHalfKAStatic<L1> {
    /// 新規作成
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(128);
        entries.push(AccumulatorEntryHalfKAStatic {
            accumulator: AccumulatorHalfKAStatic::new(),
            dirty_piece: DirtyPiece::default(),
            previous: None,
        });
        Self {
            entries,
            current_idx: 0,
        }
    }

    /// 現在のエントリを取得
    pub fn current(&self) -> &AccumulatorEntryHalfKAStatic<L1> {
        &self.entries[self.current_idx]
    }

    /// 現在のエントリを取得（可変）
    pub fn current_mut(&mut self) -> &mut AccumulatorEntryHalfKAStatic<L1> {
        &mut self.entries[self.current_idx]
    }

    /// プッシュ
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        let prev_idx = self.current_idx;
        self.current_idx = self.entries.len();
        self.entries.push(AccumulatorEntryHalfKAStatic {
            accumulator: AccumulatorHalfKAStatic::new(),
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
    pub fn entry_at(&self, idx: usize) -> &AccumulatorEntryHalfKAStatic<L1> {
        &self.entries[idx]
    }

    /// 指定インデックスのエントリを取得（可変）
    pub fn entry_at_mut(&mut self, idx: usize) -> &mut AccumulatorEntryHalfKAStatic<L1> {
        &mut self.entries[idx]
    }

    /// 前回と現在のアキュムレータを取得（可変）
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&AccumulatorHalfKAStatic<L1>, &mut AccumulatorHalfKAStatic<L1>) {
        let current_idx = self.current_idx;
        if prev_idx < current_idx {
            let (left, right) = self.entries.split_at_mut(current_idx);
            (&left[prev_idx].accumulator, &mut right[0].accumulator)
        } else {
            let (left, right) = self.entries.split_at_mut(prev_idx);
            (&right[0].accumulator, &mut left[current_idx].accumulator)
        }
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

impl<const L1: usize> Default for AccumulatorStackHalfKAStatic<L1> {
    fn default() -> Self {
        Self::new()
    }
}

/// 512次元用アキュムレータスタックの型エイリアス
pub type AccumulatorStackHalfKA512 = AccumulatorStackHalfKAStatic<512>;

/// 1024次元用アキュムレータスタックの型エイリアス
pub type AccumulatorStackHalfKA1024 = AccumulatorStackHalfKAStatic<1024>;

// =============================================================================
// FeatureTransformerHalfKAStatic - 静的サイズのFeature Transformer
// =============================================================================

/// 静的サイズのFeature Transformer
pub struct FeatureTransformerHalfKAStatic<const L1: usize> {
    /// バイアス [L1]
    pub biases: Vec<i16>,
    /// 重み [input_dimensions][L1]
    pub weights: AlignedBox<i16>,
}

impl<const L1: usize> FeatureTransformerHalfKAStatic<L1> {
    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let input_dim = HALFKA_HM_DIMENSIONS;

        // バイアスを読み込み
        let mut biases = vec![0i16; L1];
        let mut buf = [0u8; 2];
        for bias in biases.iter_mut() {
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
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKAStatic<L1>) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = &mut acc.accumulation[p];

            accumulation.copy_from_slice(&self.biases);

            let active_indices = HalfKA_hmFeatureSet::collect_active_indices(pos, perspective);
            for &index in active_indices.iter() {
                self.add_weights(accumulation, index);
            }
        }

        acc.computed_accumulation = true;
    }

    /// 差分更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorHalfKAStatic<L1>,
        prev_acc: &AccumulatorHalfKAStatic<L1>,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = HalfKA_hmFeatureSet::needs_refresh(dirty_piece, perspective);

            if reset {
                acc.accumulation[p].copy_from_slice(&self.biases);
                let active_indices = HalfKA_hmFeatureSet::collect_active_indices(pos, perspective);
                for &index in active_indices.iter() {
                    self.add_weights(&mut acc.accumulation[p], index);
                }
            } else {
                let (removed, added) = HalfKA_hmFeatureSet::collect_changed_indices(
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                );

                acc.accumulation[p].copy_from_slice(&prev_acc.accumulation[p]);

                for &index in removed.iter() {
                    self.sub_weights(&mut acc.accumulation[p], index);
                }
                for &index in added.iter() {
                    self.add_weights(&mut acc.accumulation[p], index);
                }
            }
        }

        acc.computed_accumulation = true;
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKAStatic<L1>,
        source_idx: usize,
    ) -> bool {
        let Some(path) = stack.collect_path(source_idx) else {
            return false;
        };

        // source から current へコピー（to_vec() を避けてヒープアロケーション削減）
        {
            let (source_acc, current_acc) = stack.get_prev_and_current_accumulators(source_idx);
            for p in 0..2 {
                current_acc.accumulation[p].copy_from_slice(&source_acc.accumulation[p]);
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
                let (removed, added) = HalfKA_hmFeatureSet::collect_changed_indices(
                    &dirty_piece,
                    perspective,
                    king_sq,
                );

                let p = perspective as usize;
                let accumulation = &mut stack.entry_at_mut(current_idx).accumulator.accumulation[p];

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
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16], index: usize) {
        let offset = index * L1;
        let weights = &self.weights[offset..offset + L1];

        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();
                let num_chunks = L1 / 16;

                for i in 0..num_chunks {
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
                let num_chunks = L1 / 8;

                for i in 0..num_chunks {
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
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16], index: usize) {
        let offset = index * L1;
        let weights = &self.weights[offset..offset + L1];

        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();
                let num_chunks = L1 / 16;

                for i in 0..num_chunks {
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
                let num_chunks = L1 / 8;

                for i in 0..num_chunks {
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

    /// 変換（ClippedReLU適用、SIMD最適化版）
    pub fn transform(
        &self,
        acc: &AccumulatorHalfKAStatic<L1>,
        side_to_move: Color,
        output: &mut [u8],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let zero = _mm256_setzero_si256();
                let max_val = _mm256_set1_epi16(127);

                for (p, &perspective) in perspectives.iter().enumerate() {
                    let out_offset = L1 * p;
                    let accumulation = &acc.accumulation[perspective as usize];
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output.as_mut_ptr().add(out_offset);
                    let num_chunks = L1 / 16;

                    for i in 0..num_chunks {
                        let v = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                        let clamped = _mm256_min_epi16(_mm256_max_epi16(v, zero), max_val);
                        let packed = _mm256_packus_epi16(clamped, clamped);
                        let result = _mm256_permute4x64_epi64(packed, 0b11011000);
                        _mm_storeu_si128(
                            out_ptr.add(i * 16) as *mut __m128i,
                            _mm256_castsi256_si128(result),
                        );
                    }
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
                let zero = _mm_setzero_si128();
                let max_val = _mm_set1_epi16(127);

                for (p, &perspective) in perspectives.iter().enumerate() {
                    let out_offset = L1 * p;
                    let accumulation = &acc.accumulation[perspective as usize];
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output.as_mut_ptr().add(out_offset);
                    let num_chunks = L1 / 16;

                    for i in 0..num_chunks {
                        let v0 = _mm_load_si128(acc_ptr.add(i * 16) as *const __m128i);
                        let v1 = _mm_load_si128(acc_ptr.add(i * 16 + 8) as *const __m128i);

                        let clamped0 = _mm_min_epi16(_mm_max_epi16(v0, zero), max_val);
                        let clamped1 = _mm_min_epi16(_mm_max_epi16(v1, zero), max_val);

                        let packed = _mm_packus_epi16(clamped0, clamped1);
                        _mm_storeu_si128(out_ptr.add(i * 16) as *mut __m128i, packed);
                    }
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for (p, &perspective) in perspectives.iter().enumerate() {
            let out_offset = L1 * p;
            let accumulation = &acc.accumulation[perspective as usize];

            for i in 0..L1 {
                output[out_offset + i] = accumulation[i].clamp(0, 127) as u8;
            }
        }
    }

    /// 変換（ClippedReLU非適用、SCReLU用）
    ///
    /// SCReLU モデルでは ClippedReLU を適用せず、生の i16 値を出力する。
    /// SCReLU 活性化は呼び出し側で別途適用する。
    pub fn transform_raw(
        &self,
        acc: &AccumulatorHalfKAStatic<L1>,
        side_to_move: Color,
        output: &mut [i16],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        for (p, &perspective) in perspectives.iter().enumerate() {
            let out_offset = L1 * p;
            let accumulation = &acc.accumulation[perspective as usize];

            output[out_offset..out_offset + L1].copy_from_slice(accumulation);
        }
    }
}

/// 512次元用Feature Transformerの型エイリアス
pub type FeatureTransformerHalfKA512 = FeatureTransformerHalfKAStatic<512>;

/// 1024次元用Feature Transformerの型エイリアス
pub type FeatureTransformerHalfKA1024 = FeatureTransformerHalfKAStatic<1024>;

// =============================================================================
// AffineTransformStatic - 静的サイズのアフィン変換
// =============================================================================

/// 静的サイズのアフィン変換層
pub struct AffineTransformStatic<const INPUT: usize, const OUTPUT: usize> {
    /// バイアス [OUTPUT]
    pub biases: [i32; OUTPUT],
    /// 重み [OUTPUT][padded_input]
    pub weights: AlignedBox<i8>,
    /// パディング済み入力次元
    padded_input: usize,
}

impl<const INPUT: usize, const OUTPUT: usize> AffineTransformStatic<INPUT, OUTPUT> {
    /// パディング済み入力次元を計算
    const fn padded_input() -> usize {
        INPUT.div_ceil(32) * 32
    }

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let padded_input = Self::padded_input();

        // バイアスを読み込み
        let mut biases = [0i32; OUTPUT];
        let mut buf4 = [0u8; 4];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // 重みを読み込み
        // nnue-pytorch の .nnue ファイルは OUTPUT * padded_input バイトで格納
        // （パディング込みで並んでいる）
        let weight_size = OUTPUT * padded_input;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        let mut row_buf = vec![0u8; padded_input];
        for o in 0..OUTPUT {
            reader.read_exact(&mut row_buf)?;
            for i in 0..padded_input {
                weights[o * padded_input + i] = row_buf[i] as i8;
            }
        }

        Ok(Self {
            biases,
            weights,
            padded_input,
        })
    }

    /// 順伝播（SIMD最適化版）
    pub fn propagate(&self, input: &[u8], output: &mut [i32; OUTPUT]) {
        output.copy_from_slice(&self.biases);

        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                self.propagate_avx2(input, output);
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        ))]
        {
            unsafe {
                self.propagate_ssse3(input, output);
            }
            return;
        }

        #[allow(unreachable_code)]
        for (j, out) in output.iter_mut().enumerate() {
            let weight_offset = j * self.padded_input;
            for (i, &in_val) in input.iter().enumerate().take(INPUT) {
                *out += self.weights[weight_offset + i] as i32 * in_val as i32;
            }
        }
    }

    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    #[inline]
    unsafe fn propagate_avx2(&self, input: &[u8], output: &mut [i32; OUTPUT]) {
        use std::arch::x86_64::*;

        let num_chunks = self.padded_input / 32;
        let input_ptr = input.as_ptr();
        let weight_ptr = self.weights.as_ptr();

        for (j, out) in output.iter_mut().enumerate() {
            let mut acc = _mm256_setzero_si256();
            let row_offset = j * self.padded_input;

            for chunk in 0..num_chunks {
                let in_vec = _mm256_loadu_si256(input_ptr.add(chunk * 32) as *const __m256i);
                let w_vec =
                    _mm256_load_si256(weight_ptr.add(row_offset + chunk * 32) as *const __m256i);
                m256_add_dpbusd_epi32(&mut acc, in_vec, w_vec);
            }

            *out += hsum_i32_avx2(acc);
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "ssse3",
        not(target_feature = "avx2")
    ))]
    #[inline]
    unsafe fn propagate_ssse3(&self, input: &[u8], output: &mut [i32; OUTPUT]) {
        use std::arch::x86_64::*;

        let num_chunks = self.padded_input / 16;
        let input_ptr = input.as_ptr();
        let weight_ptr = self.weights.as_ptr();

        for (j, out) in output.iter_mut().enumerate() {
            let mut acc = _mm_setzero_si128();
            let row_offset = j * self.padded_input;

            for chunk in 0..num_chunks {
                let in_vec = _mm_loadu_si128(input_ptr.add(chunk * 16) as *const __m128i);
                let w_vec =
                    _mm_load_si128(weight_ptr.add(row_offset + chunk * 16) as *const __m128i);
                m128_add_dpbusd_epi32(&mut acc, in_vec, w_vec);
            }

            *out += hsum_i32_sse2(acc);
        }
    }
}

// =============================================================================
// NetworkHalfKA512 - 512x2-8-96 静的ネットワーク
// =============================================================================

/// 512x2-8-96 アーキテクチャの静的実装
pub struct NetworkHalfKA512 {
    /// Feature Transformer (入力 → 512)
    pub feature_transformer: FeatureTransformerHalfKA512,
    /// 隠れ層1: 1024 → 8
    pub l1: AffineTransformStatic<1024, 8>,
    /// 隠れ層2: 8 → 96
    pub l2: AffineTransformStatic<8, 96>,
    /// 出力層: 96 → 1
    pub output: AffineTransformStatic<96, 1>,
    /// SCReLU を使用するかどうか
    ///
    /// arch_string に "-SCReLU" サフィックスが含まれている場合に true。
    /// bullet-shogi で学習した SCReLU モデル用。
    pub use_screlu: bool,
    /// 評価値スケーリング係数
    ///
    /// arch_str に "fv_scale=N" が含まれていればその値、
    /// なければ FV_SCALE_HALFKA (16) をデフォルトとする。
    pub fv_scale: i32,
}

impl NetworkHalfKA512 {
    /// ファイルから読み込み
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        // ヘッダを読み込み
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        if version != 0x7AF32F16 && version != NNUE_VERSION_HALFKA {
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

        // Factorizedモデル（未coalesce）の検出
        // nnue-pytorchのFactorizerは訓練時のみ使用される。
        // serialize.pyで自動的にcoalesceされる。
        // "Factorizer"が含まれる場合は訓練中のcheckpointの可能性がある。
        let arch_str = String::from_utf8_lossy(&arch);
        if arch_str.contains("Factorizer") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported model format: factorized (non-coalesced) HalfKA_hm^ model detected.\n\
                     This engine only supports coalesced models (73,305 dimensions).\n\
                     Factorized models (74,934 dimensions) are for training only.\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     The serialize.py script automatically coalesces factor weights.\n\
                     Architecture string: {arch_str}"
                ),
            ));
        }

        // SCReLU 検出: arch_string に "-SCReLU" が含まれているかチェック
        let use_screlu = arch_str.contains("SCReLU");

        // FV_SCALE 検出: arch_str に "fv_scale=N" が含まれていればその値を使用
        let fv_scale = parse_fv_scale_from_arch(&arch_str).unwrap_or(FV_SCALE_HALFKA);

        // Feature Transformer ハッシュ
        reader.read_exact(&mut buf4)?;

        // Feature Transformer
        let feature_transformer = FeatureTransformerHalfKA512::read(reader)?;

        // FC layers ハッシュ
        reader.read_exact(&mut buf4)?;

        // l1: 1024 → 8
        let l1 = AffineTransformStatic::<1024, 8>::read(reader)?;

        // l2: 8 → 96
        let l2 = AffineTransformStatic::<8, 96>::read(reader)?;

        // output: 96 → 1
        let output = AffineTransformStatic::<96, 1>::read(reader)?;

        Ok(Self {
            feature_transformer,
            l1,
            l2,
            output,
            use_screlu,
            fv_scale,
        })
    }

    /// Accumulator をリフレッシュ
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKA512) {
        self.feature_transformer.refresh_accumulator(pos, acc);
    }

    /// Accumulator を差分更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorHalfKA512,
        prev_acc: &AccumulatorHalfKA512,
    ) {
        self.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc);
    }

    /// 複数手分の差分を適用
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKA512,
        source_idx: usize,
    ) -> bool {
        self.feature_transformer.forward_update_incremental(pos, stack, source_idx)
    }

    /// 評価値を計算
    ///
    /// `use_screlu` フラグに応じて ClippedReLU 版または SCReLU 版を呼び出す。
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorHalfKA512) -> Value {
        if self.use_screlu {
            self.evaluate_screlu(pos, acc)
        } else {
            self.evaluate_clipped_relu(pos, acc)
        }
    }

    /// ClippedReLU 版の評価値計算（従来の実装）
    fn evaluate_clipped_relu(&self, pos: &Position, acc: &AccumulatorHalfKA512) -> Value {
        // Feature Transformer 出力（スタック配列でヒープアロケーション回避）
        let mut transformed = [0u8; 1024];
        self.feature_transformer.transform(acc, pos.side_to_move(), &mut transformed);

        // l1 層
        let mut l1_out = [0i32; 8];
        self.l1.propagate(&transformed, &mut l1_out);

        // デバッグ: L1出力の範囲チェック
        #[cfg(debug_assertions)]
        for (i, &v) in l1_out.iter().enumerate() {
            debug_assert!(
                v.abs() < 1_000_000,
                "L1 output[{i}] = {v} is out of expected range (HalfKA512)"
            );
        }

        // ClippedReLU - l2入力用に32バイトにパディング
        // (l2のpadded_input=32だが、l1_outは8要素しかない)
        let mut l1_relu = [0u8; 32];
        for (i, &v) in l1_out.iter().enumerate() {
            let shifted = v >> WEIGHT_SCALE_BITS;
            l1_relu[i] = shifted.clamp(0, 127) as u8;
        }

        // l2 層
        let mut l2_out = [0i32; 96];
        self.l2.propagate(&l1_relu, &mut l2_out);

        // デバッグ: L2出力の範囲チェック
        #[cfg(debug_assertions)]
        for (i, &v) in l2_out.iter().enumerate() {
            debug_assert!(
                v.abs() < 1_000_000,
                "L2 output[{i}] = {v} is out of expected range (HalfKA512)"
            );
        }

        // ClippedReLU
        let mut l2_relu = [0u8; 96];
        clipped_relu_static(&l2_out, &mut l2_relu);

        // output 層
        let mut output = [0i32; 1];
        self.output.propagate(&l2_relu, &mut output);

        // スケーリング（オーバーライド設定があればそちらを優先）
        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        let eval = output[0] / fv_scale;

        // デバッグ: 最終評価値の範囲チェック
        #[cfg(debug_assertions)]
        debug_assert!(
            eval.abs() < 50_000,
            "Final evaluation {eval} is out of expected range (HalfKA512). Raw output: {}",
            output[0]
        );

        Value::new(eval)
    }

    /// SCReLU 版の評価値計算
    ///
    /// bullet-shogi で学習した SCReLU モデル用。
    fn evaluate_screlu(&self, pos: &Position, acc: &AccumulatorHalfKA512) -> Value {
        use super::layers::SCReLU;

        // Feature Transformer 出力（生のi16値）
        let mut ft_out_i16 = [0i16; 1024];
        self.feature_transformer.transform_raw(acc, pos.side_to_move(), &mut ft_out_i16);

        // SCReLU 適用 (i16 → u8)
        let mut transformed = [0u8; 1024];
        SCReLU::<1024>::propagate_i16_to_u8(&ft_out_i16, &mut transformed);

        // l1 層
        let mut l1_out = [0i32; 8];
        self.l1.propagate(&transformed, &mut l1_out);

        // SCReLU (i32 → u8) - l2入力用に32バイトにパディング
        let mut l1_relu = [0u8; 32];
        let mut l1_screlu = [0u8; 8];
        SCReLU::<8>::propagate_i32_to_u8(&l1_out, &mut l1_screlu);
        l1_relu[..8].copy_from_slice(&l1_screlu);

        // l2 層
        let mut l2_out = [0i32; 96];
        self.l2.propagate(&l1_relu, &mut l2_out);

        // SCReLU (i32 → u8)
        let mut l2_relu = [0u8; 96];
        SCReLU::<96>::propagate_i32_to_u8(&l2_out, &mut l2_relu);

        // output 層
        let mut output = [0i32; 1];
        self.output.propagate(&l2_relu, &mut output);

        // スケーリング（オーバーライド設定があればそちらを優先）
        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        let eval = output[0] / fv_scale;

        Value::new(eval)
    }

    /// SCReLU を使用しているかどうか
    pub fn is_screlu(&self) -> bool {
        self.use_screlu
    }

    /// 新しい Accumulator を作成
    pub fn new_accumulator(&self) -> AccumulatorHalfKA512 {
        AccumulatorHalfKA512::new()
    }

    /// 新しい AccumulatorStack を作成
    pub fn new_accumulator_stack(&self) -> AccumulatorStackHalfKA512 {
        AccumulatorStackHalfKA512::new()
    }
}

// =============================================================================
// NetworkHalfKA1024 - 1024x2-8-96 静的ネットワーク
// =============================================================================

/// 1024x2-8-96 アーキテクチャの静的実装
pub struct NetworkHalfKA1024 {
    /// Feature Transformer (入力 → 1024)
    pub feature_transformer: FeatureTransformerHalfKA1024,
    /// 隠れ層1: 2048 → 8
    pub l1: AffineTransformStatic<2048, 8>,
    /// 隠れ層2: 8 → 96
    pub l2: AffineTransformStatic<8, 96>,
    /// 出力層: 96 → 1
    pub output: AffineTransformStatic<96, 1>,
    /// SCReLU を使用するかどうか
    ///
    /// arch_string に "-SCReLU" サフィックスが含まれている場合に true。
    /// bullet-shogi で学習した SCReLU モデル用。
    pub use_screlu: bool,
    /// 評価値スケーリング係数
    ///
    /// arch_str に "fv_scale=N" が含まれていればその値、
    /// なければ FV_SCALE_HALFKA (16) をデフォルトとする。
    pub fv_scale: i32,
}

impl NetworkHalfKA1024 {
    /// ファイルから読み込み
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        // ヘッダを読み込み
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        if version != 0x7AF32F16 && version != NNUE_VERSION_HALFKA {
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

        // Factorizedモデル（未coalesce）の検出
        // nnue-pytorchのFactorizerは訓練時のみ使用される。
        // serialize.pyで自動的にcoalesceされる。
        // "Factorizer"が含まれる場合は訓練中のcheckpointの可能性がある。
        let arch_str = String::from_utf8_lossy(&arch);
        if arch_str.contains("Factorizer") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported model format: factorized (non-coalesced) HalfKA_hm^ model detected.\n\
                     This engine only supports coalesced models (73,305 dimensions).\n\
                     Factorized models (74,934 dimensions) are for training only.\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     The serialize.py script automatically coalesces factor weights.\n\
                     Architecture string: {arch_str}"
                ),
            ));
        }

        // SCReLU 検出: arch_string に "-SCReLU" が含まれているかチェック
        let use_screlu = arch_str.contains("SCReLU");

        // FV_SCALE 検出: arch_str に "fv_scale=N" が含まれていればその値を使用
        let fv_scale = parse_fv_scale_from_arch(&arch_str).unwrap_or(FV_SCALE_HALFKA);

        // Feature Transformer ハッシュ
        reader.read_exact(&mut buf4)?;

        // Feature Transformer
        let feature_transformer = FeatureTransformerHalfKA1024::read(reader)?;

        // FC layers ハッシュ
        reader.read_exact(&mut buf4)?;

        // l1: 2048 → 8
        let l1 = AffineTransformStatic::<2048, 8>::read(reader)?;

        // l2: 8 → 96
        let l2 = AffineTransformStatic::<8, 96>::read(reader)?;

        // output: 96 → 1
        let output = AffineTransformStatic::<96, 1>::read(reader)?;

        Ok(Self {
            feature_transformer,
            l1,
            l2,
            output,
            use_screlu,
            fv_scale,
        })
    }

    /// Accumulator をリフレッシュ
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKA1024) {
        self.feature_transformer.refresh_accumulator(pos, acc);
    }

    /// Accumulator を差分更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorHalfKA1024,
        prev_acc: &AccumulatorHalfKA1024,
    ) {
        self.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc);
    }

    /// 複数手分の差分を適用
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKA1024,
        source_idx: usize,
    ) -> bool {
        self.feature_transformer.forward_update_incremental(pos, stack, source_idx)
    }

    /// 評価値を計算
    ///
    /// `use_screlu` フラグに応じて ClippedReLU 版または SCReLU 版を呼び出す。
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorHalfKA1024) -> Value {
        if self.use_screlu {
            self.evaluate_screlu(pos, acc)
        } else {
            self.evaluate_clipped_relu(pos, acc)
        }
    }

    /// ClippedReLU 版の評価値計算（従来の実装）
    fn evaluate_clipped_relu(&self, pos: &Position, acc: &AccumulatorHalfKA1024) -> Value {
        // Feature Transformer 出力（スタック配列でヒープアロケーション回避）
        let mut transformed = [0u8; 2048];
        self.feature_transformer.transform(acc, pos.side_to_move(), &mut transformed);

        // l1 層
        let mut l1_out = [0i32; 8];
        self.l1.propagate(&transformed, &mut l1_out);

        // デバッグ: L1出力の範囲チェック
        #[cfg(debug_assertions)]
        for (i, &v) in l1_out.iter().enumerate() {
            debug_assert!(
                v.abs() < 1_000_000,
                "L1 output[{i}] = {v} is out of expected range (HalfKA1024)"
            );
        }

        // ClippedReLU - l2入力用に32バイトにパディング
        // (l2のpadded_input=32だが、l1_outは8要素しかない)
        let mut l1_relu = [0u8; 32];
        for (i, &v) in l1_out.iter().enumerate() {
            let shifted = v >> WEIGHT_SCALE_BITS;
            l1_relu[i] = shifted.clamp(0, 127) as u8;
        }

        // l2 層
        let mut l2_out = [0i32; 96];
        self.l2.propagate(&l1_relu, &mut l2_out);

        // デバッグ: L2出力の範囲チェック
        #[cfg(debug_assertions)]
        for (i, &v) in l2_out.iter().enumerate() {
            debug_assert!(
                v.abs() < 1_000_000,
                "L2 output[{i}] = {v} is out of expected range (HalfKA1024)"
            );
        }

        // ClippedReLU
        let mut l2_relu = [0u8; 96];
        clipped_relu_static(&l2_out, &mut l2_relu);

        // output 層
        let mut output = [0i32; 1];
        self.output.propagate(&l2_relu, &mut output);

        // スケーリング（オーバーライド設定があればそちらを優先）
        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        let eval = output[0] / fv_scale;

        // デバッグ: 最終評価値の範囲チェック
        #[cfg(debug_assertions)]
        debug_assert!(
            eval.abs() < 50_000,
            "Final evaluation {eval} is out of expected range (HalfKA1024). Raw output: {}",
            output[0]
        );

        Value::new(eval)
    }

    /// SCReLU 版の評価値計算
    ///
    /// bullet-shogi で学習した SCReLU モデル用。
    fn evaluate_screlu(&self, pos: &Position, acc: &AccumulatorHalfKA1024) -> Value {
        use super::layers::SCReLU;

        // Feature Transformer 出力（生のi16値）
        let mut ft_out_i16 = [0i16; 2048];
        self.feature_transformer.transform_raw(acc, pos.side_to_move(), &mut ft_out_i16);

        // SCReLU 適用 (i16 → u8)
        let mut transformed = [0u8; 2048];
        SCReLU::<2048>::propagate_i16_to_u8(&ft_out_i16, &mut transformed);

        // l1 層
        let mut l1_out = [0i32; 8];
        self.l1.propagate(&transformed, &mut l1_out);

        // SCReLU (i32 → u8) - l2入力用に32バイトにパディング
        let mut l1_relu = [0u8; 32];
        let mut l1_screlu = [0u8; 8];
        SCReLU::<8>::propagate_i32_to_u8(&l1_out, &mut l1_screlu);
        l1_relu[..8].copy_from_slice(&l1_screlu);

        // l2 層
        let mut l2_out = [0i32; 96];
        self.l2.propagate(&l1_relu, &mut l2_out);

        // SCReLU (i32 → u8)
        let mut l2_relu = [0u8; 96];
        SCReLU::<96>::propagate_i32_to_u8(&l2_out, &mut l2_relu);

        // output 層
        let mut output = [0i32; 1];
        self.output.propagate(&l2_relu, &mut output);

        // スケーリング（オーバーライド設定があればそちらを優先）
        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        let eval = output[0] / fv_scale;

        Value::new(eval)
    }

    /// SCReLU を使用しているかどうか
    pub fn is_screlu(&self) -> bool {
        self.use_screlu
    }

    /// 新しい Accumulator を作成
    pub fn new_accumulator(&self) -> AccumulatorHalfKA1024 {
        AccumulatorHalfKA1024::new()
    }

    /// 新しい AccumulatorStack を作成
    pub fn new_accumulator_stack(&self) -> AccumulatorStackHalfKA1024 {
        AccumulatorStackHalfKA1024::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_halfka_static_512() {
        let mut acc = AccumulatorHalfKA512::new();
        assert_eq!(acc.accumulation[0].len(), 512);
        assert!(!acc.computed_accumulation);

        acc.accumulation[0][0] = 100;
        acc.computed_accumulation = true;

        let cloned = acc.clone();
        assert_eq!(cloned.accumulation[0][0], 100);
        assert!(cloned.computed_accumulation);
    }

    #[test]
    fn test_accumulator_halfka_static_1024() {
        let mut acc = AccumulatorHalfKA1024::new();
        assert_eq!(acc.accumulation[0].len(), 1024);
        assert!(!acc.computed_accumulation);

        acc.accumulation[0][0] = 200;
        acc.computed_accumulation = true;

        let cloned = acc.clone();
        assert_eq!(cloned.accumulation[0][0], 200);
        assert!(cloned.computed_accumulation);
    }

    #[test]
    fn test_padded_input() {
        assert_eq!(AffineTransformStatic::<8, 96>::padded_input(), 32);
        assert_eq!(AffineTransformStatic::<32, 96>::padded_input(), 32);
        assert_eq!(AffineTransformStatic::<33, 96>::padded_input(), 64);
        assert_eq!(AffineTransformStatic::<96, 1>::padded_input(), 96);
        assert_eq!(AffineTransformStatic::<1024, 8>::padded_input(), 1024);
        assert_eq!(AffineTransformStatic::<2048, 8>::padded_input(), 2048);
    }

    #[test]
    fn test_clipped_relu_static_basic() {
        // WEIGHT_SCALE_BITS = 6 なので、64で割った結果が出力される
        // 入力: [0, 64, 128, 8192, -64, -128]
        // 期待: [0, 1, 2, 127, 0, 0] (負は0にクランプ、127超は127にクランプ)
        let input: [i32; 8] = [0, 64, 128, 8192, -64, -128, 64 * 100, 64 * 127];
        let mut output = [0u8; 8];
        clipped_relu_static(&input, &mut output);

        assert_eq!(output[0], 0); // 0 >> 6 = 0
        assert_eq!(output[1], 1); // 64 >> 6 = 1
        assert_eq!(output[2], 2); // 128 >> 6 = 2
        assert_eq!(output[3], 127); // 8192 >> 6 = 128 -> clamped to 127
        assert_eq!(output[4], 0); // -64 >> 6 = -1 -> clamped to 0
        assert_eq!(output[5], 0); // -128 >> 6 = -2 -> clamped to 0
        assert_eq!(output[6], 100); // 6400 >> 6 = 100
        assert_eq!(output[7], 127); // 8128 >> 6 = 127
    }

    #[test]
    fn test_clipped_relu_static_96_elements() {
        // 96要素のテスト（L2層で使用）
        let mut input = [0i32; 96];
        for (i, val) in input.iter_mut().enumerate() {
            *val = (i as i32) * 64; // 0, 64, 128, ...
        }
        let mut output = [0u8; 96];
        clipped_relu_static(&input, &mut output);

        for (i, &actual) in output.iter().enumerate() {
            let expected = (i as u8).min(127);
            assert_eq!(
                actual, expected,
                "Mismatch at index {i}: expected {expected}, got {actual}",
            );
        }
    }

    #[test]
    fn test_affine_transform_static_propagate() {
        // 簡単な 4 -> 2 のアフィン変換テスト
        // バイアス: [100, 200]
        // 重み: [[1, 2, 0, 0, ...], [3, 4, 0, 0, ...]] (padded to 32)
        // 入力: [10, 20, 0, 0]
        // 期待出力: [100 + 1*10 + 2*20, 200 + 3*10 + 4*20] = [150, 310]

        let padded_input = AffineTransformStatic::<4, 2>::padded_input();
        assert_eq!(padded_input, 32);

        let mut weights = AlignedBox::new_zeroed(2 * padded_input);
        // Row 0: [1, 2, 0, 0, ...]
        weights[0] = 1;
        weights[1] = 2;
        // Row 1: [3, 4, 0, 0, ...]
        weights[padded_input] = 3;
        weights[padded_input + 1] = 4;

        let layer = AffineTransformStatic::<4, 2> {
            biases: [100, 200],
            weights,
            padded_input,
        };

        let mut input = [0u8; 32];
        input[0] = 10;
        input[1] = 20;

        let mut output = [0i32; 2];
        layer.propagate(&input, &mut output);

        assert_eq!(output[0], 150); // 100 + 1*10 + 2*20 = 150
        assert_eq!(output[1], 310); // 200 + 3*10 + 4*20 = 310
    }

    #[test]
    fn test_l1_relu_with_weight_scale_bits() {
        // L1出力からL2入力への変換をテスト
        // WEIGHT_SCALE_BITS = 6 でシフトが正しく適用されることを確認
        let l1_out: [i32; 8] = [
            64,    // -> 1
            128,   // -> 2
            0,     // -> 0
            -64,   // -> 0 (clamped)
            8128,  // -> 127
            10000, // -> 127 (clamped from 156)
            3200,  // -> 50
            6400,  // -> 100
        ];

        let mut l1_relu = [0u8; 32];
        for (i, &v) in l1_out.iter().enumerate() {
            let shifted = v >> WEIGHT_SCALE_BITS;
            l1_relu[i] = shifted.clamp(0, 127) as u8;
        }

        assert_eq!(l1_relu[0], 1);
        assert_eq!(l1_relu[1], 2);
        assert_eq!(l1_relu[2], 0);
        assert_eq!(l1_relu[3], 0); // 負の値は0にクランプ
        assert_eq!(l1_relu[4], 127);
        assert_eq!(l1_relu[5], 127); // 127超は127にクランプ
        assert_eq!(l1_relu[6], 50);
        assert_eq!(l1_relu[7], 100);
        // パディング部分は0
        assert_eq!(l1_relu[8], 0);
        assert_eq!(l1_relu[31], 0);
    }
}
