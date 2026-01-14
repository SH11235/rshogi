//! NetworkHalfKADynamic - 動的サイズ対応のNNUEネットワーク
//!
//! HalfKA_hm^ 特徴量を使用し、L1/L2/L3 のサイズをファイルから動的に読み取る。
//! nnue-pytorch で学習したモデルに対応。
//!
//! # アーキテクチャ
//!
//! - Feature Transformer: 73,305 → L1 (例: 1024)
//! - l1: L1 * 2 → L2 (例: 2048 → 8)
//! - l2: L2 → L3 (例: 8 → 96)
//! - output: L3 → 1 (例: 96 → 1)
//! - 活性化: ClippedReLU のみ（SqrClippedReLU なし）
//!
//! # SIMD最適化
//!
//! AVX2/SSE2/WASM SIMD128 による最適化を実装:
//! - AffineTransformDynamic: DPBUSD emulation による行列積
//! - FeatureTransformerHalfKADynamic: i16 加減算のベクトル化

use super::accumulator::{AlignedBox, DirtyPiece, IndexList, MAX_PATH_LENGTH};
use super::constants::{
    HALFKA_HM_DIMENSIONS, MAX_ARCH_LEN, NNUE_VERSION_HALFKA, WEIGHT_SCALE_BITS,
};
use super::features::{FeatureSet, HalfKA_hmFeatureSet};
use crate::position::Position;
use crate::types::{Color, Value};
use std::io::{self, Read, Seek, SeekFrom};

// =============================================================================
// SIMD ヘルパー関数
// =============================================================================

/// AVX2用 DPBUSD エミュレーション（u8×i8→i32積和演算）
///
/// # Safety
///
/// - 呼び出し元のCPUがAVX2命令をサポートしていること
///   （`target_feature = "avx2"` で保証）
/// - `acc`, `a`, `b` は有効な `__m256i` 値であること
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
///
/// # Safety
///
/// - 呼び出し元のCPUがAVX2命令をサポートしていること
///   （`target_feature = "avx2"` で保証）
/// - `v` は有効な `__m256i` 値であること
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
///
/// # Safety
///
/// - 呼び出し元のCPUがSSSE3命令をサポートしていること
///   （`target_feature = "ssse3"` で保証）
/// - `v` は有効な `__m128i` 値であること
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

/// SSSE3用 DPBUSD エミュレーション（u8×i8→i32積和演算）
///
/// # Safety
///
/// - 呼び出し元のCPUがSSSE3命令をサポートしていること
///   （`target_feature = "ssse3"` で保証）
/// - `acc`, `a`, `b` は有効な `__m128i` 値であること
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
// 定数
// =============================================================================

/// 評価値のスケーリング（kPonanzaConstant = 600, FV_SCALE = 16）
const FV_SCALE_HALFKA_DYNAMIC: i32 = 16;

// =============================================================================
// AccumulatorHalfKADynamic - 動的サイズのアキュムレータ
// =============================================================================

/// 動的サイズのアキュムレータ（64バイトアライン済み）
pub struct AccumulatorHalfKADynamic {
    /// アキュムレータバッファ [perspective][L1]（SIMD最適化のため64バイトアライン）
    pub accumulation: [AlignedBox<i16>; 2],
    /// 計算済みフラグ
    pub computed_accumulation: bool,
    /// L1 サイズ
    pub l1: usize,
}

impl AccumulatorHalfKADynamic {
    /// 新規作成
    pub fn new(l1: usize) -> Self {
        Self {
            accumulation: [AlignedBox::new_zeroed(l1), AlignedBox::new_zeroed(l1)],
            computed_accumulation: false,
            l1,
        }
    }

    /// クリア
    pub fn clear(&mut self) {
        self.accumulation[0].fill(0);
        self.accumulation[1].fill(0);
        self.computed_accumulation = false;
    }
}

impl Clone for AccumulatorHalfKADynamic {
    fn clone(&self) -> Self {
        Self {
            accumulation: [self.accumulation[0].clone(), self.accumulation[1].clone()],
            computed_accumulation: self.computed_accumulation,
            l1: self.l1,
        }
    }
}

// =============================================================================
// AccumulatorStackHalfKADynamic - アキュムレータスタック
// =============================================================================

