//! AccumulatorLayerStacks - LayerStacksアーキテクチャ用の1536次元アキュムレータ
//!
//! LayerStacks の Feature Transformer は各視点で 1536 次元を出力する。
//! 既存の Accumulator（256次元、HalfKP用）とは別に管理する。

use super::accumulator::{DirtyPiece, IndexList, MAX_PATH_LENGTH};
use super::constants::NNUE_PYTORCH_L1;
use crate::types::MAX_PLY;

/// LayerStacks用アキュムレータ（1536次元）
#[repr(C, align(64))]
#[derive(Clone)]
pub struct AccumulatorLayerStacks {
    /// 各視点の累積値 [perspective][dimension]
    /// perspective: 0 = Black, 1 = White
    pub accumulation: [[i16; NNUE_PYTORCH_L1]; 2],

    /// 計算済みフラグ
    pub computed_accumulation: bool,

    /// スコア計算済みフラグ（差分更新時にリセット）
    pub computed_score: bool,
}

impl AccumulatorLayerStacks {
    /// 新規作成
    pub fn new() -> Self {
        Self {
            accumulation: [[0; NNUE_PYTORCH_L1]; 2],
            computed_accumulation: false,
            computed_score: false,
        }
    }

    /// 指定視点の累積値を取得
    #[inline]
    pub fn get(&self, perspective: usize) -> &[i16; NNUE_PYTORCH_L1] {
        &self.accumulation[perspective]
    }

    /// 指定視点の累積値を取得（可変）
    #[inline]
    pub fn get_mut(&mut self, perspective: usize) -> &mut [i16; NNUE_PYTORCH_L1] {
        &mut self.accumulation[perspective]
    }
}

impl Default for AccumulatorLayerStacks {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// DirtyPiece - 駒の変更情報（LayerStacks用、accumulator.rsから再エクスポート）
// =============================================================================

// DirtyPiece は accumulator.rs で定義済み

// =============================================================================
// StackEntryLayerStacks - スタックエントリ
// =============================================================================

/// スタックエントリ（LayerStacks用）
pub struct StackEntryLayerStacks {
    /// アキュムレータ
    pub accumulator: AccumulatorLayerStacks,
    /// 変更された駒の情報
    pub dirty_piece: DirtyPiece,
    /// 直前のエントリインデックス（差分計算用）
    pub previous: Option<usize>,
}

impl StackEntryLayerStacks {
    pub fn new() -> Self {
        Self {
            accumulator: AccumulatorLayerStacks::new(),
            dirty_piece: DirtyPiece::default(),
            previous: None,
        }
    }
}

impl Default for StackEntryLayerStacks {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// AccumulatorStackLayerStacks - スタック管理
// =============================================================================

/// アキュムレータスタック（LayerStacks用）
pub struct AccumulatorStackLayerStacks {
    /// スタックエントリ
    entries: Box<[StackEntryLayerStacks]>,
    /// 現在のインデックス
    current: usize,
}

impl AccumulatorStackLayerStacks {
    const STACK_SIZE: usize = (MAX_PLY as usize) + 16;

    /// 新規作成
    pub fn new() -> Self {
        let entries: Vec<StackEntryLayerStacks> =
            (0..Self::STACK_SIZE).map(|_| StackEntryLayerStacks::new()).collect();

        Self {
            entries: entries.into_boxed_slice(),
            current: 0,
        }
    }

    /// 現在のエントリを取得
    #[inline]
    pub fn current(&self) -> &StackEntryLayerStacks {
        &self.entries[self.current]
    }

    /// 現在のエントリを取得（可変）
    #[inline]
    pub fn current_mut(&mut self) -> &mut StackEntryLayerStacks {
        &mut self.entries[self.current]
    }

    /// 現在のインデックスを取得
    #[inline]
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// 指定インデックスのエントリを取得
    #[inline]
    pub fn entry_at(&self, index: usize) -> &StackEntryLayerStacks {
        &self.entries[index]
    }

    /// 指定インデックスのエントリを取得（可変）
    #[inline]
    pub fn entry_at_mut(&mut self, index: usize) -> &mut StackEntryLayerStacks {
        &mut self.entries[index]
    }

    /// スタックをプッシュ
    #[inline]
    pub fn push(&mut self) {
        let prev = self.current;
        self.current += 1;
        debug_assert!(self.current < Self::STACK_SIZE);
        self.entries[self.current].previous = Some(prev);
        self.entries[self.current].accumulator.computed_accumulation = false;
        self.entries[self.current].accumulator.computed_score = false;
        self.entries[self.current].dirty_piece = DirtyPiece::default();
    }

    /// スタックをポップ
    #[inline]
    pub fn pop(&mut self) {
        debug_assert!(self.current > 0);
        self.current -= 1;
    }

    /// 前回と現在のアキュムレータを同時に取得（clone不要）
    ///
    /// `split_at_mut`を使用して、prev_idx の accumulator への不変参照と
    /// 現在の accumulator への可変参照を同時に返す。
    #[inline]
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&AccumulatorLayerStacks, &mut AccumulatorLayerStacks) {
        let cur_idx = self.current;
        debug_assert!(prev_idx < cur_idx, "prev_idx ({prev_idx}) must be < cur_idx ({cur_idx})");
        let (left, right) = self.entries.split_at_mut(cur_idx);
        (&left[prev_idx].accumulator, &mut right[0].accumulator)
    }

    /// スタックをリセット
    #[inline]
    pub fn reset(&mut self) {
        self.current = 0;
        self.entries[0].accumulator.computed_accumulation = false;
        self.entries[0].accumulator.computed_score = false;
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

        let current = &self.entries[self.current];

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

    /// 指定インデックスから現在位置までのパスを収集
    ///
    /// 戻り値:
    /// - Some(path): source_idx に到達できた場合、source側から適用する順のインデックス列
    /// - None: パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
    pub fn collect_path(&self, source_idx: usize) -> Option<IndexList<MAX_PATH_LENGTH>> {
        self.collect_path_internal(source_idx)
    }

    fn collect_path_internal(&self, source_idx: usize) -> Option<IndexList<MAX_PATH_LENGTH>> {
        let mut path = IndexList::new();
        let mut idx = self.current;

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

impl Default for AccumulatorStackLayerStacks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_new() {
        let acc = AccumulatorLayerStacks::new();
        assert!(!acc.computed_accumulation);
        assert_eq!(acc.accumulation[0].len(), NNUE_PYTORCH_L1);
    }

    #[test]
    fn test_stack_push_pop() {
        let mut stack = AccumulatorStackLayerStacks::new();
        assert_eq!(stack.current_index(), 0);

        stack.push();
        assert_eq!(stack.current_index(), 1);
        assert_eq!(stack.current().previous, Some(0));

        stack.pop();
        assert_eq!(stack.current_index(), 0);
    }
}
