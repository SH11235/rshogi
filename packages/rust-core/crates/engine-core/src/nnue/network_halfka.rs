// NOTE: 公式表記(HalfKA)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

//! NetworkHalfKA - const generics ベースの HalfKA ネットワーク統一実装
//!
//! HalfKA 特徴量を使用し、L1/L2/L3 のサイズと活性化関数を型パラメータで切り替え可能にした実装。
//!
//! # 設計
//!
//! ```text
//! Network<L1, L2, L3, A>
//!   L1: FT出力次元（片側）
//!   L2: 隠れ層1の出力次元
//!   L3: 隠れ層2の出力次元
//!   A: FtActivation trait を実装する活性化関数型
//! ```
//!
//! # サポートするアーキテクチャ
//!
//! | 型エイリアス | L1 | L2 | L3 | 活性化 |
//! |-------------|------|-----|-----|--------|
//! | HalfKA256CReLU | 256 | 32 | 32 | CReLU |
//! | HalfKA256SCReLU | 256 | 32 | 32 | SCReLU |
//! | HalfKA256Pairwise | 256 | 32 | 32 | PairwiseCReLU |
//! | HalfKA512CReLU | 512 | 8 | 96 | CReLU |
//! | HalfKA512SCReLU | 512 | 8 | 96 | SCReLU |
//! | HalfKA512Pairwise | 512 | 8 | 96 | PairwiseCReLU |
//! | HalfKA512_32_32CReLU | 512 | 32 | 32 | CReLU |
//! | HalfKA512_32_32SCReLU | 512 | 32 | 32 | SCReLU |
//! | HalfKA512_32_32Pairwise | 512 | 32 | 32 | PairwiseCReLU |
//! | HalfKA1024CReLU | 1024 | 8 | 96 | CReLU |
//! | HalfKA1024SCReLU | 1024 | 8 | 96 | SCReLU |
//! | HalfKA1024Pairwise | 1024 | 8 | 96 | PairwiseCReLU |
//! | HalfKA1024_8_32CReLU | 1024 | 8 | 32 | CReLU |
//! | HalfKA1024_8_32SCReLU | 1024 | 8 | 32 | SCReLU |
//! | HalfKA1024_8_32Pairwise | 1024 | 8 | 32 | PairwiseCReLU |
//!
//! # 特徴量
//!
//! - 入力次元: 138,510 (81キング位置 × 1,710駒入力)
//! - coalesce済みモデル専用（nnue-pytorch serialize.py でエクスポート）

use std::io::{self, Read, Seek};
use std::marker::PhantomData;
use std::sync::OnceLock;

use super::accumulator::{Aligned, AlignedBox, DirtyPiece, IndexList, MAX_PATH_LENGTH};
use super::activation::FtActivation;
use super::constants::{FV_SCALE_HALFKA, HALFKA_DIMENSIONS, MAX_ARCH_LEN, NNUE_VERSION_HALFKA};
use super::features::{FeatureSet, HalfKAFeatureSet};
use super::network::{get_fv_scale_override, parse_fv_scale_from_arch};
use crate::position::Position;
use crate::types::{Color, Value};

#[inline]
fn nnue_debug_enabled() -> bool {
    static NNUE_DEBUG: OnceLock<bool> = OnceLock::new();
    *NNUE_DEBUG.get_or_init(|| std::env::var("NNUE_DEBUG").is_ok())
}

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
// AccumulatorHalfKA - const generics 版アキュムレータ
// =============================================================================

