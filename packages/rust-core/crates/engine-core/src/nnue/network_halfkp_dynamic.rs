//! HalfKP 動的サイズネットワーク
//!
//! HalfKP 特徴量を使用し、L1/L2/L3 のサイズをファイルから動的に読み取る。
//! 256x2-32-32 以外のアーキテクチャ（512x2-8-96, 512x2-32-32, 1024x2-8-32 など）に対応。
//!
//! ## ネットワーク構造
//!
//! - Feature Transformer: 125,388 → L1 (例: 512)
//! - l1: L1 * 2 → L2 (例: 1024 → 8)
//! - l2: L2 → L3 (例: 8 → 96)
//! - output: L3 → 1 (例: 96 → 1)
//!
//! ## 実装方針
//!
//! `NetworkHalfKADynamic` と同様のアプローチ:
//! - Feature Transformer, Accumulator, Network すべて動的サイズ
//! - SIMD 最適化（AVX2/SSE2）
//! - 既存の `AffineTransformDynamic` を再利用

use super::accumulator::{AlignedBox, DirtyPiece, IndexList, MAX_PATH_LENGTH};
use super::constants::{
    FV_SCALE, FV_SCALE_HALFKA, HALFKP_DIMENSIONS, MAX_ARCH_LEN, NNUE_VERSION, SCRELU_DEFAULT_QA,
    WEIGHT_SCALE_BITS,
};
use super::features::{FeatureSet, HalfKPFeatureSet};
use super::network::{get_fv_scale_override, parse_fv_scale_from_arch, parse_qa_from_arch};
use super::network_halfka_dynamic::AffineTransformDynamic;
use crate::position::Position;
use crate::types::{Color, Value};
use std::io::{self, Read, Seek, SeekFrom};

// =============================================================================
// AccumulatorHalfKPDynamic - 動的サイズのアキュムレータ
// =============================================================================

/// 動的サイズのHalfKP用アキュムレータ（64バイトアライン済み）
pub struct AccumulatorHalfKPDynamic {
    /// アキュムレータバッファ [perspective][L1]（SIMD最適化のため64バイトアライン）
    pub accumulation: [AlignedBox<i16>; 2],
    /// 計算済みフラグ
    pub computed_accumulation: bool,
    /// L1 サイズ
    pub l1: usize,
}

