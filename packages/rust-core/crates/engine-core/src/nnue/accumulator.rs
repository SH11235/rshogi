//! Accumulator - 入力特徴量の累積値を保持
//!
//! HalfKP 特徴量を FeatureTransformer で変換した結果を視点ごとに保持し、
//! 差分更新対応の評価値計算を行うための中間バッファ。
//! 実際の差分更新ロジックは FeatureTransformer / `nnue::diff` 側にあり、
//! この型は `[perspective][dimension]` の累積ベクトルと計算済みフラグを管理する。
//!
//! AccumulatorStack は探索時の Accumulator と DirtyPiece を管理するスタック。
//! StateInfo から Accumulator を分離し、do_move での初期化コストを削減する。

use super::constants::{NUM_REFRESH_TRIGGERS, TRANSFORMED_FEATURE_DIMENSIONS};
use crate::types::{Color, Piece, PieceType, Square, Value, MAX_PLY};
use std::mem::MaybeUninit;

// =============================================================================
// IndexList - 固定長の特徴量インデックスリスト
// =============================================================================

/// 差分更新での最大変化特徴量数（駒3 + 手駒2 + 余裕）
pub const MAX_CHANGED_FEATURES: usize = 8;

/// 全特徴量取得での最大数（盤上38 + 手駒14 = 52）
pub const MAX_ACTIVE_FEATURES: usize = 52;

/// collect_path での最大パス長（find_usable_accumulator の MAX_DEPTH と同じ）
pub const MAX_PATH_LENGTH: usize = 8;

/// 固定長の特徴量インデックスリスト
///
/// Vec の代わりにスタック上の固定長配列を使用し、ヒープ割り当てを回避する。
/// MaybeUninit を使用して初期化コストをゼロにする。
#[derive(Clone, Copy)]
pub struct IndexList<const N: usize> {
    /// 未初期化領域を許容する配列
    indices: [MaybeUninit<usize>; N],
    /// 有効な要素数
    len: u8,
}

impl<const N: usize> IndexList<N> {
    /// 空のリストを作成（初期化コストゼロ）
    #[inline]
    pub fn new() -> Self {
        Self {
            // インラインconst ブロックで未初期化配列を安全に作成
            indices: [const { MaybeUninit::uninit() }; N],
            len: 0,
        }
    }

    /// 要素を追加
    #[inline]
    pub fn push(&mut self, index: usize) {
        debug_assert!((self.len as usize) < N, "IndexList overflow");
        // SAFETY: len < N なので範囲内。MaybeUninit への書き込みは常に安全
        self.indices[self.len as usize].write(index);
        self.len += 1;
    }

    /// イテレータを返す
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &usize> {
        // SAFETY: 0..len の範囲は全て初期化済み
        self.indices[..self.len as usize].iter().map(|v| unsafe { v.assume_init_ref() })
    }

    /// 空かどうか
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 要素数
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// 要素を逆順に並べ替え
    #[inline]
    pub fn reverse(&mut self) {
        // SAFETY: 0..len の範囲は全て初期化済み
        let slice = &mut self.indices[..self.len as usize];
        slice.reverse();
    }
}

impl<const N: usize> Default for IndexList<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// アライメントを保証するラッパー（64バイト = キャッシュライン）
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct Aligned<T: Copy>(pub T);

impl<T: Default + Copy> Default for Aligned<T> {
    fn default() -> Self {
        Self(T::default())
    }
}

/// Accumulatorの構造
/// 入力特徴量をアフィン変換した結果を保持
///
/// YaneuraOu の classic NNUE と同様に、トリガーごとに accumulation を分離。
/// `accumulation[perspective][trigger][dimension]` の構造で、
/// transform 時にトリガーごとの値を合算する。
/// 現在は NUM_REFRESH_TRIGGERS=1 なので従来と同等の動作。
#[repr(C, align(64))]
#[derive(Clone)]
pub struct Accumulator {
    /// 累積値 [perspective][trigger][dimension]
    /// - perspective: BLACK=0, WHITE=1
    /// - trigger: 0..NUM_REFRESH_TRIGGERS
    pub accumulation: [[Aligned<[i16; TRANSFORMED_FEATURE_DIMENSIONS]>; NUM_REFRESH_TRIGGERS]; 2],

    /// 計算済みの評価値（キャッシュ）
    pub score: Value,

    /// accumulationが計算済みかどうか
    pub computed_accumulation: bool,

    /// scoreが計算済みかどうか
    pub computed_score: bool,
}

impl Default for Accumulator {
    fn default() -> Self {
        Self {
            accumulation: [[Aligned([0i16; TRANSFORMED_FEATURE_DIMENSIONS]); NUM_REFRESH_TRIGGERS];
                2],
            score: Value::ZERO,
            computed_accumulation: false,
            computed_score: false,
        }
    }
}