/// HalfKA アキュムレータ
/// HalfKA アキュムレータ
///
/// # 最適化に関する注意
///
/// 現在の実装では `accumulation` に `AlignedBox<i16>`（動的ヒープメモリ）を使用している。
/// `add_weights`/`sub_weights` に渡される引数が `&mut [i16]`（スライス）となるため、
/// 固定サイズ配列を使用する場合と比較してコンパイラ最適化が効きにくい。
///
/// ただし、HalfKA系（L1=512, 1024）ではL1サイズが大きいため境界チェックの
/// 相対的オーバーヘッドが小さく、ベンチマーク上は改善が見られる。
///
/// ## 改善案（オプション）
///
/// さらなる最適化が必要な場合、固定サイズ配列への変更を検討：
/// ```ignore
/// pub accumulation: [Box<[i16; L1]>; 2],
/// ```
///
/// 参考: HalfKP（L1=256）では元のfeature_transformer.rsが `Aligned<[i16; 256]>` を
/// 使用していたため、動的スライスへの変更で約17%の性能低下が見られた。
pub struct AccumulatorHalfKA<const L1: usize> {
    /// アキュムレータバッファ [perspective][L1]
    pub accumulation: [AlignedBox<i16>; 2],
    /// 計算済みフラグ
    pub computed_accumulation: bool,
}

impl<const L1: usize> AccumulatorHalfKA<L1> {
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

impl<const L1: usize> Default for AccumulatorHalfKA<L1> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const L1: usize> Clone for AccumulatorHalfKA<L1> {
    fn clone(&self) -> Self {
        Self {
            accumulation: [self.accumulation[0].clone(), self.accumulation[1].clone()],
            computed_accumulation: self.computed_accumulation,
        }
    }
}

// =============================================================================
// AccumulatorStackHalfKA - アキュムレータスタック
// =============================================================================

/// スタックエントリ
pub struct AccumulatorEntryHalfKA<const L1: usize> {
    pub accumulator: AccumulatorHalfKA<L1>,
    pub dirty_piece: DirtyPiece,
    pub previous: Option<usize>,
}

/// アキュムレータスタック
pub struct AccumulatorStackHalfKA<const L1: usize> {
    entries: Vec<AccumulatorEntryHalfKA<L1>>,
    current_idx: usize,
}

impl<const L1: usize> AccumulatorStackHalfKA<L1> {
    /// 新規作成
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(128);
        entries.push(AccumulatorEntryHalfKA {
            accumulator: AccumulatorHalfKA::new(),
            dirty_piece: DirtyPiece::default(),
            previous: None,
        });
        Self {
            entries,
            current_idx: 0,
        }
    }

    /// 現在のエントリを取得
    pub fn current(&self) -> &AccumulatorEntryHalfKA<L1> {
        &self.entries[self.current_idx]
    }

    /// 現在のエントリを取得（可変）
    pub fn current_mut(&mut self) -> &mut AccumulatorEntryHalfKA<L1> {
        &mut self.entries[self.current_idx]
    }

    /// 現在の Accumulator を取得
    ///
    /// `define_l1_variants!` マクロから使用される。
    #[inline]
    pub fn top(&self) -> &AccumulatorHalfKA<L1> {
        &self.entries[self.current_idx].accumulator
    }

    /// 現在の Accumulator を取得（可変）
    ///
    /// `define_l1_variants!` マクロから使用される。
    #[inline]
    pub fn top_mut(&mut self) -> &mut AccumulatorHalfKA<L1> {
        &mut self.entries[self.current_idx].accumulator
    }

    /// 現在と source の Accumulator を同時取得（差分更新用）
    ///
    /// # 引数
    /// - `source_idx`: source エントリの絶対インデックス
    ///
    /// # 戻り値
    /// `(現在の Accumulator への可変参照, source の Accumulator への不変参照)`
    ///
    /// # 契約
    /// - `source_idx < self.current_idx` でなければならない
    /// - 範囲外の場合は panic（ホットパスなので Option 不使用）
    ///
    /// `define_l1_variants!` マクロから使用される。
    #[inline]
    pub fn top_and_source(
        &mut self,
        source_idx: usize,
    ) -> (&mut AccumulatorHalfKA<L1>, &AccumulatorHalfKA<L1>) {
        let current_idx = self.current_idx;
        debug_assert!(
            source_idx < current_idx,
            "source_idx ({source_idx}) must be < current_idx ({current_idx})"
        );
        let (left, right) = self.entries.split_at_mut(current_idx);
        (&mut right[0].accumulator, &left[source_idx].accumulator)
    }