impl AccumulatorHalfKPDynamic {
    /// # 制約
    ///
    /// `l1` は SIMD 最適化のため 16 の倍数である必要がある。
    /// 一般的な値: 256, 512, 1024
    pub fn new(l1: usize) -> Self {
        debug_assert!(
            l1.is_multiple_of(16),
            "L1 size must be multiple of 16 for SIMD optimization, got {l1}"
        );
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

impl Clone for AccumulatorHalfKPDynamic {
    fn clone(&self) -> Self {
        Self {
            accumulation: [self.accumulation[0].clone(), self.accumulation[1].clone()],
            computed_accumulation: self.computed_accumulation,
            l1: self.l1,
        }
    }
}

// =============================================================================
// AccumulatorStackHalfKPDynamic - アキュムレータスタック
// =============================================================================

/// スタックエントリ
pub struct AccumulatorEntryHalfKPDynamic {
    pub accumulator: AccumulatorHalfKPDynamic,
    pub dirty_piece: DirtyPiece,
    pub previous: Option<usize>,
}

/// アキュムレータスタック
pub struct AccumulatorStackHalfKPDynamic {
    entries: Vec<AccumulatorEntryHalfKPDynamic>,
    current_idx: usize,
    l1: usize,
}

impl AccumulatorStackHalfKPDynamic {
    /// 新規作成
    pub fn new(l1: usize) -> Self {
        let mut entries = Vec::with_capacity(128);
        entries.push(AccumulatorEntryHalfKPDynamic {
            accumulator: AccumulatorHalfKPDynamic::new(l1),
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
    pub fn current(&self) -> &AccumulatorEntryHalfKPDynamic {
        &self.entries[self.current_idx]
    }

    /// 現在のエントリを取得（可変）
    pub fn current_mut(&mut self) -> &mut AccumulatorEntryHalfKPDynamic {
        &mut self.entries[self.current_idx]
    }

    /// プッシュ
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        let prev_idx = self.current_idx;
        self.current_idx = self.entries.len();
        self.entries.push(AccumulatorEntryHalfKPDynamic {
            accumulator: AccumulatorHalfKPDynamic::new(self.l1),
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

        // 現局面で玉が動いていたら差分更新不可
        if current.dirty_piece.king_moved[0] || current.dirty_piece.king_moved[1] {
            return None;
        }

        // 直前局面をチェック
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

            // さらに前の局面へ
            let next_prev_idx = prev.previous?;

            // 玉が動いていたら打ち切り
            if prev.dirty_piece.king_moved[0] || prev.dirty_piece.king_moved[1] {
                return None;
            }

            prev_idx = next_prev_idx;
            depth += 1;
        }
    }

    /// 指定インデックスのエントリを取得
    pub fn entry_at(&self, idx: usize) -> &AccumulatorEntryHalfKPDynamic {
        &self.entries[idx]
    }

    /// 指定インデックスのエントリを取得（可変）
    pub fn entry_at_mut(&mut self, idx: usize) -> &mut AccumulatorEntryHalfKPDynamic {
        &mut self.entries[idx]
    }

    /// 前回と現在のアキュムレータを取得（可変）
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&AccumulatorHalfKPDynamic, &mut AccumulatorHalfKPDynamic) {
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

// =============================================================================
// FeatureTransformerHalfKPDynamic - 動的サイズのFeature Transformer
// =============================================================================

/// 動的サイズのHalfKP用Feature Transformer
pub struct FeatureTransformerHalfKPDynamic {
    /// バイアス [L1]
    pub biases: Vec<i16>,
    /// 重み [input_dimensions][L1]
    pub weights: AlignedBox<i16>,
    /// 出力次元数 (L1)
    pub l1: usize,
    /// 入力次元数
    pub input_dim: usize,
}

impl FeatureTransformerHalfKPDynamic {
    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R, l1: usize) -> io::Result<Self> {
        let input_dim = HALFKP_DIMENSIONS;

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
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorHalfKPDynamic) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = &mut acc.accumulation[p];

            // バイアスで初期化
            accumulation.copy_from_slice(&self.biases);

            // アクティブ特徴量を加算
            let active_indices = HalfKPFeatureSet::collect_active_indices(pos, perspective);
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
        acc: &mut AccumulatorHalfKPDynamic,
        prev_acc: &AccumulatorHalfKPDynamic,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = HalfKPFeatureSet::needs_refresh(dirty_piece, perspective);

            if reset {
                // リフレッシュ
                acc.accumulation[p].copy_from_slice(&self.biases);
                let active_indices = HalfKPFeatureSet::collect_active_indices(pos, perspective);
                for &index in active_indices.iter() {
                    self.add_weights(&mut acc.accumulation[p], index);
                }
            } else {
                // 差分更新
                let (removed, added) = HalfKPFeatureSet::collect_changed_indices(
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
        stack: &mut AccumulatorStackHalfKPDynamic,
        source_idx: usize,
    ) -> bool {
        let Some(path) = stack.collect_path(source_idx) else {
            return false;
        };

        // ソースからコピー
        let current_idx = stack.current_index();
        for p in 0..2 {
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
                let (removed, added) =
                    HalfKPFeatureSet::collect_changed_indices(&dirty_piece, perspective, king_sq);

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
    ///
    /// # 制約
    ///
    /// - `accumulation` は 32 バイトアライン済み（AlignedBox 使用）
    /// - `self.l1` は 16 の倍数
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16], index: usize) {
        let offset = index * self.l1;
        let weights = &self.weights[offset..offset + self.l1];

        // AVX2: 256bit = 16 x i16
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                debug_assert!(
                    (acc_ptr as usize).is_multiple_of(32),
                    "accumulation must be 32-byte aligned for AVX2"
                );
                debug_assert!(
                    (weight_ptr as usize).is_multiple_of(32),
                    "weights must be 32-byte aligned for AVX2"
                );

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

        // SSE2: 128bit = 8 x i16
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
    ///
    /// # 制約
    ///
    /// - `accumulation` は 32 バイトアライン済み（AlignedBox 使用）
    /// - `self.l1` は 16 の倍数
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16], index: usize) {
        let offset = index * self.l1;
        let weights = &self.weights[offset..offset + self.l1];

        // AVX2: 256bit = 16 x i16
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                debug_assert!(
                    (acc_ptr as usize).is_multiple_of(32),
                    "accumulation must be 32-byte aligned for AVX2"
                );
                debug_assert!(
                    (weight_ptr as usize).is_multiple_of(32),
                    "weights must be 32-byte aligned for AVX2"
                );

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

        // SSE2: 128bit = 8 x i16
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
    ///
    /// # 制約
    ///
    /// - `acc.accumulation` は 32 バイトアライン済み（AlignedBox 使用）
    /// - `self.l1` は 16 の倍数
    pub fn transform(
        &self,
        acc: &AccumulatorHalfKPDynamic,
        side_to_move: Color,
        output: &mut [u8],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        // AVX2: i16→u8パック + クリップ [0, 127]
        // 32要素/イテレーションで最適化
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

                    debug_assert!(
                        (acc_ptr as usize).is_multiple_of(32),
                        "accumulation must be 32-byte aligned for AVX2"
                    );

                    // 32要素ずつ処理（2x __m256i → 1x __m256i出力）
                    let num_pairs = self.l1 / 32;
                    for i in 0..num_pairs {
                        // 32個のi16をロード
                        let v0 = _mm256_load_si256(acc_ptr.add(i * 32) as *const __m256i);
                        let v1 = _mm256_load_si256(acc_ptr.add(i * 32 + 16) as *const __m256i);

                        // クランプ [0, 127]
                        let clamped0 = _mm256_min_epi16(_mm256_max_epi16(v0, zero), max_val);
                        let clamped1 = _mm256_min_epi16(_mm256_max_epi16(v1, zero), max_val);

                        // パック: 32個のi16 → 32個のu8
                        // packus_epi16はレーン内で動作するため、結果は [v0_lo, v1_lo | v0_hi, v1_hi]
                        let packed = _mm256_packus_epi16(clamped0, clamped1);

                        // レーン再配置: [0,1,2,3] → [0,2,1,3] で正しい順序に
                        let result = _mm256_permute4x64_epi64(packed, 0b11011000);

                        // 32バイト出力
                        _mm256_storeu_si256(out_ptr.add(i * 32) as *mut __m256i, result);
                    }

                    // 残り16要素があれば処理（L1が32の倍数でない場合）
                    let remainder_start = num_pairs * 32;
                    if remainder_start < self.l1 {
                        let v = _mm256_load_si256(acc_ptr.add(remainder_start) as *const __m256i);
                        let clamped = _mm256_min_epi16(_mm256_max_epi16(v, zero), max_val);
                        let packed = _mm256_packus_epi16(clamped, clamped);
                        let result = _mm256_permute4x64_epi64(packed, 0b11011000);
                        _mm_storeu_si128(
                            out_ptr.add(remainder_start) as *mut __m128i,
                            _mm256_castsi256_si128(result),
                        );
                    }
                }
            }
            return;
        }

        // SSE2: i16→u8パック + クリップ
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

    /// SCReLU用: i16出力版transform (ClippedReLU適用なし)
    pub fn transform_raw(
        &self,
        acc: &AccumulatorHalfKPDynamic,
        side_to_move: Color,
        output: &mut [i16],
    ) {
        let perspectives = [side_to_move, !side_to_move];

        for (p, &perspective) in perspectives.iter().enumerate() {
            let out_offset = self.l1 * p;
            let accumulation = &acc.accumulation[perspective as usize];
            output[out_offset..out_offset + self.l1].copy_from_slice(&accumulation[..self.l1]);
        }
    }
}

// =============================================================================
// NetworkHalfKPDynamic - 動的サイズのネットワーク
// =============================================================================

/// HalfKP 特徴量 + 動的サイズ FC 層のネットワーク
///
/// アーキテクチャ表記 `L1xN-L2-L3` の意味:
/// - L1: Feature Transformer 出力次元（片側）
/// - L1*2: Hidden1 入力次元（両視点連結）
/// - L2: Hidden1 出力次元
/// - L3: Hidden2 出力次元
///
/// 例: 512x2-8-96 → L1=512, Hidden1入力=1024, L2=8, L3=96
///
/// 256x2-32-32 固定の場合は既存の `Network` を使用。
pub struct NetworkHalfKPDynamic {
    /// 特徴量変換器 (入力 → L1)
    pub feature_transformer: FeatureTransformerHalfKPDynamic,
    /// 隠れ層1: L1*2 → L2（両視点の連結が入力）
    pub l1: AffineTransformDynamic,
    /// 隠れ層2: L2 → L3
    pub l2: AffineTransformDynamic,
    /// 出力層: L3 → 1
    pub output: AffineTransformDynamic,
    /// L1: Feature Transformer 出力次元（片側）
    pub arch_l1: usize,
    /// L2: 隠れ層1 出力次元
    pub arch_l2: usize,
    /// L3: 隠れ層2 出力次元
    pub arch_l3: usize,
    /// SCReLU を使用するかどうか
    pub use_screlu: bool,
    /// 評価値スケーリング係数
    pub fv_scale: i32,
    /// SCReLU の量子化係数 QA (127 or 255)
    pub screlu_qa: i16,
}

impl NetworkHalfKPDynamic {
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

        // HalfKP バージョンのみ許容
        if version != NNUE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Invalid NNUE version for HalfKP: {version:#x}, expected {NNUE_VERSION:#x}"
                ),
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
        let arch_str = String::from_utf8_lossy(&arch);

        // SCReLU 検出
        let use_screlu = arch_str.contains("SCReLU");

        // FV_SCALE 検出
        let fv_scale = parse_fv_scale_from_arch(&arch_str).unwrap_or_else(|| {
            // フォールバック: leb128圧縮の有無でヒューリスティック判定
            if arch_str.contains("leb128") {
                FV_SCALE_HALFKA
            } else {
                FV_SCALE
            }
        });

        // QA 検出: arch_str に "qa=N" が含まれていればその値を使用
        // デフォルトは 127（YaneuraOu の ClippedReLU 由来の量子化係数）
        let screlu_qa = parse_qa_from_arch(&arch_str).unwrap_or(127);

        // Feature Transformer ハッシュ
        reader.read_exact(&mut buf4)?;
        let _ft_hash = u32::from_le_bytes(buf4);

        // Feature Transformer
        let feature_transformer = FeatureTransformerHalfKPDynamic::read(reader, l1)?;

        // FC layers ハッシュ
        reader.read_exact(&mut buf4)?;
        let _fc_hash = u32::from_le_bytes(buf4);

        // l1: L1*2 → L2
        let l1_layer = AffineTransformDynamic::read(reader, l1 * 2, l2)?;

        // l2: L2 → L3
        let l2_layer = AffineTransformDynamic::read(reader, l2, l3)?;

        // output: L3 → 1
        let output_layer = AffineTransformDynamic::read(reader, l3, 1)?;

        // QA > SCRELU_DEFAULT_QA の場合、全層の bias スケールを修正
        //
        // 背景:
        // - bullet-shogi は bias を QA×QB でスケールする
        // - しかし FT SCReLU 出力は QA に依存せず 0〜SCRELU_DEFAULT_QA に正規化される
        // - そのため L1 積和のスケールは常に SCRELU_DEFAULT_QA×QB
        // - QA > SCRELU_DEFAULT_QA の場合、bias スケールと積和スケールが不一致
        // - bias を QA/SCRELU_DEFAULT_QA で割ることでスケールを統一
        //
        // 注: output layer の bias 修正は棋力に影響しない（評価値のオフセットが変わるだけ）が、
        //     GUIに表示される評価値の絶対値が正確になるため修正する
        let mut l1_layer = l1_layer;
        let mut l2_layer = l2_layer;
        let mut output_layer = output_layer;
        if use_screlu && screlu_qa as i32 > SCRELU_DEFAULT_QA {
            let bias_scale = screlu_qa as i32 / SCRELU_DEFAULT_QA;
            for bias in l1_layer.biases.iter_mut() {
                *bias /= bias_scale;
            }
            for bias in l2_layer.biases.iter_mut() {
                *bias /= bias_scale;
            }
            for bias in output_layer.biases.iter_mut() {
                *bias /= bias_scale;
            }
        }

        Ok(Self {
            feature_transformer,
            l1: l1_layer,
            l2: l2_layer,
            output: output_layer,
            arch_l1: l1,
            arch_l2: l2,
            arch_l3: l3,
            use_screlu,
            fv_scale,
            screlu_qa,
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

        // アーキテクチャ文字列から L1, L2, L3 をパース
        let l1 = Self::parse_l1_from_arch(&arch_str).unwrap_or(512);

        // L2, L3 をパース（フォールバック付き）
        let fallback = match l1 {
            256 => (32, 32),
            512 => (8, 96), // 512x2-8-96 がデフォルト
            1024 => (8, 32),
            _ => (32, 32),
        };
        let (l2, l3) = Self::parse_l2_l3_from_arch(&arch_str).unwrap_or(fallback);

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

    /// アーキテクチャ文字列から L2, L3 をパース
    fn parse_l2_l3_from_arch(arch: &str) -> Option<(usize, usize)> {
        // bullet-shogi の l2=N,l3=N フィールドを試す
        let l2 = Self::extract_field(arch, "l2=");
        let l3 = Self::extract_field(arch, "l3=");
        if let (Some(l2_val), Some(l3_val)) = (l2, l3) {
            return Some((l2_val, l3_val));
        }

        // nnue-pytorch 形式: AffineTransform[OUT<-IN] パターン
        let mut layers: Vec<(usize, usize)> = Vec::new();
        for cap in arch.match_indices("AffineTransform[") {
            let start = cap.0 + "AffineTransform[".len();
            if let Some(end) = arch[start..].find(']') {
                let content = &arch[start..start + end];
                if let Some(arrow_idx) = content.find("<-") {
                    let out_str = &content[..arrow_idx];
                    let in_str = &content[arrow_idx + 2..];
                    if let (Ok(out), Ok(inp)) = (out_str.parse::<usize>(), in_str.parse::<usize>())
                    {
                        layers.push((out, inp));
                    }
                }
            }
        }

        // 逆順にして最内側から: [L2<-L1*2] (L1層), [L3<-L2] (L2層), [1<-L3] (output)
        layers.reverse();
        if layers.len() >= 3 {
            let l2 = layers[0].0;
            let l3 = layers[1].0;
            return Some((l2, l3));
        }

        None
    }

    /// arch 文字列からフィールドを抽出
    fn extract_field(arch: &str, prefix: &str) -> Option<usize> {
        if let Some(idx) = arch.find(prefix) {
            let start = idx + prefix.len();
            let end =
                arch[start..].find(|c: char| !c.is_ascii_digit()).unwrap_or(arch[start..].len());
            return arch[start..start + end].parse().ok();
        }
        None
    }

    /// 評価値を計算
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorHalfKPDynamic) -> Value {
        if self.use_screlu {
            self.evaluate_screlu(pos, acc)
        } else {
            self.evaluate_clipped_relu(pos, acc)
        }
    }

    /// ClippedReLU 版の評価値計算
    fn evaluate_clipped_relu(&self, pos: &Position, acc: &AccumulatorHalfKPDynamic) -> Value {
        // 変換済み特徴量
        let ft_out_size = self.arch_l1 * 2;
        let mut transformed = vec![0u8; ft_out_size];
        self.feature_transformer.transform(acc, pos.side_to_move(), &mut transformed);

        // 隠れ層1
        let mut l1_out = vec![0i32; self.arch_l2];
        self.l1.propagate(&transformed, &mut l1_out);

        let mut l1_relu = vec![0u8; self.arch_l2];
        clipped_relu_dynamic(&l1_out, &mut l1_relu);

        // 隠れ層2
        let mut l2_out = vec![0i32; self.arch_l3];
        self.l2.propagate(&l1_relu, &mut l2_out);

        let mut l2_relu = vec![0u8; self.arch_l3];
        clipped_relu_dynamic(&l2_out, &mut l2_relu);

        // 出力層
        let mut output = vec![0i32; 1];
        self.output.propagate(&l2_relu, &mut output);

        // スケーリング（オーバーライド設定があればそちらを優先）
        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        Value::new(output[0] / fv_scale)
    }

    /// SCReLU 版の評価値計算
    ///
    /// HalfKA Dynamic と同じパターンで、全層に SCReLU (二乗活性化) を適用。
    fn evaluate_screlu(&self, pos: &Position, acc: &AccumulatorHalfKPDynamic) -> Value {
        use crate::nnue::layers::SCReLUDynamic;

        let ft_out_size = self.arch_l1 * 2;
        let mut ft_out_i16 = vec![0i16; ft_out_size];
        self.feature_transformer.transform_raw(acc, pos.side_to_move(), &mut ft_out_i16);

        // FT出力に SCReLU 適用 (i16 → u8)
        // QA=127: clamp(x, 0, 127)² >> 7 → u8 (0〜126)
        // QA=255: clamp(x, 0, 255)² >> 9 → u8 (0〜127)
        let mut transformed = vec![0u8; ft_out_size];
        SCReLUDynamic::propagate_i16_to_u8_with_qa(&ft_out_i16, &mut transformed, self.screlu_qa);

        // 注: QA > 127 の場合の正規化は読み込み時の bias スケール修正で対応済み

        // 隠れ層1
        let mut l1_out = vec![0i32; self.arch_l2];
        self.l1.propagate(&transformed, &mut l1_out);

        // L1出力に SCReLU 適用 (i32 → u8)
        // clamp(x >> 6, 0, 127)² >> 7 → u8
        let mut l1_relu = vec![0u8; self.arch_l2];
        SCReLUDynamic::propagate_i32_to_u8(&l1_out, &mut l1_relu);

        // 隠れ層2
        let mut l2_out = vec![0i32; self.arch_l3];
        self.l2.propagate(&l1_relu, &mut l2_out);

        // L2出力に SCReLU 適用 (i32 → u8)
        let mut l2_relu = vec![0u8; self.arch_l3];
        SCReLUDynamic::propagate_i32_to_u8(&l2_out, &mut l2_relu);

        // 出力層
        let mut output = vec![0i32; 1];
        self.output.propagate(&l2_relu, &mut output);

        // スケーリング（オーバーライド設定があればそちらを優先）
        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        Value::new(output[0] / fv_scale)
    }

    /// アーキテクチャ名を取得
    pub fn arch_name(&self) -> String {
        let suffix = if self.use_screlu { "-SCReLU" } else { "" };
        format!("HalfKPDynamic {}x2-{}-{}{}", self.arch_l1, self.arch_l2, self.arch_l3, suffix)
    }
}

/// ClippedReLU（動的サイズ版）
fn clipped_relu_dynamic(input: &[i32], output: &mut [u8]) {
    for (i, &v) in input.iter().enumerate() {
        let shifted = v >> WEIGHT_SCALE_BITS;
        output[i] = shifted.clamp(0, 127) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_new() {
        let acc = AccumulatorHalfKPDynamic::new(512);
        assert_eq!(acc.l1, 512);
        assert!(!acc.computed_accumulation);
        assert_eq!(acc.accumulation[0].len(), 512);
        assert_eq!(acc.accumulation[1].len(), 512);
    }

    #[test]
    fn test_accumulator_stack_new() {
        let stack = AccumulatorStackHalfKPDynamic::new(512);
        assert_eq!(stack.l1(), 512);
        assert!(!stack.current().accumulator.computed_accumulation);
    }

    #[test]
    fn test_parse_l1_from_arch() {
        // [512x2] パターン
        assert_eq!(
            NetworkHalfKPDynamic::parse_l1_from_arch("Features=HalfKP[125388->512x2]"),
            Some(512)
        );

        // [1024x2] パターン
        assert_eq!(
            NetworkHalfKPDynamic::parse_l1_from_arch("Features=HalfKP[125388->1024x2]"),
            Some(1024)
        );

        // [256x2] パターン
        assert_eq!(
            NetworkHalfKPDynamic::parse_l1_from_arch("Features=HalfKP[125388->256x2]"),
            Some(256)
        );
    }

    #[test]
    fn test_parse_l2_l3_from_arch() {
        // bullet-shogi 形式
        let arch = "Features=halfkp[125388->512x2],fv_scale=16,l2=8,l3=96,qa=255,qb=64,scale=1600";
        assert_eq!(NetworkHalfKPDynamic::parse_l2_l3_from_arch(arch), Some((8, 96)));

        // nnue-pytorch 形式
        let arch = "Features=HalfKP[125388->512x2],Network=AffineTransform[1<-96](ClippedReLU[96](AffineTransform[96<-8](ClippedReLU[8](AffineTransform[8<-1024](InputSlice[1024(0:1024)])))))";
        assert_eq!(NetworkHalfKPDynamic::parse_l2_l3_from_arch(arch), Some((8, 96)));
    }

    #[test]
    fn test_clipped_relu_dynamic() {
        let input = vec![0, 64, 128, 256, -64, 8192];
        let mut output = vec![0u8; 6];
        clipped_relu_dynamic(&input, &mut output);

        // WEIGHT_SCALE_BITS = 6 なので、64で割る
        assert_eq!(output[0], 0); // 0 / 64 = 0
        assert_eq!(output[1], 1); // 64 / 64 = 1
        assert_eq!(output[2], 2); // 128 / 64 = 2
        assert_eq!(output[3], 4); // 256 / 64 = 4
        assert_eq!(output[4], 0); // -64 / 64 = -1 -> clamped to 0
        assert_eq!(output[5], 127); // 8192 / 64 = 128 -> clamped to 127
    }
}
