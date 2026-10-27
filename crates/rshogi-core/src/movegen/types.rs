//! 合法手生成の型定義

use std::mem::MaybeUninit;

use crate::types::Move;

/// 1局面での最大合法手数
/// 理論上の最大は593手だが、余裕を持たせる
pub const MAX_MOVES: usize = 600;

/// 指し手生成のタイプ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenType {
    /// 駒を取らない指し手
    Quiets,
    /// 駒を取る指し手
    Captures,
    /// 駒を取らない指し手（不成含む）
    QuietsAll,
    /// 駒を取る指し手（不成含む）
    CapturesAll,
    /// 駒を取る指し手 + 歩の価値ある成り
    CapturesProPlus,
    /// 駒を取らない指し手 - 歩の敵陣成り
    QuietsProMinus,
    /// 駒を取る指し手 + 歩の価値ある成り（不成含む）
    CapturesProPlusAll,
    /// 駒を取らない指し手 - 歩の敵陣成り（不成含む）
    QuietsProMinusAll,
    /// 王手回避手
    Evasions,
    /// 王手回避手（不成含む）
    EvasionsAll,
    /// 王手がかかっていない全ての手
    NonEvasions,
    /// 王手がかかっていない全ての手（不成含む）
    NonEvasionsAll,
    /// 合法手すべて（is_legal()チェック付き）
    Legal,
    /// 合法手すべて（不成含む）
    LegalAll,
    /// 王手となる指し手
    Checks,
    /// 王手となる指し手（不成含む）
    ChecksAll,
    /// 駒を取らない王手
    QuietChecks,
    /// 駒を取らない王手（不成含む）
    QuietChecksAll,
    /// 指定升への再捕獲
    Recaptures,
    /// 指定升への再捕獲（不成含む）
    RecapturesAll,
}

impl GenType {
    /// 不成も含めて生成するタイプか
    #[inline]
    pub const fn includes_non_promotions(self) -> bool {
        matches!(
            self,
            Self::QuietsAll
                | Self::CapturesAll
                | Self::CapturesProPlusAll
                | Self::QuietsProMinusAll
                | Self::EvasionsAll
                | Self::NonEvasionsAll
                | Self::LegalAll
                | Self::ChecksAll
                | Self::QuietChecksAll
                | Self::RecapturesAll
        )
    }
}

/// 指し手とスコアのペア（オーダリング用）
#[derive(Debug, Clone, Copy)]
pub struct ExtMove {
    /// 指し手
    pub mv: Move,
    /// オーダリング用スコア
    pub value: i32,
}

impl ExtMove {
    /// 新しいExtMoveを作成
    #[inline]
    pub const fn new(mv: Move, value: i32) -> Self {
        Self { mv, value }
    }
}

impl From<Move> for ExtMove {
    #[inline]
    fn from(mv: Move) -> Self {
        Self { mv, value: 0 }
    }
}

impl PartialOrd for ExtMove {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ExtMove {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value.cmp(&other.value)
    }
}

impl PartialEq for ExtMove {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Eq for ExtMove {}

// =============================================================================
// ExtMoveBuffer（ゼロ初期化不要バッファ）
// =============================================================================

/// ExtMove用のゼロ初期化不要バッファ
///
/// MaybeUninitを使用して初期化コストを回避し、MovePickerのホットパスを高速化する。
pub struct ExtMoveBuffer {
    buf: [MaybeUninit<ExtMove>; MAX_MOVES],
    len: usize,
}

impl ExtMoveBuffer {
    /// 空のバッファを作成
    ///
    /// MaybeUninitにより初期化コストはゼロ。
    #[inline]
    pub fn new() -> Self {
        Self {
            // SAFETY: MaybeUninitの配列は未初期化のまま作成可能
            buf: unsafe { MaybeUninit::uninit().assume_init() },
            len: 0,
        }
    }

    /// 指し手を追加
    #[inline]
    pub fn push(&mut self, ext: ExtMove) {
        if self.len < MAX_MOVES {
            self.buf[self.len].write(ext);
            self.len += 1;
        } else {
            debug_assert!(
                false,
                "ExtMoveBuffer overflow: tried to add move beyond MAX_MOVES ({MAX_MOVES})"
            );
        }
    }