impl Accumulator {
    /// 新しいAccumulatorを作成
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// リセット（計算済みフラグをクリア）
    #[inline]
    pub fn reset(&mut self) {
        self.computed_accumulation = false;
        self.computed_score = false;
    }

    /// 視点・トリガーごとの累積値への参照を取得
    #[inline]
    pub fn get(
        &self,
        perspective: usize,
        trigger: usize,
    ) -> &[i16; TRANSFORMED_FEATURE_DIMENSIONS] {
        &self.accumulation[perspective][trigger].0
    }

    /// 視点・トリガーごとの累積値への可変参照を取得
    #[inline]
    pub fn get_mut(
        &mut self,
        perspective: usize,
        trigger: usize,
    ) -> &mut [i16; TRANSFORMED_FEATURE_DIMENSIONS] {
        &mut self.accumulation[perspective][trigger].0
    }
}

// =============================================================================
// DirtyPiece - 差分更新用の駒移動情報
// =============================================================================

/// 差分更新用の駒移動情報（固定長バッファでヒープ確保を回避）
#[derive(Clone, Copy)]
pub struct DirtyPiece {
    /// 変化した駒（最大3つ: 動いた駒 + 取られた駒）
    pieces: [ChangedPiece; Self::MAX_PIECES],
    /// 有効な pieces 要素数
    pieces_len: u8,
    /// 手駒の変化（最大2つ: 打ち駒 or 取り駒による変化）
    hand_changes: [HandChange; Self::MAX_HAND_CHANGES],
    /// 有効な hand_changes 要素数
    hand_changes_len: u8,
    /// 玉が動いたかどうか [Color]
    pub king_moved: [bool; Color::NUM],
}

impl DirtyPiece {
    /// pieces の最大要素数
    pub const MAX_PIECES: usize = 3;
    /// hand_changes の最大要素数
    pub const MAX_HAND_CHANGES: usize = 2;

    /// 新しい DirtyPiece を作成
    #[inline]
    pub const fn new() -> Self {
        Self {
            pieces: [ChangedPiece::EMPTY; Self::MAX_PIECES],
            pieces_len: 0,
            hand_changes: [HandChange::EMPTY; Self::MAX_HAND_CHANGES],
            hand_changes_len: 0,
            king_moved: [false; Color::NUM],
        }
    }

    /// 情報をクリア
    #[inline]
    pub fn clear(&mut self) {
        self.pieces_len = 0;
        self.hand_changes_len = 0;
        self.king_moved = [false; Color::NUM];
    }

    /// 駒変化を追加
    #[inline]
    pub fn push_piece(&mut self, piece: ChangedPiece) {
        let idx = self.pieces_len as usize;
        self.pieces[idx] = piece;
        self.pieces_len += 1;
    }

    /// 手駒変化を追加
    #[inline]
    pub fn push_hand_change(&mut self, change: HandChange) {
        let idx = self.hand_changes_len as usize;
        self.hand_changes[idx] = change;
        self.hand_changes_len += 1;
    }

    /// 駒変化のスライスを取得
    #[inline]
    pub fn pieces(&self) -> &[ChangedPiece] {
        &self.pieces[..self.pieces_len as usize]
    }

    /// 手駒変化のスライスを取得
    #[inline]
    pub fn hand_changes(&self) -> &[HandChange] {
        &self.hand_changes[..self.hand_changes_len as usize]
    }
}

impl Default for DirtyPiece {
    fn default() -> Self {
        Self::new()
    }
}

/// 1 駒分の変更情報
#[derive(Clone, Copy)]
pub struct ChangedPiece {
    /// 駒の色
    pub color: Color,
    /// 変更前の駒（盤上に無ければ Piece::NONE）
    pub old_piece: Piece,
    /// 変更前の位置（盤上に無ければ None）
    pub old_sq: Option<Square>,
    /// 変更後の駒（盤上に無ければ Piece::NONE）
    pub new_piece: Piece,
    /// 変更後の位置（盤上に無ければ None）
    pub new_sq: Option<Square>,
}

impl ChangedPiece {
    /// 空の ChangedPiece（固定長配列の初期化用）
    pub const EMPTY: Self = Self {
        color: Color::Black,
        old_piece: Piece::NONE,
        old_sq: None,
        new_piece: Piece::NONE,
        new_sq: None,
    };
}

/// 手駒の変化情報
#[derive(Clone, Copy)]
pub struct HandChange {
    pub owner: Color,
    pub piece_type: PieceType,
    pub old_count: u8,
    pub new_count: u8,
}

impl HandChange {
    /// 空の HandChange（固定長配列の初期化用）
    pub const EMPTY: Self = Self {
        owner: Color::Black,
        piece_type: PieceType::Pawn,
        old_count: 0,
        new_count: 0,
    };
}

