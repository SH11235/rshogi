//! AccumulatorNnuePytorch - nnue-pytorch用の1536次元アキュムレータ
//!
//! nnue-pytorch の Feature Transformer は各視点で 1536 次元を出力する。
//! 既存の Accumulator（256次元）とは別に管理する。

use super::accumulator::{DirtyPiece, IndexList, MAX_PATH_LENGTH};
use super::constants::NNUE_PYTORCH_L1;
use crate::types::MAX_PLY;

/// nnue-pytorch用アキュムレータ（1536次元）
#[repr(C, align(64))]
#[derive(Clone)]
pub struct AccumulatorNnuePytorch {
    /// 各視点の累積値 [perspective][dimension]
    /// perspective: 0 = Black, 1 = White
    pub accumulation: [[i16; NNUE_PYTORCH_L1]; 2],

    /// 計算済みフラグ
    pub computed_accumulation: bool,

    /// スコア計算済みフラグ（差分更新時にリセット）
    pub computed_score: bool,
}

impl AccumulatorNnuePytorch {
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

impl Default for AccumulatorNnuePytorch {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// DirtyPiece - 駒の変更情報（nnue-pytorch用、accumulator.rsから再エクスポート）
// =============================================================================

// DirtyPiece は accumulator.rs で定義済み

// =============================================================================
// StackEntryNnuePytorch - スタックエントリ
// =============================================================================

/// スタックエントリ（nnue-pytorch用）
pub struct StackEntryNnuePytorch {
    /// アキュムレータ
    pub accumulator: AccumulatorNnuePytorch,
    /// 変更された駒の情報
    pub dirty_piece: DirtyPiece,
    /// 直前のエントリインデックス（差分計算用）
    pub previous: Option<usize>,
}

impl StackEntryNnuePytorch {
    pub fn new() -> Self {
        Self {
            accumulator: AccumulatorNnuePytorch::new(),
            dirty_piece: DirtyPiece::default(),
            previous: None,
        }
    }
}

impl Default for StackEntryNnuePytorch {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// AccumulatorStackNnuePytorch - スタック管理
// =============================================================================

/// アキュムレータスタック（nnue-pytorch用）
pub struct AccumulatorStackNnuePytorch {
    /// スタックエントリ
    entries: Box<[StackEntryNnuePytorch]>,
    /// 現在のインデックス
    current: usize,
}

impl AccumulatorStackNnuePytorch {
    const STACK_SIZE: usize = (MAX_PLY as usize) + 16;

    /// 新規作成
    pub fn new() -> Self {
        let entries: Vec<StackEntryNnuePytorch> =
            (0..Self::STACK_SIZE).map(|_| StackEntryNnuePytorch::new()).collect();

        Self {
            entries: entries.into_boxed_slice(),
            current: 0,
        }
    }

    /// 現在のエントリを取得
    #[inline]
    pub fn current(&self) -> &StackEntryNnuePytorch {
        &self.entries[self.current]
    }

    /// 現在のエントリを取得（可変）
    #[inline]
    pub fn current_mut(&mut self) -> &mut StackEntryNnuePytorch {
        &mut self.entries[self.current]
    }

    /// 現在のインデックスを取得
    #[inline]
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// 指定インデックスのエントリを取得
    #[inline]
    pub fn entry_at(&self, index: usize) -> &StackEntryNnuePytorch {
        &self.entries[index]
    }

    /// 指定インデックスのエントリを取得（可変）
    #[inline]
    pub fn entry_at_mut(&mut self, index: usize) -> &mut StackEntryNnuePytorch {
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

    /// スタックをリセット
    #[inline]
    pub fn reset(&mut self) {
        self.current = 0;
        self.entries[0].accumulator.computed_accumulation = false;
        self.entries[0].accumulator.computed_score = false;
        self.entries[0].previous = None;
    }

    /// 祖先を辿って使用可能なアキュムレータを探す
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        const MAX_DEPTH: usize = MAX_PATH_LENGTH;

        let mut idx = self.current;
        let mut depth = 0;

        while depth < MAX_DEPTH {
            let entry = &self.entries[idx];

            // 計算済みかチェック
            if entry.accumulator.computed_accumulation {
                // 玉が移動していないかチェック
                let path = self.collect_path_internal(idx);
                let has_king_move = path.iter().any(|&i| {
                    let e = &self.entries[i];
                    e.dirty_piece.king_moved[0] || e.dirty_piece.king_moved[1]
                });

                if !has_king_move {
                    return Some((idx, depth));
                }
            }

            // 前のエントリへ
            match entry.previous {
                Some(prev) => {
                    idx = prev;
                    depth += 1;
                }
                None => break,
            }
        }

        None
    }

    /// 指定インデックスから現在位置までのパスを収集
    pub fn collect_path(&self, source_idx: usize) -> IndexList<MAX_PATH_LENGTH> {
        self.collect_path_internal(source_idx)
    }

    fn collect_path_internal(&self, source_idx: usize) -> IndexList<MAX_PATH_LENGTH> {
        let mut path = IndexList::new();
        let mut idx = self.current;

        while idx != source_idx && path.len() < MAX_PATH_LENGTH {
            path.push(idx);
            match self.entries[idx].previous {
                Some(prev) => idx = prev,
                None => break,
            }
        }

        path.reverse();
        path
    }
}

impl Default for AccumulatorStackNnuePytorch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_new() {
        let acc = AccumulatorNnuePytorch::new();
        assert!(!acc.computed_accumulation);
        assert_eq!(acc.accumulation[0].len(), NNUE_PYTORCH_L1);
    }

    #[test]
    fn test_stack_push_pop() {
        let mut stack = AccumulatorStackNnuePytorch::new();
        assert_eq!(stack.current_index(), 0);

        stack.push();
        assert_eq!(stack.current_index(), 1);
        assert_eq!(stack.current().previous, Some(0));

        stack.pop();
        assert_eq!(stack.current_index(), 0);
    }
}