    /// プッシュ
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        let prev_idx = self.current_idx;
        self.current_idx = self.entries.len();
        self.entries.push(AccumulatorEntryHalfKA {
            accumulator: AccumulatorHalfKA::new(),
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
    pub fn entry_at(&self, idx: usize) -> &AccumulatorEntryHalfKA<L1> {
        &self.entries[idx]
    }

    /// 指定インデックスのエントリを取得（可変）
    pub fn entry_at_mut(&mut self, idx: usize) -> &mut AccumulatorEntryHalfKA<L1> {
        &mut self.entries[idx]
    }

    /// 前回と現在のアキュムレータを取得（可変）
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&AccumulatorHalfKA<L1>, &mut AccumulatorHalfKA<L1>) {
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

impl<const L1: usize> Default for AccumulatorStackHalfKA<L1> {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// FeatureTransformerHalfKA - const generics 版 Feature Transformer
// =============================================================================

/// HalfKA Feature Transformer
pub struct FeatureTransformerHalfKA<const L1: usize> {
    /// バイアス [L1]
    pub biases: Vec<i16>,
    /// 重み [input_dimensions][L1]
    pub weights: AlignedBox<i16>,
}

impl<const L1: usize> FeatureTransformerHalfKA<L1> {
    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let input_dim = HALFKA_DIMENSIONS;

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
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKA<L1>) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = &mut acc.accumulation[p];

            accumulation.copy_from_slice(&self.biases);

            let active_indices = HalfKAFeatureSet::collect_active_indices(pos, perspective);
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
        acc: &mut AccumulatorHalfKA<L1>,
        prev_acc: &AccumulatorHalfKA<L1>,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = HalfKAFeatureSet::needs_refresh(dirty_piece, perspective);

            if reset {
                acc.accumulation[p].copy_from_slice(&self.biases);
                let active_indices = HalfKAFeatureSet::collect_active_indices(pos, perspective);
                for &index in active_indices.iter() {
                    self.add_weights(&mut acc.accumulation[p], index);
                }
            } else {
                let (removed, added) = HalfKAFeatureSet::collect_changed_indices(
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
        stack: &mut AccumulatorStackHalfKA<L1>,
        source_idx: usize,
    ) -> bool {
        let Some(path) = stack.collect_path(source_idx) else {
            return false;
        };

        // source から current へコピー
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
                let (removed, added) =
                    HalfKAFeatureSet::collect_changed_indices(&dirty_piece, perspective, king_sq);

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

    /// 変換（生の i16 出力）
    ///
    /// 活性化関数は呼び出し側で適用する。
    pub fn transform_raw(
        &self,
        acc: &AccumulatorHalfKA<L1>,
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

// =============================================================================
// AffineTransformHalfKA - const generics 版アフィン変換（ループ逆転最適化版）
// =============================================================================

/// アフィン変換層（ループ逆転最適化 + スクランブル重み形式）
///
/// YaneuraOu/Stockfish スタイルの SIMD 最適化を実装。
/// 重みはスクランブル形式 `weights[input_chunk][output][4]` で保持し、
/// ループ逆転により入力をブロードキャストして全出力に同時適用する。
pub struct AffineTransformHalfKA<const INPUT: usize, const OUTPUT: usize> {
    /// バイアス [OUTPUT]
    pub biases: [i32; OUTPUT],
    /// 重み（スクランブル形式、64バイトアライン）
    pub weights: AlignedBox<i8>,
}

impl<const INPUT: usize, const OUTPUT: usize> AffineTransformHalfKA<INPUT, OUTPUT> {
    /// パディング済み入力次元（32の倍数）
    const PADDED_INPUT: usize = INPUT.div_ceil(32) * 32;

    // SIMD最適化用の定数・メソッド（AVX2/SSSE3環境でのみコンパイル）
    #[cfg(any(target_feature = "avx2", target_feature = "ssse3"))]
    /// チャンクサイズ（u8×4 = i32として読む単位）
    const CHUNK_SIZE: usize = 4;

    #[cfg(any(target_feature = "avx2", target_feature = "ssse3"))]
    /// 入力チャンク数
    const NUM_INPUT_CHUNKS: usize = Self::PADDED_INPUT / Self::CHUNK_SIZE;

    #[cfg(any(target_feature = "avx2", target_feature = "ssse3"))]
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

    #[cfg(any(target_feature = "avx2", target_feature = "ssse3"))]
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
// NetworkHalfKA - const generics 版統一ネットワーク
// =============================================================================

/// HalfKA ネットワーク（const generics 版）
///
/// # 型パラメータ
/// - `L1`: FT出力次元（片側）
/// - `FT_OUT`: FT出力次元（両視点連結、常に L1 * 2）
/// - `L1_INPUT`: L1層の入力次元
///   - CReLU/SCReLU: L1 * 2（活性化後も次元維持）
///   - Pairwise: L1（Pairwise乗算で次元半減）
/// - `L2`: 隠れ層1の出力次元
/// - `L3`: 隠れ層2の出力次元
/// - `A`: 活性化関数（FtActivation trait を実装する型）
pub struct NetworkHalfKA<
    const L1: usize,
    const FT_OUT: usize,
    const L1_INPUT: usize,
    const L2: usize,
    const L3: usize,
    A: FtActivation,
> {
    /// Feature Transformer (入力 → L1)
    pub feature_transformer: FeatureTransformerHalfKA<L1>,
    /// 隠れ層1: L1_INPUT → L2
    pub l1: AffineTransformHalfKA<L1_INPUT, L2>,
    /// 隠れ層2: L2 → L3
    pub l2: AffineTransformHalfKA<L2, L3>,
    /// 出力層: L3 → 1
    pub output: AffineTransformHalfKA<L3, 1>,
    /// 評価値スケーリング係数
    pub fv_scale: i32,
    /// QA値（クリッピング閾値）
    pub qa: i16,
    /// 活性化関数（型情報のみ）
    _activation: PhantomData<A>,
}

impl<
        const L1: usize,
        const FT_OUT: usize,
        const L1_INPUT: usize,
        const L2: usize,
        const L3: usize,
        A: FtActivation,
    > NetworkHalfKA<L1, FT_OUT, L1_INPUT, L2, L3, A>
{
    /// コンパイル時制約
    ///
    /// - `FT_OUT == L1 * 2`: FT出力は常に両視点の連結
    /// - `L1_INPUT`:
    ///   - CReLU/SCReLU: `L1 * 2`（活性化後も次元維持）
    ///   - Pairwise: `L1`（Pairwise乗算で次元半減）
    const _ASSERT_DIMS: () = {
        assert!(FT_OUT == L1 * 2, "FT_OUT must equal L1 * 2");
        assert!(
            L1_INPUT == L1 * 2 || L1_INPUT == L1,
            "L1_INPUT must equal L1 * 2 (CReLU/SCReLU) or L1 (Pairwise)"
        );
    };

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

        let arch_str = String::from_utf8_lossy(&arch);

        // Factorizedモデル（未coalesce）の検出
        if arch_str.contains("Factorizer") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported model format: factorized (non-coalesced) HalfKA^ model detected.\n\
                     This engine only supports coalesced models (138,510 dimensions).\n\
                     Factorized models are for training only.\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     The serialize.py script automatically coalesces factor weights.\n\
                     Architecture string: {arch_str}"
                ),
            ));
        }

        // FV_SCALE 検出
        let fv_scale = parse_fv_scale_from_arch(&arch_str).unwrap_or(FV_SCALE_HALFKA);

        // QA 検出（デフォルト 127）
        let qa = parse_qa_from_arch(&arch_str).unwrap_or(127);

        // Feature Transformer ハッシュ
        reader.read_exact(&mut buf4)?;

        // Feature Transformer
        let feature_transformer = FeatureTransformerHalfKA::read(reader)?;

        // FC layers ハッシュ
        reader.read_exact(&mut buf4)?;

        // l1: L1*2 → L2
        let l1 = AffineTransformHalfKA::read(reader)?;

        // l2: L2 → L3
        let l2 = AffineTransformHalfKA::read(reader)?;

        // output: L3 → 1
        let output = AffineTransformHalfKA::read(reader)?;

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
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKA<L1>) {
        self.feature_transformer.refresh_accumulator(pos, acc);
    }

    /// Accumulator を差分更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorHalfKA<L1>,
        prev_acc: &AccumulatorHalfKA<L1>,
    ) {
        self.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc);
    }

    /// 複数手分の差分を適用
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKA<L1>,
        source_idx: usize,
    ) -> bool {
        self.feature_transformer.forward_update_incremental(pos, stack, source_idx)
    }