/// スタックエントリ
pub struct AccumulatorEntryHalfKADynamic {
    pub accumulator: AccumulatorHalfKADynamic,
    pub dirty_piece: DirtyPiece,
    pub previous: Option<usize>,
}

/// アキュムレータスタック
pub struct AccumulatorStackHalfKADynamic {
    entries: Vec<AccumulatorEntryHalfKADynamic>,
    current_idx: usize,
    l1: usize,
}

impl AccumulatorStackHalfKADynamic {
    /// 新規作成
    pub fn new(l1: usize) -> Self {
        let mut entries = Vec::with_capacity(128);
        entries.push(AccumulatorEntryHalfKADynamic {
            accumulator: AccumulatorHalfKADynamic::new(l1),
            dirty_piece: DirtyPiece::default(),
            previous: None,
        });
        Self {
            entries,
            current_idx: 0,
            l1,
        }
    }

    /// L1サイズを取得
    pub fn l1(&self) -> usize {
        self.l1
    }

    /// 現在のエントリを取得
    pub fn current(&self) -> &AccumulatorEntryHalfKADynamic {
        &self.entries[self.current_idx]
    }

    /// 現在のエントリを取得（可変）
    pub fn current_mut(&mut self) -> &mut AccumulatorEntryHalfKADynamic {
        &mut self.entries[self.current_idx]
    }

    /// プッシュ
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        let prev_idx = self.current_idx;
        self.current_idx = self.entries.len();
        self.entries.push(AccumulatorEntryHalfKADynamic {
            accumulator: AccumulatorHalfKADynamic::new(self.l1),
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
    ///
    /// スタックを初期状態に戻し、computed_accumulation フラグをクリアする。
    /// これにより、前回の探索で計算済みになったアキュムレータが
    /// 新しい探索で誤用されることを防ぐ。
    pub fn reset(&mut self) {
        self.current_idx = 0;
        self.entries.truncate(1);
        self.entries[0].accumulator.computed_accumulation = false;
        self.entries[0].dirty_piece.clear();
        self.entries[0].previous = None;
    }

    /// 祖先を辿って使用可能なアキュムレータを探す
    ///
    /// ## 実装方針
    ///
    /// アキュムレータの差分更新における祖先探索には複数のアプローチがある:
    ///
    /// - **YaneuraOu方式**: 1手前のみをチェック（シンプルだが差分更新の機会を逃す）
    /// - **Stockfish方式**: スタック全体を探索し、各ステップで玉移動をチェック
    ///
    /// このプロジェクトでは、HalfKP側（accumulator.rs）と同じロジックを採用している。
    /// 最大8手前まで探索し、各ステップで玉移動があれば即座に打ち切る方式である。
    /// この方式により、1手前限定より多くの差分更新機会を得つつ、玉移動時の
    /// 無駄な探索を早期に打ち切ることでNPS向上が観測されている。
    ///
    /// ## 戻り値
    ///
    /// `Some((計算済みエントリのインデックス, 経由する局面数))` - 玉移動がない範囲で
    /// 計算済み祖先が見つかった場合。`None` - 使用可能な祖先が見つからない場合。
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        const MAX_DEPTH: usize = 8;

        let current = &self.entries[self.current_idx];

        // 現局面で玉が動いていたら差分更新不可
        if current.dirty_piece.king_moved[0] || current.dirty_piece.king_moved[1] {
            return None;
        }

        // 直前局面をチェック（depth=1から開始）
        let mut prev_idx = current.previous?;
        let mut depth = 1;

        loop {
            let prev = &self.entries[prev_idx];

            // 計算済みなら成功
            if prev.accumulator.computed_accumulation {
                return Some((prev_idx, depth));
            }

            // 探索上限に達した
            if depth >= MAX_DEPTH {
                return None;
            }

            // さらに前の局面へ（ルートに達したらNone）
            let next_prev_idx = prev.previous?;

            // 玉が動いていたら打ち切り（早期終了による最適化）
            if prev.dirty_piece.king_moved[0] || prev.dirty_piece.king_moved[1] {
                return None;
            }

            prev_idx = next_prev_idx;
            depth += 1;
        }
    }

    /// 指定インデックスのエントリを取得
    pub fn entry_at(&self, idx: usize) -> &AccumulatorEntryHalfKADynamic {
        &self.entries[idx]
    }

    /// 指定インデックスのエントリを取得（可変）
    pub fn entry_at_mut(&mut self, idx: usize) -> &mut AccumulatorEntryHalfKADynamic {
        &mut self.entries[idx]
    }

    /// 前回と現在のアキュムレータを取得（可変）
    ///
    /// split_at_mut を使用して clone を回避
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&AccumulatorHalfKADynamic, &mut AccumulatorHalfKADynamic) {
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
    ///
    /// 戻り値:
    /// - Some(path): source_idx に到達できた場合、source側から適用する順のインデックス列
    /// - None: パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
    pub fn collect_path(&self, source_idx: usize) -> Option<IndexList<MAX_PATH_LENGTH>> {
        let mut path = IndexList::new();
        let mut idx = self.current_idx;

        while idx != source_idx {
            // パス長が上限を超えたら失敗
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

// =============================================================================
// FeatureTransformerHalfKADynamic - 動的サイズのFeature Transformer
// =============================================================================

/// 動的サイズのFeature Transformer
pub struct FeatureTransformerHalfKADynamic {
    /// バイアス [L1]
    pub biases: Vec<i16>,
    /// 重み [input_dimensions][L1]
    pub weights: AlignedBox<i16>,
    /// 出力次元数 (L1)
    pub l1: usize,
    /// 入力次元数
    pub input_dim: usize,
}

impl FeatureTransformerHalfKADynamic {
    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R, l1: usize) -> io::Result<Self> {
        let input_dim = HALFKA_HM_DIMENSIONS;

        // バイアスを読み込み
        let mut biases = vec![0i16; l1];
        let mut buf = [0u8; 2];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf)?;
            *bias = i16::from_le_bytes(buf);
        }

        // 重みを読み込み
        let weight_size = input_dim * l1;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf)?;
            *weight = i16::from_le_bytes(buf);
        }