// =============================================================================
// AccumulatorStack - 探索時の Accumulator と DirtyPiece を管理
// =============================================================================

/// AccumulatorStackのエントリ
///
/// AccumulatorとDirtyPieceを対で管理する。
/// StateInfoからNNUE関連のフィールドを分離し、do_moveでの初期化コストを削減する。
#[repr(C, align(64))]
#[derive(Default)]
pub struct StackEntry {
    /// Accumulator（差分更新用の中間表現）
    pub accumulator: Accumulator,
    /// 差分更新用の駒移動情報
    pub dirty_piece: DirtyPiece,
    /// 前のエントリへのインデックス（祖先探索用）
    pub previous: Option<usize>,
}

/// AccumulatorStack - 探索時のAccumulatorを管理するスタック
///
/// SearchWorkerが所有し、Position.do_move/undo_moveと同期してpush/popする。
/// StateInfoからAccumulator/DirtyPieceを分離することで、do_moveでの
/// Accumulator::new()（約1KBゼロ初期化）を回避する。
pub struct AccumulatorStack {
    /// スタックエントリの配列（MAX_PLY + 1 要素、ヒープ確保）
    entries: Box<[StackEntry]>,
    /// 現在のスタックインデックス
    current_idx: usize,
}

impl AccumulatorStack {
    /// スタックのサイズ（MAX_PLY + 1）
    pub const SIZE: usize = (MAX_PLY + 1) as usize;

    /// 新しいAccumulatorStackを作成（ヒープに配置）
    pub fn new() -> Self {
        // Vec経由でヒープに確保し、Box<[T]>に変換
        let entries: Vec<StackEntry> = (0..Self::SIZE).map(|_| StackEntry::default()).collect();
        Self {
            entries: entries.into_boxed_slice(),
            current_idx: 0,
        }
    }

    /// スタックをリセット（探索開始時に呼び出す）
    pub fn reset(&mut self) {
        self.current_idx = 0;
        self.entries[0].accumulator.reset();
        self.entries[0].dirty_piece.clear();
        self.entries[0].previous = None;
    }

    /// 現在のエントリを取得
    #[inline]
    pub fn current(&self) -> &StackEntry {
        &self.entries[self.current_idx]
    }

    /// 現在のエントリを可変参照で取得
    #[inline]
    pub fn current_mut(&mut self) -> &mut StackEntry {
        &mut self.entries[self.current_idx]
    }

    /// 指定インデックスのエントリを取得
    #[inline]
    pub fn entry_at(&self, idx: usize) -> &StackEntry {
        &self.entries[idx]
    }

    /// 指定インデックスのエントリを可変参照で取得
    #[inline]
    pub fn entry_at_mut(&mut self, idx: usize) -> &mut StackEntry {
        &mut self.entries[idx]
    }

    /// 現在のインデックスを取得
    #[inline]
    pub fn current_index(&self) -> usize {
        self.current_idx
    }

    /// do_move時に呼び出す: 新しいエントリをpush
    ///
    /// DirtyPieceは呼び出し側で設定する。
    /// Accumulatorは計算済みフラグをリセットするだけで、配列の初期化は行わない。
    #[inline]
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        let prev_idx = self.current_idx;
        self.current_idx += 1;
        debug_assert!(self.current_idx < Self::SIZE, "AccumulatorStack overflow");

        let entry = &mut self.entries[self.current_idx];
        entry.previous = Some(prev_idx);
        entry.accumulator.reset(); // フラグのみリセット、配列初期化なし
        entry.dirty_piece = dirty_piece;
    }

    /// undo_move時に呼び出す: エントリをpop
    #[inline]
    pub fn pop(&mut self) {
        debug_assert!(self.current_idx > 0, "AccumulatorStack underflow");
        self.current_idx -= 1;
    }

    /// 祖先を遡って計算済みアキュムレータを探す
    ///
    /// 戻り値: Some((計算済みエントリのインデックス, 経由する局面数))
    ///         両視点で玉移動がない範囲で計算済み祖先が見つかった場合
    pub fn find_usable_accumulator(&self) -> Option<(usize, usize)> {
        const MAX_DEPTH: usize = 8;

        let current = &self.entries[self.current_idx];

        // 現局面で玉が動いていたら差分更新不可
        if current.dirty_piece.king_moved[Color::Black.index()]
            || current.dirty_piece.king_moved[Color::White.index()]
        {
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

            // さらに前の局面へ（ルートに達したらNone）
            let next_prev_idx = prev.previous?;

            // 玉が動いていたら打ち切り
            if prev.dirty_piece.king_moved[Color::Black.index()]
                || prev.dirty_piece.king_moved[Color::White.index()]
            {
                return None;
            }

            prev_idx = next_prev_idx;
            depth += 1;
        }
    }

    /// source_idxからcurrent_idxまでのパスを収集
    ///
    /// 戻り値: source側から適用する順のインデックス列
    pub fn collect_path(&self, source_idx: usize) -> IndexList<MAX_PATH_LENGTH> {
        let mut path = IndexList::new();
        let mut idx = self.current_idx;

        while idx != source_idx {
            path.push(idx);
            let entry = &self.entries[idx];
            match entry.previous {
                Some(prev_idx) => idx = prev_idx,
                None => {
                    debug_assert!(
                        false,
                        "Path broken: expected to reach source_idx={source_idx} but got None at idx={idx}"
                    );
                    return IndexList::new();
                }
            }
        }

        path.reverse();
        path
    }
}