    /// 評価値を計算
    ///
    /// 最適化: スタック配列 + 64バイトアラインメントで SIMD 効率を最大化
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorHalfKA<L1>) -> Value {
        let debug = nnue_debug_enabled();

        // Feature Transformer 出力（生のi16値）- 64バイトアライン
        // FT出力は常に FT_OUT（= L1 * 2、両視点の連結）
        let mut ft_out_i16 = Aligned([0i16; FT_OUT]);
        self.feature_transformer
            .transform_raw(acc, pos.side_to_move(), &mut ft_out_i16.0);

        if debug {
            let ft_min = ft_out_i16.0.iter().min().copied().unwrap_or(0);
            let ft_max = ft_out_i16.0.iter().max().copied().unwrap_or(0);
            let ft_sum: i64 = ft_out_i16.0.iter().map(|&x| x as i64).sum();
            eprintln!(
                "[DEBUG] FT output: min={ft_min}, max={ft_max}, sum={ft_sum}, len={}",
                ft_out_i16.0.len()
            );
            eprintln!("[DEBUG] FT[0..8]: {:?}", &ft_out_i16.0[0..8]);
        }

        // 活性化関数適用 (i16 → u8) - 64バイトアライン
        // 活性化後のサイズは L1_INPUT（CReLU: L1*2、Pairwise: L1）
        let mut transformed = Aligned([0u8; L1_INPUT]);
        A::activate_i16_to_u8(&ft_out_i16.0, &mut transformed.0, self.qa);

        if debug {
            let t_min = transformed.0.iter().min().copied().unwrap_or(0);
            let t_max = transformed.0.iter().max().copied().unwrap_or(0);
            let t_sum: u64 = transformed.0.iter().map(|&x| x as u64).sum();
            eprintln!("[DEBUG] After activation ({} i16→u8): min={t_min}, max={t_max}, sum={t_sum}, len={}", A::name(), transformed.0.len());
            eprintln!("[DEBUG] transformed[0..16]: {:?}", &transformed.0[0..16]);
        }

        // l1 層 - 64バイトアライン
        let mut l1_out = Aligned([0i32; L2]);
        self.l1.propagate(&transformed.0, &mut l1_out.0);

        if debug {
            eprintln!("[DEBUG] L1 output: {:?}", &l1_out.0);
            eprintln!(
                "[DEBUG] L1 biases[0..8]: {:?}",
                &self.l1.biases[0..8.min(self.l1.biases.len())]
            );
        }

        // デバッグ: L1出力の範囲チェック
        #[cfg(debug_assertions)]
        for (i, &v) in l1_out.0.iter().enumerate() {
            debug_assert!(
                v.abs() < 1_000_000,
                "L1 output[{i}] = {v} is out of expected range (NetworkHalfKA<{}, {}, {}, {}>)",
                L1,
                L2,
                L3,
                A::name()
            );
        }

        // 活性化関数適用 (i32 → u8) - 64バイトアライン
        let mut l1_relu = Aligned([0u8; L2]);
        A::activate_i32_to_u8(&l1_out.0, &mut l1_relu.0);

        // l2 層 - 64バイトアライン
        let mut l2_out = Aligned([0i32; L3]);
        self.l2.propagate(&l1_relu.0, &mut l2_out.0);

        // デバッグ: L2出力の範囲チェック
        #[cfg(debug_assertions)]
        for (i, &v) in l2_out.0.iter().enumerate() {
            debug_assert!(
                v.abs() < 1_000_000,
                "L2 output[{i}] = {v} is out of expected range (NetworkHalfKA<{}, {}, {}, {}>)",
                L1,
                L2,
                L3,
                A::name()
            );
        }

        // 活性化関数適用 (i32 → u8) - 64バイトアライン
        let mut l2_relu = Aligned([0u8; L3]);
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
            "Final evaluation {eval} is out of expected range (NetworkHalfKA<{}, {}, {}, {}>). Raw output: {}",
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
    pub fn new_accumulator(&self) -> AccumulatorHalfKA<L1> {
        AccumulatorHalfKA::new()
    }

    /// 新しい AccumulatorStack を作成
    pub fn new_accumulator_stack(&self) -> AccumulatorStackHalfKA<L1> {
        AccumulatorStackHalfKA::new()
    }

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> String {
        format!("HalfKA^{}x2-{}-{}-{}", L1, L2, L3, A::name())
    }
}