        Ok(Self {
            biases,
            weights,
            l1,
            input_dim,
        })
    }

    /// Accumulatorをリフレッシュ
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKADynamic) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = &mut acc.accumulation[p];

            // バイアスで初期化
            accumulation.copy_from_slice(&self.biases);

            // アクティブ特徴量を加算
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
        acc: &mut AccumulatorHalfKADynamic,
        prev_acc: &AccumulatorHalfKADynamic,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = HalfKA_hmFeatureSet::needs_refresh(dirty_piece, perspective);

            if reset {
                // リフレッシュ
                acc.accumulation[p].copy_from_slice(&self.biases);
                let active_indices = HalfKA_hmFeatureSet::collect_active_indices(pos, perspective);
                for &index in active_indices.iter() {
                    self.add_weights(&mut acc.accumulation[p], index);
                }
            } else {
                // 差分更新
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
        stack: &mut AccumulatorStackHalfKADynamic,
        source_idx: usize,
    ) -> bool {
        let Some(path) = stack.collect_path(source_idx) else {
            // パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
            return false;
        };

        // ソースからコピー（借用の競合を避けるため、一時バッファにコピー）
        let current_idx = stack.current_index();
        for p in 0..2 {
            // 一時バッファを経由してコピー
            let source_data: Vec<i16> =
                stack.entry_at(source_idx).accumulator.accumulation[p].to_vec();
            stack.entry_at_mut(current_idx).accumulator.accumulation[p]
                .copy_from_slice(&source_data);
        }

        // パス上の各エントリの差分を適用
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
        let offset = index * self.l1;
        let weights = &self.weights[offset..offset + self.l1];

        // AVX2: 256bit = 16 x i16（アライン済みロード/ストア）
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();
                let num_chunks = self.l1 / 16;

                for i in 0..num_chunks {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        // SSE2: 128bit = 8 x i16（アライン済みロード/ストア）
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
                let num_chunks = self.l1 / 8;

                for i in 0..num_chunks {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // スカラーフォールバック
        #[allow(unreachable_code)]
        for (acc, &w) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_add(w);
        }
    }

    /// 重みを減算（SIMD最適化版）
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16], index: usize) {
        let offset = index * self.l1;
        let weights = &self.weights[offset..offset + self.l1];

        // AVX2: 256bit = 16 x i16（アライン済みロード/ストア）
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();
                let num_chunks = self.l1 / 16;

                for i in 0..num_chunks {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_sub_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        // SSE2: 128bit = 8 x i16（アライン済みロード/ストア）
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
                let num_chunks = self.l1 / 8;

                for i in 0..num_chunks {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_sub_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // スカラーフォールバック
        #[allow(unreachable_code)]
        for (acc, &w) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_sub(w);
        }
    }

    /// 変換（ClippedReLU適用、SIMD最適化版）
    pub fn transform(
        &self,
        acc: &AccumulatorHalfKADynamic,
        side_to_move: Color,
        output: &mut [u8],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        // AVX2: i16→u8パック + クリップ [0, 127]（accumulation はアライン済み）
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let zero = _mm256_setzero_si256();
                let max_val = _mm256_set1_epi16(127);

                for (p, &perspective) in perspectives.iter().enumerate() {
                    let out_offset = self.l1 * p;
                    let accumulation = &acc.accumulation[perspective as usize];
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output.as_mut_ptr().add(out_offset);
                    let num_chunks = self.l1 / 16;

                    for i in 0..num_chunks {
                        // accumulation は AlignedBox なので aligned load
                        let v = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                        // クリップ: max(0, min(127, v))
                        let clamped = _mm256_min_epi16(_mm256_max_epi16(v, zero), max_val);
                        // i16→u8 パック（飽和）
                        let packed = _mm256_packus_epi16(clamped, clamped);
                        // レーン順序を修正
                        let result = _mm256_permute4x64_epi64(packed, 0b11011000);
                        // 下位128bitのみ保存（output はアライメント保証なしなので unaligned）
                        _mm_storeu_si128(
                            out_ptr.add(i * 16) as *mut __m128i,
                            _mm256_castsi256_si128(result),
                        );
                    }
                }
            }
            return;
        }

        // SSE2: i16→u8パック + クリップ（accumulation はアライン済み）
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
                    let out_offset = self.l1 * p;
                    let accumulation = &acc.accumulation[perspective as usize];
                    let acc_ptr = accumulation.as_ptr();
                    let out_ptr = output.as_mut_ptr().add(out_offset);
                    let num_chunks = self.l1 / 16;

                    for i in 0..num_chunks {
                        // 16要素を2つのSSEレジスタで処理（aligned load）
                        let v0 = _mm_load_si128(acc_ptr.add(i * 16) as *const __m128i);
                        let v1 = _mm_load_si128(acc_ptr.add(i * 16 + 8) as *const __m128i);

                        // クリップ
                        let clamped0 = _mm_min_epi16(_mm_max_epi16(v0, zero), max_val);
                        let clamped1 = _mm_min_epi16(_mm_max_epi16(v1, zero), max_val);

                        // i16→u8 パック（output は unaligned store）
                        let packed = _mm_packus_epi16(clamped0, clamped1);
                        _mm_storeu_si128(out_ptr.add(i * 16) as *mut __m128i, packed);
                    }
                }
            }
            return;
        }

        // スカラーフォールバック
        #[allow(unreachable_code)]
        for (p, &perspective) in perspectives.iter().enumerate() {
            let out_offset = self.l1 * p;
            let accumulation = &acc.accumulation[perspective as usize];

            for i in 0..self.l1 {
                output[out_offset + i] = accumulation[i].clamp(0, 127) as u8;
            }
        }
    }
}