    /// Moveを追加（value=0で初期化）
    ///
    /// generate関数から直接ExtMoveBufferに書き込む際に使用。
    /// 中間バッファへのコピーを回避してパフォーマンスを向上。
    #[inline]
    pub fn push_move(&mut self, mv: Move) {
        if self.len < MAX_MOVES {
            self.buf[self.len].write(ExtMove { mv, value: 0 });
            self.len += 1;
        } else {
            debug_assert!(
                false,
                "ExtMoveBuffer overflow: tried to add move beyond MAX_MOVES ({})",
                MAX_MOVES
            );
        }
    }

    /// 現在の要素数
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// 空かどうか
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// バッファをクリア
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// 初期化済みの要素数を指定した長さまで短縮する。
    ///
    /// 現在の長さ以下を渡すこと。要素の追加には `push()` / `set()` を使う。
    // MovePicker で手ごとに通るため、release では範囲を検査しない。
    #[inline]
    pub fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.len, "cannot extend buffer length without initializing elements");
        self.len = len;
    }

    /// スライスとして取得
    #[inline]
    pub fn as_slice(&self) -> &[ExtMove] {
        // SAFETY: 0..len の範囲は全て push() または set() で初期化済み
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const ExtMove, self.len) }
    }

    /// 可変スライスとして取得
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [ExtMove] {
        // SAFETY: 0..len の範囲は全て push() または set() で初期化済み
        unsafe { std::slice::from_raw_parts_mut(self.buf.as_mut_ptr() as *mut ExtMove, self.len) }
    }

    /// 指定インデックスの要素を取得
    ///
    /// `idx < len()` を渡すこと。
    // MovePicker で手ごとに通るため、release では範囲を検査しない。
    #[inline]
    pub fn get(&self, idx: usize) -> ExtMove {
        debug_assert!(idx < self.len, "index out of bounds: {idx} >= {}", self.len);
        // SAFETY: idx < len であれば初期化済みで、buf の長さは MAX_MOVES >= len。
        unsafe { self.buf.get_unchecked(idx).assume_init() }
    }

    /// 指定インデックスを上書きする。`idx == len()` なら末尾に追加する。
    ///
    /// # Panics
    /// `idx > len()` または `idx >= MAX_MOVES` の場合は panic する。
    #[inline]
    pub fn set(&mut self, idx: usize, ext: ExtMove) {
        assert!(idx <= self.len, "cannot leave uninitialized elements before the index");
        self.buf[idx].write(ext);
        if idx >= self.len {
            self.len = idx + 1;
        }
    }

    /// 指定インデックスのvalueを設定
    ///
    /// # Panics
    /// `idx >= len()` の場合は panic する。
    #[inline]
    pub fn set_value(&mut self, idx: usize, value: i32) {
        self.as_mut_slice()[idx].value = value;
    }

    /// 2つの要素を入れ替え
    ///
    /// どちらのインデックスも `len()` 未満を渡すこと。
    // MovePicker で手ごとに通るため、release では範囲を検査しない。
    #[inline]
    pub fn swap(&mut self, i: usize, j: usize) {
        debug_assert!(i < self.len && j < self.len, "index out of bounds");
        // SAFETY: i, j < len であれば初期化済み。ptr::swap は i == j でも正しい。
        unsafe {
            let base = self.buf.as_mut_ptr();
            std::ptr::swap(base.add(i), base.add(j));
        }
    }

    /// イテレータを返す
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = ExtMove> + '_ {
        self.as_slice().iter().copied()
    }

    /// 条件を満たす要素のみ残す（in-place）
    ///
    /// 捕獲手のみフィルタする際などに使用。
    /// O(n)でフィルタリングを行う。
    ///
    /// # Safety guarantees
    ///
    /// この関数は内部でunsafeを使用するが、以下の不変条件により安全性が保証される:
    /// - `self.len`は常に初期化済み要素数を正確に追跡
    /// - `read_idx < self.len`の範囲のみアクセスするため、未初期化メモリは読み込まない
    /// - `write_idx <= read_idx`が常に成立するため、書き込み先は既に読み込み済みか同一位置
    /// - ExtMoveはCopyトレイトを実装しており、assume_init()後の再writeは安全
    #[inline]
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(Move) -> bool,
    {
        let mut write_idx = 0;
        for read_idx in 0..self.len {
            // SAFETY: read_idx < self.len であり、0..self.len の範囲は
            // push()/push_move()/set() により全て初期化済み。
            // したがって assume_init() は未初期化メモリにアクセスしない。
            let ext = unsafe { self.buf[read_idx].assume_init() };
            if f(ext.mv) {
                // SAFETY: write_idx <= read_idx が常に成立。
                // write_idx < read_idx の場合、write_idx位置は既に読み込み済みなので
                // 上書きしても問題ない。write_idx == read_idx の場合はスキップ。
                if write_idx != read_idx {
                    self.buf[write_idx].write(ext);
                }
                write_idx += 1;
            }
        }
        // write_idx は条件を満たした要素数。
        // 新しいlenより後ろの要素は論理的に無効となるが、
        // MaybeUninitなのでドロップ処理は不要。
        self.len = write_idx;
    }
}