impl Default for AccumulatorStack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_new() {
        let acc = Accumulator::new();
        assert!(!acc.computed_accumulation);
        assert!(!acc.computed_score);
        assert_eq!(acc.score, Value::ZERO);
    }

    #[test]
    fn test_accumulator_reset() {
        let mut acc = Accumulator::new();
        acc.computed_accumulation = true;
        acc.computed_score = true;

        acc.reset();

        assert!(!acc.computed_accumulation);
        assert!(!acc.computed_score);
    }

    #[test]
    fn test_accumulator_get() {
        let mut acc = Accumulator::new();
        // [perspective][trigger][dimension] 構造
        acc.accumulation[0][0].0[0] = 100;
        acc.accumulation[1][0].0[0] = 200;

        assert_eq!(acc.get(0, 0)[0], 100);
        assert_eq!(acc.get(1, 0)[0], 200);
    }

    #[test]
    fn test_accumulator_alignment() {
        let acc = Accumulator::new();
        let addr = &acc as *const _ as usize;
        // 64バイトアライメントを確認
        assert_eq!(addr % 64, 0);
    }

    #[test]
    fn test_dirty_piece_new() {
        let dp = DirtyPiece::new();
        assert_eq!(dp.pieces().len(), 0);
        assert_eq!(dp.hand_changes().len(), 0);
        assert!(!dp.king_moved[0]);
        assert!(!dp.king_moved[1]);
    }

    #[test]
    fn test_dirty_piece_push() {
        let mut dp = DirtyPiece::new();
        dp.push_piece(ChangedPiece {
            color: Color::Black,
            old_piece: Piece::NONE,
            old_sq: None,
            new_piece: Piece::NONE,
            new_sq: Some(Square::SQ_11),
        });
        assert_eq!(dp.pieces().len(), 1);

        dp.push_hand_change(HandChange {
            owner: Color::Black,
            piece_type: PieceType::Pawn,
            old_count: 1,
            new_count: 0,
        });
        assert_eq!(dp.hand_changes().len(), 1);
    }

    #[test]
    fn test_accumulator_stack_push_pop() {
        let mut stack = AccumulatorStack::new();
        assert_eq!(stack.current_index(), 0);

        stack.push(DirtyPiece::new());
        assert_eq!(stack.current_index(), 1);
        assert_eq!(stack.current().previous, Some(0));

        stack.push(DirtyPiece::new());
        assert_eq!(stack.current_index(), 2);
        assert_eq!(stack.current().previous, Some(1));

        stack.pop();
        assert_eq!(stack.current_index(), 1);

        stack.pop();
        assert_eq!(stack.current_index(), 0);
    }

    #[test]
    fn test_accumulator_stack_reset() {
        let mut stack = AccumulatorStack::new();
        stack.push(DirtyPiece::new());
        stack.push(DirtyPiece::new());
        stack.current_mut().accumulator.computed_accumulation = true;

        stack.reset();
        assert_eq!(stack.current_index(), 0);
        assert!(!stack.current().accumulator.computed_accumulation);
    }

    #[test]
    fn test_accumulator_stack_find_usable() {
        let mut stack = AccumulatorStack::new();

        // 最初のエントリを計算済みにする
        stack.current_mut().accumulator.computed_accumulation = true;

        // 2手進める（玉移動なし）
        stack.push(DirtyPiece::new());
        stack.push(DirtyPiece::new());

        // 祖先探索
        let result = stack.find_usable_accumulator();
        assert!(result.is_some());
        let (idx, depth) = result.unwrap();
        assert_eq!(idx, 0);
        assert_eq!(depth, 2);
    }

    #[test]
    fn test_accumulator_stack_find_usable_with_king_move() {
        let mut stack = AccumulatorStack::new();

        // 最初のエントリを計算済みにする
        stack.current_mut().accumulator.computed_accumulation = true;

        // 1手目で玉移動
        let mut dp = DirtyPiece::new();
        dp.king_moved[Color::Black.index()] = true;
        stack.push(dp);

        // 玉移動があるので祖先探索は失敗
        let result = stack.find_usable_accumulator();
        assert!(result.is_none());
    }
}