// =============================================================================
// AffineTransformDynamic - 動的サイズのアフィン変換
// =============================================================================

/// 動的サイズのアフィン変換層
pub struct AffineTransformDynamic {
    /// バイアス [output_dim]
    pub biases: Vec<i32>,
    /// 重み [output_dim][padded_input_dim]
    pub weights: AlignedBox<i8>,
    /// 入力次元
    pub input_dim: usize,
    /// パディング済み入力次元
    pub padded_input_dim: usize,
    /// 出力次元
    pub output_dim: usize,
}

impl AffineTransformDynamic {
    /// パディング済み入力次元を計算
    fn padded_input(input_dim: usize) -> usize {
        input_dim.div_ceil(32) * 32
    }

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R, input_dim: usize, output_dim: usize) -> io::Result<Self> {
        let padded_input_dim = Self::padded_input(input_dim);

        // バイアスを読み込み
        let mut biases = vec![0i32; output_dim];
        let mut buf4 = [0u8; 4];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // 重みを読み込み
        let weight_size = output_dim * padded_input_dim;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        let mut buf1 = [0u8; 1];
        for i in 0..weight_size {
            reader.read_exact(&mut buf1)?;
            weights[i] = buf1[0] as i8;
        }

        Ok(Self {
            biases,
            weights,
            input_dim,
            padded_input_dim,
            output_dim,
        })
    }

    /// 順伝播（SIMD最適化版）
    pub fn propagate(&self, input: &[u8], output: &mut [i32]) {
        // バイアスで初期化
        output.copy_from_slice(&self.biases);

        // AVX2: 32バイトずつ処理
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                self.propagate_avx2(input, output);
            }
            return;
        }

        // SSSE3: 16バイトずつ処理
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

        // スカラーフォールバック
        #[allow(unreachable_code)]
        for (j, out) in output.iter_mut().enumerate() {
            let weight_offset = j * self.padded_input_dim;
            for (i, &in_val) in input.iter().enumerate().take(self.input_dim) {
                *out += self.weights[weight_offset + i] as i32 * in_val as i32;
            }
        }
    }

    /// AVX2 による順伝播
    ///
    /// # Safety
    ///
    /// - 呼び出し元のCPUがAVX2命令をサポートしていること
    ///   （`target_feature = "avx2"` で保証）
    /// - `input` のサイズは `self.padded_input_dim` 以上であること
    /// - `self.weights` のアライメントが32バイト境界であること
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    #[inline]
    unsafe fn propagate_avx2(&self, input: &[u8], output: &mut [i32]) {
        use std::arch::x86_64::*;

        let num_chunks = self.padded_input_dim / 32;
        let input_ptr = input.as_ptr();
        let weight_ptr = self.weights.as_ptr();

        for (j, out) in output.iter_mut().enumerate() {
            let mut acc = _mm256_setzero_si256();
            let row_offset = j * self.padded_input_dim;

            for chunk in 0..num_chunks {
                let in_vec = _mm256_loadu_si256(input_ptr.add(chunk * 32) as *const __m256i);
                let w_vec =
                    _mm256_load_si256(weight_ptr.add(row_offset + chunk * 32) as *const __m256i);
                m256_add_dpbusd_epi32(&mut acc, in_vec, w_vec);
            }

            *out += hsum_i32_avx2(acc);
        }
    }

    /// SSSE3 による順伝播
    ///
    /// # Safety
    ///
    /// - 呼び出し元のCPUがSSSE3命令をサポートしていること
    ///   （`target_feature = "ssse3"` で保証）
    /// - `input` のサイズは `self.padded_input_dim` 以上であること
    /// - `self.weights` のアライメントが16バイト境界であること
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "ssse3",
        not(target_feature = "avx2")
    ))]
    #[inline]
    unsafe fn propagate_ssse3(&self, input: &[u8], output: &mut [i32]) {
        use std::arch::x86_64::*;

        let num_chunks = self.padded_input_dim / 16;
        let input_ptr = input.as_ptr();
        let weight_ptr = self.weights.as_ptr();

        for (j, out) in output.iter_mut().enumerate() {
            let mut acc = _mm_setzero_si128();
            let row_offset = j * self.padded_input_dim;

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
// NetworkHalfKADynamic - 動的サイズのネットワーク
// =============================================================================

/// HalfKA_hm^ 特徴量 + 動的サイズ FC 層のネットワーク
pub struct NetworkHalfKADynamic {
    /// Feature Transformer
    pub feature_transformer: FeatureTransformerHalfKADynamic,
    /// L1 層: L1*2 → L2
    pub l1: AffineTransformDynamic,
    /// L2 層: L2 → L3
    pub l2: AffineTransformDynamic,
    /// 出力層: L3 → 1
    pub output: AffineTransformDynamic,
    /// アーキテクチャ: Feature Transformer 出力次元
    pub arch_l1: usize,
    /// アーキテクチャ: L1 出力次元
    pub arch_l2: usize,
    /// アーキテクチャ: L2 出力次元
    pub arch_l3: usize,
}

impl NetworkHalfKADynamic {
    /// ファイルから読み込み（L1, L2, L3 を指定）
    pub fn read_with_arch<R: Read + Seek>(
        reader: &mut R,
        l1: usize,
        l2: usize,
        l3: usize,
    ) -> io::Result<Self> {
        // ヘッダを読み込み
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        // HalfKP または HalfKA バージョンを許容
        if version != 0x7AF32F16 && version != NNUE_VERSION_HALFKA {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unknown NNUE version: {version:#x}"),
            ));
        }

        // 構造ハッシュ
        reader.read_exact(&mut buf4)?;
        let _hash = u32::from_le_bytes(buf4);

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

        // Feature Transformer ハッシュ
        reader.read_exact(&mut buf4)?;
        let _ft_hash = u32::from_le_bytes(buf4);

        // Feature Transformer
        let feature_transformer = FeatureTransformerHalfKADynamic::read(reader, l1)?;

        // FC layers ハッシュ
        reader.read_exact(&mut buf4)?;
        let _fc_hash = u32::from_le_bytes(buf4);

        // l1: L1*2 → L2
        let l1_layer = AffineTransformDynamic::read(reader, l1 * 2, l2)?;

        // l2: L2 → L3
        let l2_layer = AffineTransformDynamic::read(reader, l2, l3)?;

        // output: L3 → 1
        let output_layer = AffineTransformDynamic::read(reader, l3, 1)?;

        Ok(Self {
            feature_transformer,
            l1: l1_layer,
            l2: l2_layer,
            output: output_layer,
            arch_l1: l1,
            arch_l2: l2,
            arch_l3: l3,
        })
    }

    /// アーキテクチャ文字列から L1 を推定して読み込み
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        // まずヘッダを読んで L1 を推定
        let start_pos = reader.stream_position()?;

        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        let _version = u32::from_le_bytes(buf4);

        reader.read_exact(&mut buf4)?; // hash
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

        // アーキテクチャ文字列から L1 を推定
        // 例: "[1024x2]" や "[256x2]" を探す
        let l1 = Self::parse_l1_from_arch(&arch_str).unwrap_or(1024);

        // 将棋用デフォルトは L1=1024, L2=8, L3=96
        let (l2, l3) = match l1 {
            1024 => (8, 96),
            512 => (8, 96),
            256 => (32, 32), // classic
            _ => (8, 96),
        };

        // 位置を戻して読み直し
        reader.seek(SeekFrom::Start(start_pos))?;
        Self::read_with_arch(reader, l1, l2, l3)
    }

    /// アーキテクチャ文字列から L1 を推定
    fn parse_l1_from_arch(arch: &str) -> Option<usize> {
        // "[NNNx2]" パターンを探す
        if let Some(idx) = arch.find("x2]") {
            let before = &arch[..idx];
            if let Some(start) = before.rfind(|c: char| !c.is_ascii_digit()) {
                let num_str = &before[start + 1..];
                return num_str.parse().ok();
            }
        }
        // "->NNN" パターンを探す
        if let Some(idx) = arch.find("->") {
            let after = &arch[idx + 2..];
            let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
            let num_str = &after[..end];
            return num_str.parse().ok();
        }
        None
    }

    /// Accumulator をリフレッシュ
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKADynamic) {
        self.feature_transformer.refresh_accumulator(pos, acc);
    }

    /// Accumulator を差分更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorHalfKADynamic,
        prev_acc: &AccumulatorHalfKADynamic,
    ) {
        self.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc);
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackHalfKADynamic,
        source_idx: usize,
    ) -> bool {
        self.feature_transformer.forward_update_incremental(pos, stack, source_idx)
    }

    /// 評価値を計算
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorHalfKADynamic) -> Value {
        let l1 = self.arch_l1;

        // Feature Transformer 出力
        let mut transformed = vec![0u8; l1 * 2];
        self.feature_transformer.transform(acc, pos.side_to_move(), &mut transformed);

        // l1 層
        let mut l1_out = vec![0i32; self.arch_l2];
        self.l1.propagate(&transformed, &mut l1_out);

        // ClippedReLU
        let mut l1_relu = vec![0u8; self.arch_l2];
        for (i, &v) in l1_out.iter().enumerate() {
            let shifted = v >> WEIGHT_SCALE_BITS;
            l1_relu[i] = shifted.clamp(0, 127) as u8;
        }

        // l2 層
        let mut l2_out = vec![0i32; self.arch_l3];
        self.l2.propagate(&l1_relu, &mut l2_out);

        // ClippedReLU
        let mut l2_relu = vec![0u8; self.arch_l3];
        for (i, &v) in l2_out.iter().enumerate() {
            let shifted = v >> WEIGHT_SCALE_BITS;
            l2_relu[i] = shifted.clamp(0, 127) as u8;
        }

        // output 層
        let mut output = vec![0i32; 1];
        self.output.propagate(&l2_relu, &mut output);

        // スケーリング
        Value::new(output[0] / FV_SCALE_HALFKA_DYNAMIC)
    }

    /// 新しい Accumulator を作成
    pub fn new_accumulator(&self) -> AccumulatorHalfKADynamic {
        AccumulatorHalfKADynamic::new(self.arch_l1)
    }

    /// 新しい AccumulatorStack を作成
    pub fn new_accumulator_stack(&self) -> AccumulatorStackHalfKADynamic {
        AccumulatorStackHalfKADynamic::new(self.arch_l1)
    }

    /// アーキテクチャ名を取得
    pub fn architecture_name(&self) -> String {
        format!("HalfKADynamic {}x2-{}-{}", self.arch_l1, self.arch_l2, self.arch_l3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_l1_from_arch() {
        assert_eq!(NetworkHalfKADynamic::parse_l1_from_arch("[1024x2]"), Some(1024));
        assert_eq!(NetworkHalfKADynamic::parse_l1_from_arch("[256x2]"), Some(256));
        assert_eq!(
            NetworkHalfKADynamic::parse_l1_from_arch("Features=HalfKP[73305->1024x2]"),
            Some(1024)
        );
        assert_eq!(NetworkHalfKADynamic::parse_l1_from_arch("->512x2"), Some(512));
    }

    #[test]
    fn test_accumulator_halfka_dynamic() {
        let mut acc = AccumulatorHalfKADynamic::new(1024);
        assert_eq!(acc.l1, 1024);
        assert_eq!(acc.accumulation[0].len(), 1024);
        assert!(!acc.computed_accumulation);

        acc.accumulation[0][0] = 100;
        acc.computed_accumulation = true;

        let cloned = acc.clone();
        assert_eq!(cloned.accumulation[0][0], 100);
        assert!(cloned.computed_accumulation);
    }

    #[test]
    fn test_padded_input() {
        assert_eq!(AffineTransformDynamic::padded_input(8), 32);
        assert_eq!(AffineTransformDynamic::padded_input(32), 32);
        assert_eq!(AffineTransformDynamic::padded_input(33), 64);
        assert_eq!(AffineTransformDynamic::padded_input(96), 96);
    }

    #[test]
    fn test_load_epoch20_v2_nnue() {
        use std::fs::File;
        use std::io::BufReader;
        use std::path::Path;

        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("epoch20_v2.nnue");

        if !path.exists() {
            eprintln!("Skipping test: NNUE file not found at {path:?}");
            return;
        }

        let file = File::open(&path).expect("Failed to open NNUE file");
        let mut reader = BufReader::new(file);

        let network = NetworkHalfKADynamic::read(&mut reader).expect("Failed to read NNUE file");

        // アーキテクチャの検証
        assert_eq!(network.arch_l1, 1024, "L1 should be 1024");
        assert_eq!(network.arch_l2, 8, "L2 should be 8");
        assert_eq!(network.arch_l3, 96, "L3 should be 96");

        // Feature Transformer の検証
        assert_eq!(network.feature_transformer.l1, 1024, "FT output dim should be 1024");
        assert_eq!(
            network.feature_transformer.input_dim, HALFKA_HM_DIMENSIONS,
            "FT input dim should be HalfKA_hm dimensions"
        );
        assert_eq!(
            network.feature_transformer.biases.len(),
            1024,
            "FT biases should have 1024 elements"
        );
        assert_eq!(
            network.feature_transformer.weights.len(),
            HALFKA_HM_DIMENSIONS * 1024,
            "FT weights should have input_dim * L1 elements"
        );

        // FC層の検証
        // l1: 2048 -> 8
        assert_eq!(network.l1.input_dim, 2048, "l1 input_dim should be 2048");
        assert_eq!(network.l1.output_dim, 8, "l1 output_dim should be 8");

        // l2: 8 -> 96
        assert_eq!(network.l2.input_dim, 8, "l2 input_dim should be 8");
        assert_eq!(network.l2.output_dim, 96, "l2 output_dim should be 96");

        // output: 96 -> 1
        assert_eq!(network.output.input_dim, 96, "output input_dim should be 96");
        assert_eq!(network.output.output_dim, 1, "output output_dim should be 1");

        println!("Successfully loaded: {}", network.architecture_name());
    }
}