impl Default for ExtMoveBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_preserves_initialized_prefix() {
        let mut buffer = ExtMoveBuffer::new();
        buffer.set(0, ExtMove::new(Move::NULL, 10));
        buffer.push_move(Move::PASS);
        buffer.set_value(1, 20);
        buffer.swap(0, 1);
        assert_eq!(buffer.get(0).mv, Move::PASS);
        assert_eq!(buffer.get(0).value, 20);
        assert_eq!(buffer.get(1).mv, Move::NULL);
        buffer.set_len(1);
        buffer.set(1, ExtMove::new(Move::WIN, 30));
        assert_eq!(buffer.len(), 2);
        buffer.retain(|mv| !mv.is_pass());
        assert_eq!(buffer.get(0).mv, Move::WIN);
        assert_eq!(buffer.get(0).value, 30);
        buffer.clear();
        assert!(buffer.as_slice().is_empty());
        buffer.push(ExtMove::new(Move::NONE, 40));
        assert_eq!(buffer.get(0).value, 40);
    }

    #[test]
    fn buffer_set_rejects_uninitialized_and_out_of_bounds_index() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let mut buffer = ExtMoveBuffer::new();
        assert!(catch_unwind(AssertUnwindSafe(|| buffer.set_value(0, 7))).is_err());
        assert!(buffer.is_empty());

        buffer.push(ExtMove::new(Move::NULL, 10));
        for index in [1, MAX_MOVES, usize::MAX] {
            assert!(catch_unwind(AssertUnwindSafe(|| buffer.set_value(index, 7))).is_err());
        }
        for index in [2, MAX_MOVES, usize::MAX] {
            assert!(
                catch_unwind(AssertUnwindSafe(|| buffer.set(index, ExtMove::new(Move::WIN, 20))))
                    .is_err()
            );
        }
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.get(0).mv, Move::NULL);
        assert_eq!(buffer.get(0).value, 10);
    }

    #[test]
    fn buffer_set_cannot_exceed_capacity() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let mut buffer = ExtMoveBuffer::new();
        for index in 0..MAX_MOVES {
            buffer.set(index, ExtMove::new(Move::NULL, index as i32));
        }
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                buffer.set(MAX_MOVES, ExtMove::new(Move::WIN, 0))
            }))
            .is_err()
        );
        assert_eq!(buffer.len(), MAX_MOVES);
        assert_eq!(buffer.get(MAX_MOVES - 1).value, (MAX_MOVES - 1) as i32);
    }

    #[test]
    fn test_ext_move_ordering() {
        let ext1 = ExtMove::new(Move::NONE, 100);
        let ext2 = ExtMove::new(Move::NONE, 200);
        let ext3 = ExtMove::new(Move::NONE, 100);

        assert!(ext1 < ext2);
        assert!(ext2 > ext1);
        assert_eq!(ext1, ext3);
    }
}
