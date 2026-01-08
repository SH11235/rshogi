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
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};

// =============================================================================
// IndexList - 固定長の特徴量インデックスリスト
// =============================================================================

/// 差分更新での最大変化特徴量数
/// HalfKP: 駒3 + 手駒2 = 5
/// HalfKA_hm^（coalesced）: 各変化=最大5
/// 余裕を持たせて16
pub const MAX_CHANGED_FEATURES: usize = 16;

/// IndexListの容量（全特徴量取得用）
///
/// アーキテクチャごとの理論上限:
/// - HalfKP: 盤上38 + 手駒14 = 52
/// - HalfKA_hm^（coalesced）: 盤上38 + 自玉1 + 敵玉1 + 手駒14 = 54
///
/// この値は`Feature::MAX_ACTIVE`（合法局面での最大値）より大きく設定し、
/// テスト用の非合法局面にも安全に対応できるマージンを持たせている。
///
/// 注意: 合法局面では Feature::MAX_ACTIVE を超えることはないが、
/// SFEN入力で非合法局面が来た場合にもパニックしないよう、余裕を持たせている。
pub const MAX_ACTIVE_FEATURES: usize = 54;

/// collect_path での最大パス長（find_usable_accumulator の MAX_DEPTH と同じ）
pub const MAX_PATH_LENGTH: usize = 8;

/// 固定長の特徴量インデックスリスト
///
/// Vec の代わりにスタック上の固定長配列を使用し、ヒープ割り当てを回避する。
/// MaybeUninit を使用して初期化コストをゼロにする。
///
/// # 制約
/// N は 255 以下である必要がある（len が u8 のため）。
/// この制約はコンパイル時にチェックされる。
#[derive(Clone, Copy)]
pub struct IndexList<const N: usize> {
    /// 未初期化領域を許容する配列
    indices: [MaybeUninit<usize>; N],
    /// 有効な要素数
    len: u8,
}

impl<const N: usize> IndexList<N> {
    /// N <= 255 をコンパイル時に保証
    /// N > 255 の場合、このアサートがコンパイルエラーを発生させる
    const _ASSERT_N_FITS_U8: () = assert!(N <= u8::MAX as usize, "IndexList: N must be <= 255");

    /// 空のリストを作成（初期化コストゼロ）
    #[inline]
    #[allow(path_statements)]
    pub fn new() -> Self {
        // N <= 255 のコンパイル時チェックを強制評価
        // これにより IndexList::<300>::new() などはコンパイルエラーになる
        // path_statements警告を許可するのは意図的な強制評価のため
        Self::_ASSERT_N_FITS_U8;
        Self {
            // インラインconst ブロックで未初期化配列を安全に作成
            indices: [const { MaybeUninit::uninit() }; N],
            len: 0,
        }
    }

    /// 要素を追加
    ///
    /// 容量（N）を超える場合は追加を無視する（安全のため）。
    /// 戻り値: 追加に成功した場合は true、容量オーバーで無視した場合は false
    #[inline]
    #[must_use]
    pub fn push(&mut self, index: usize) -> bool {
        let pos = self.len as usize;
        if pos >= N {
            debug_assert!(false, "IndexList overflow: capacity={N}, len={pos}");
            return false;
        }
        // SAFETY: pos < N なので範囲内。MaybeUninit への書き込みは常に安全
        self.indices[pos].write(index);
        self.len += 1;
        true
    }

