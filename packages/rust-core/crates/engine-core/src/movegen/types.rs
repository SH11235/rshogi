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

    /// 長さを設定
    ///
    /// 直接配列に書き込んだ後にlenを更新するために使用。
    #[inline]
    pub fn set_len(&mut self, len: usize) {
        debug_assert!(len <= MAX_MOVES, "len out of bounds: {len} > {MAX_MOVES}");
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
    #[inline]
    pub fn get(&self, idx: usize) -> ExtMove {
        debug_assert!(idx < self.len, "index out of bounds: {idx} >= {}", self.len);
        // SAFETY: idx < len であれば初期化済み
        unsafe { self.buf[idx].assume_init() }
    }

    /// 指定インデックスに値を設定
    #[inline]
    pub fn set(&mut self, idx: usize, ext: ExtMove) {
        debug_assert!(idx < MAX_MOVES, "index out of bounds: {idx} >= {MAX_MOVES}");
        self.buf[idx].write(ext);
        if idx >= self.len {
            self.len = idx + 1;
        }
    }

    /// 指定インデックスのvalueを設定
    #[inline]
    pub fn set_value(&mut self, idx: usize, value: i32) {
        debug_assert!(idx < self.len, "index out of bounds: {idx} >= {}", self.len);
        // SAFETY: idx < len であれば初期化済み
        unsafe {
            let ptr = self.buf[idx].as_mut_ptr();
            (*ptr).value = value;
        }
    }

    /// 2つの要素を入れ替え
    #[inline]
    pub fn swap(&mut self, i: usize, j: usize) {
        debug_assert!(i < self.len && j < self.len, "index out of bounds");
        // SAFETY: i, j < len であれば初期化済み
        unsafe {
            let pi = self.buf.as_mut_ptr().add(i);
            let pj = self.buf.as_mut_ptr().add(j);
            std::ptr::swap(pi, pj);
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
    fn test_ext_move_new() {
        let mv = Move::NONE;
        let ext = ExtMove::new(mv, 100);
        assert_eq!(ext.mv, mv);
        assert_eq!(ext.value, 100);
    }

    #[test]
    fn test_ext_move_from_move() {
        let mv = Move::NONE;
        let ext: ExtMove = mv.into();
        assert_eq!(ext.mv, mv);
        assert_eq!(ext.value, 0);
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