// =============================================================================
// ヘルパー関数
// =============================================================================

/// アーキテクチャ文字列から QA 値をパース
fn parse_qa_from_arch(arch_str: &str) -> Option<i16> {
    // "qa=N" パターンを探す
    if let Some(start) = arch_str.find("qa=") {
        let rest = &arch_str[start + 3..];
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

// L1=256, FT_OUT=512
// CReLU/SCReLU: L1_INPUT=512, Pairwise: L1_INPUT=256
/// HalfKA 256x2-32-32 CReLU
pub type HalfKA256CReLU = NetworkHalfKA<256, 512, 512, 32, 32, CReLU>;
/// HalfKA 256x2-32-32 SCReLU
pub type HalfKA256SCReLU = NetworkHalfKA<256, 512, 512, 32, 32, SCReLU>;
/// HalfKA 256/2x2-32-32 PairwiseCReLU (L1入力=256, Pairwise乗算で次元半減)
pub type HalfKA256Pairwise = NetworkHalfKA<256, 512, 256, 32, 32, PairwiseCReLU>;

// L1=512, FT_OUT=1024, L2=8, L3=96
// CReLU/SCReLU: L1_INPUT=1024, Pairwise: L1_INPUT=512
/// HalfKA 512x2-8-96 CReLU
pub type HalfKA512CReLU = NetworkHalfKA<512, 1024, 1024, 8, 96, CReLU>;
/// HalfKA 512x2-8-96 SCReLU
pub type HalfKA512SCReLU = NetworkHalfKA<512, 1024, 1024, 8, 96, SCReLU>;
/// HalfKA 512/2x2-8-96 PairwiseCReLU (L1入力=512, Pairwise乗算で次元半減)
pub type HalfKA512Pairwise = NetworkHalfKA<512, 1024, 512, 8, 96, PairwiseCReLU>;

// L1=512, FT_OUT=1024, L2=32, L3=32
/// HalfKA 512x2-32-32 CReLU
pub type HalfKA512_32_32CReLU = NetworkHalfKA<512, 1024, 1024, 32, 32, CReLU>;
/// HalfKA 512x2-32-32 SCReLU
pub type HalfKA512_32_32SCReLU = NetworkHalfKA<512, 1024, 1024, 32, 32, SCReLU>;
/// HalfKA 512/2x2-32-32 PairwiseCReLU (L1入力=512, Pairwise乗算で次元半減)
pub type HalfKA512_32_32Pairwise = NetworkHalfKA<512, 1024, 512, 32, 32, PairwiseCReLU>;

// L1=1024, FT_OUT=2048, L2=8, L3=96
// CReLU/SCReLU: L1_INPUT=2048, Pairwise: L1_INPUT=1024
/// HalfKA 1024x2-8-96 CReLU
pub type HalfKA1024CReLU = NetworkHalfKA<1024, 2048, 2048, 8, 96, CReLU>;
/// HalfKA 1024x2-8-96 SCReLU
pub type HalfKA1024SCReLU = NetworkHalfKA<1024, 2048, 2048, 8, 96, SCReLU>;
/// HalfKA 1024/2x2-8-96 PairwiseCReLU (L1入力=1024, Pairwise乗算で次元半減)
pub type HalfKA1024Pairwise = NetworkHalfKA<1024, 2048, 1024, 8, 96, PairwiseCReLU>;

// L1=1024, FT_OUT=2048, L2=8, L3=32
/// HalfKA 1024x2-8-32 CReLU
pub type HalfKA1024_8_32CReLU = NetworkHalfKA<1024, 2048, 2048, 8, 32, CReLU>;
/// HalfKA 1024x2-8-32 SCReLU
pub type HalfKA1024_8_32SCReLU = NetworkHalfKA<1024, 2048, 2048, 8, 32, SCReLU>;
/// HalfKA 1024/2x2-8-32 PairwiseCReLU (L1入力=1024, Pairwise乗算で次元半減)
pub type HalfKA1024_8_32Pairwise = NetworkHalfKA<1024, 2048, 1024, 8, 32, PairwiseCReLU>;

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_halfka_256() {
        let mut acc = AccumulatorHalfKA::<256>::new();
        assert_eq!(acc.accumulation[0].len(), 256);
        assert!(!acc.computed_accumulation);

        acc.accumulation[0][0] = 100;
        acc.computed_accumulation = true;

        let cloned = acc.clone();
        assert_eq!(cloned.accumulation[0][0], 100);
        assert!(cloned.computed_accumulation);
    }

    #[test]
    fn test_accumulator_halfka_512() {
        let acc = AccumulatorHalfKA::<512>::new();
        assert_eq!(acc.accumulation[0].len(), 512);
    }

    #[test]
    fn test_accumulator_halfka_1024() {
        let acc = AccumulatorHalfKA::<1024>::new();
        assert_eq!(acc.accumulation[0].len(), 1024);
    }

    #[test]
    fn test_padded_input() {
        assert_eq!(AffineTransformHalfKA::<8, 96>::PADDED_INPUT, 32);
        assert_eq!(AffineTransformHalfKA::<32, 96>::PADDED_INPUT, 32);
        assert_eq!(AffineTransformHalfKA::<33, 96>::PADDED_INPUT, 64);
        assert_eq!(AffineTransformHalfKA::<96, 1>::PADDED_INPUT, 96);
        assert_eq!(AffineTransformHalfKA::<1024, 8>::PADDED_INPUT, 1024);
        assert_eq!(AffineTransformHalfKA::<2048, 8>::PADDED_INPUT, 2048);
    }

    #[test]
    fn test_parse_qa_from_arch() {
        assert_eq!(parse_qa_from_arch("HalfKA^512x2-8-96-qa=255"), Some(255));
        assert_eq!(parse_qa_from_arch("HalfKA^512x2-8-96-qa=127"), Some(127));
        assert_eq!(parse_qa_from_arch("HalfKA^512x2-8-96"), None);
    }

    #[test]
    fn test_type_aliases() {
        // 型エイリアスがコンパイルできることを確認
        fn _check_halfka_256_crelu(_: HalfKA256CReLU) {}
        fn _check_halfka_512_screlu(_: HalfKA512SCReLU) {}
        fn _check_halfka_1024_pairwise(_: HalfKA1024Pairwise) {}
    }
}