    /// イテレータを返す
    #[inline]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &usize> + '_ {
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

// =============================================================================
// AlignedBox - 64バイトアラインメントのヒープ確保スライス
// =============================================================================

/// キャッシュラインサイズ（64バイト）
pub const CACHE_LINE_SIZE: usize = 64;

/// 64バイトアラインメントでヒープに確保されたスライス
///
/// FeatureTransformerのweightsなど、大きな配列をアラインして確保するために使用。
/// aligned load/store命令を使うためにはデータが64バイト境界に配置されている必要がある。
///
/// # 安全性契約
///
/// - `T: Copy + Default` を要求することで、`T` が `Drop` を実装できないことを保証
/// - `Copy` トレイトは `Drop` と排他的（コンパイラが禁止）
/// - これにより `drop` 時に `drop_in_place` を呼ぶ必要がなく、`dealloc` のみで安全
///
/// # 使用例
///
/// ```ignore
/// let weights: AlignedBox<i16> = AlignedBox::new_zeroed(1000);
/// assert!(weights.as_ptr() as usize % 64 == 0); // 64バイトアライン
/// ```
pub struct AlignedBox<T> {
    ptr: *mut T,
    len: usize,
    layout: Layout,
}

impl<T: Copy + Default> AlignedBox<T> {
    /// 指定された長さの配列をゼロ初期化して確保
    ///
    /// # Panics
    /// - `len * size_of::<T>()` がオーバーフローする場合
    /// - レイアウトが無効な場合
    /// - メモリ確保に失敗した場合
    pub fn new_zeroed(len: usize) -> Self {
        let size = std::mem::size_of::<T>()
            .checked_mul(len)
            .expect("AlignedBox::new_zeroed: size overflow");
        let align = CACHE_LINE_SIZE.max(std::mem::align_of::<T>());

        // SAFETY: align は 2 のべき乗で、size は align の倍数に切り上げられる
        let layout = Layout::from_size_align(size, align).expect("Invalid layout").pad_to_align();

        // SAFETY: layout は有効、alloc_zeroed は失敗時に null を返す
        let ptr = unsafe { alloc_zeroed(layout) as *mut T };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        Self { ptr, len, layout }
    }
}

impl<T> Deref for AlignedBox<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        // SAFETY: ptr は有効で、len 要素分のメモリが確保されている
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl<T> DerefMut for AlignedBox<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: ptr は有効で、len 要素分のメモリが確保されている
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl<T> Drop for AlignedBox<T> {
    fn drop(&mut self) {
        // SAFETY:
        // - ptr は alloc_zeroed で確保したポインタ、layout は同じもの
        // - AlignedBox::new_zeroed は T: Copy + Default を要求する
        // - Copy トレイトは Drop と排他的なので、T は Drop を実装できない
        // - したがって drop_in_place は不要で、dealloc のみで安全
        unsafe {
            dealloc(self.ptr as *mut u8, self.layout);
        }
    }
}

// SAFETY: T が Send なら AlignedBox<T> も Send
unsafe impl<T: Send> Send for AlignedBox<T> {}
// SAFETY: T が Sync なら AlignedBox<T> も Sync
unsafe impl<T: Sync> Sync for AlignedBox<T> {}

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
    ///
    /// 容量を超える場合は追加を無視する（安全のため）。
    /// 戻り値: 追加に成功した場合は true、容量オーバーで無視した場合は false
    #[inline]
    #[must_use]
    pub fn push_piece(&mut self, piece: ChangedPiece) -> bool {
        let idx = self.pieces_len as usize;
        if idx >= Self::MAX_PIECES {
            debug_assert!(false, "DirtyPiece::push_piece overflow: idx={idx}");
            return false;
        }
        self.pieces[idx] = piece;
        self.pieces_len += 1;
        true
    }

    /// 手駒変化を追加
    ///
    /// 容量を超える場合は追加を無視する（安全のため）。
    /// 戻り値: 追加に成功した場合は true、容量オーバーで無視した場合は false
    #[inline]
    #[must_use]
    pub fn push_hand_change(&mut self, change: HandChange) -> bool {
        let idx = self.hand_changes_len as usize;
        if idx >= Self::MAX_HAND_CHANGES {
            debug_assert!(false, "DirtyPiece::push_hand_change overflow: idx={idx}");
            return false;
        }
        self.hand_changes[idx] = change;
        self.hand_changes_len += 1;
        true
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

    /// 前回と現在のアキュムレータを同時に取得（clone不要）
    ///
    /// `split_at_mut`を使用して、prev_idx の accumulator への不変参照と
    /// 現在の accumulator への可変参照を同時に返す。
    ///
    /// # Safety
    /// prev_idx < current_idx であることが前提（常に成り立つはず）
    #[inline]
    pub fn get_prev_and_current_accumulators(
        &mut self,
        prev_idx: usize,
    ) -> (&Accumulator, &mut Accumulator) {
        let cur_idx = self.current_idx;
        debug_assert!(prev_idx < cur_idx, "prev_idx ({prev_idx}) must be < cur_idx ({cur_idx})");
        let (left, right) = self.entries.split_at_mut(cur_idx);
        (&left[prev_idx].accumulator, &mut right[0].accumulator)
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
    /// 戻り値:
    /// - Some(path): source_idx に到達できた場合、source側から適用する順のインデックス列
    /// - None: パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
    pub fn collect_path(&self, source_idx: usize) -> Option<IndexList<MAX_PATH_LENGTH>> {
        let mut path = IndexList::new();
        let mut idx = self.current_idx;

        while idx != source_idx {
            // パス長が上限を超えたら失敗
            if !path.push(idx) {
                debug_assert!(false, "collect_path overflow: MAX_PATH_LENGTH={MAX_PATH_LENGTH}");
                return None;
            }
            let entry = &self.entries[idx];
            match entry.previous {
                Some(prev_idx) => idx = prev_idx,
                None => {
                    debug_assert!(
                        false,
                        "Path broken: expected to reach source_idx={source_idx} but got None at idx={idx}"
                    );
                    return None;
                }
            }
        }

        path.reverse();
        Some(path)
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
        let _ = dp.push_piece(ChangedPiece {
            color: Color::Black,
            old_piece: Piece::NONE,
            old_sq: None,
            new_piece: Piece::NONE,
            new_sq: Some(Square::SQ_11),
        });
        assert_eq!(dp.pieces().len(), 1);

        let _ = dp.push_hand_change(HandChange {
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
