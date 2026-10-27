//! 駒（Piece）
//!
//! 内部表現は YaneuraOu の `Piece` に準拠した 5bit ラッパー。
//! - bit 0-3: `PieceType`（1..=14）。0 は `Piece::NONE` のみで使用される。
//! - bit 4: `Color`（0 = Black, 1 = White）。
//!
//! `Piece::NONE` 以外の値は常に有効な `PieceType` / `Color` の組み合わせであることを前提とする。
//! `Piece::NONE` に対して駒種の取得・成り・生駒への変換を呼ばないのが契約 (探索の速度のため release では検査しない)。

use std::ops::{Index, IndexMut};

use super::{Color, PieceType};

/// 駒（先後の区別あり）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Piece(u8);

impl Piece {
    /// 駒なし
    pub const NONE: Piece = Piece(0);

    // 先手の駒
    pub const B_PAWN: Piece = Piece(1);
    pub const B_LANCE: Piece = Piece(2);
    pub const B_KNIGHT: Piece = Piece(3);
    pub const B_SILVER: Piece = Piece(4);
    pub const B_BISHOP: Piece = Piece(5);
    pub const B_ROOK: Piece = Piece(6);
    pub const B_GOLD: Piece = Piece(7);
    pub const B_KING: Piece = Piece(8);
    pub const B_PRO_PAWN: Piece = Piece(9);
    pub const B_PRO_LANCE: Piece = Piece(10);
    pub const B_PRO_KNIGHT: Piece = Piece(11);
    pub const B_PRO_SILVER: Piece = Piece(12);
    pub const B_HORSE: Piece = Piece(13);
    pub const B_DRAGON: Piece = Piece(14);

    // 後手の駒（+16）
    pub const W_PAWN: Piece = Piece(17);
    pub const W_LANCE: Piece = Piece(18);
    pub const W_KNIGHT: Piece = Piece(19);
    pub const W_SILVER: Piece = Piece(20);
    pub const W_BISHOP: Piece = Piece(21);
    pub const W_ROOK: Piece = Piece(22);
    pub const W_GOLD: Piece = Piece(23);
    pub const W_KING: Piece = Piece(24);
    pub const W_PRO_PAWN: Piece = Piece(25);
    pub const W_PRO_LANCE: Piece = Piece(26);
    pub const W_PRO_KNIGHT: Piece = Piece(27);
    pub const W_PRO_SILVER: Piece = Piece(28);
    pub const W_HORSE: Piece = Piece(29);
    pub const W_DRAGON: Piece = Piece(30);

    /// 駒の種類数（NONEを含む、配列サイズ用）
    pub const NUM: usize = 31;

    /// ColorとPieceTypeから生成（newのエイリアス）
    #[inline]
    pub const fn make(color: Color, piece_type: PieceType) -> Piece {
        Piece::new(color, piece_type)
    }

    /// ColorとPieceTypeから生成
    #[inline]
    pub const fn new(color: Color, piece_type: PieceType) -> Piece {
        Piece(piece_type as u8 | ((color as u8) << 4))
    }

    /// 駒種を取得
    ///
    /// `Piece::NONE` に対して呼ばないこと。
    // 探索の最内ループで通るため release では検査しない (検査を入れると探索が 2〜3% 遅くなる)。
    #[inline]
    pub const fn piece_type(self) -> PieceType {
        debug_assert!(!self.is_none(), "piece_type() called on Piece::NONE");
        // SAFETY: Piece は {0, 1..=14, 17..=30} で、NONE を除く下位4bitは 1..=14。NONE でないことは呼び出し側の前提。
        unsafe { std::mem::transmute(self.0 & 0x0F) }
    }

    /// 手番を取得
    #[inline]
    pub const fn color(self) -> Color {
        // SAFETY: (self.0 >> 4) & 1 は 0 or 1 なので有効なColor値
        unsafe { std::mem::transmute((self.0 >> 4) & 1) }
    }

    /// 駒がないか
    #[inline]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// 駒があるか
    #[inline]
    pub const fn is_some(self) -> bool {
        self.0 != 0
    }

    /// 成り駒を返す
    ///
    /// `Piece::NONE` に対して呼ばないこと。
    #[inline]
    pub const fn promote(self) -> Option<Piece> {
        match self.piece_type().promote() {
            Some(pt) => Some(Piece::new(self.color(), pt)),
            None => None,
        }
    }

    /// 生駒を返す
    ///
    /// `Piece::NONE` に対して呼ばないこと。
    #[inline]
    pub const fn unpromote(self) -> Piece {
        Piece::new(self.color(), self.piece_type().unpromote())
    }

    /// インデックス（0-30、0は無効）
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// 内部値を取得
    #[inline]
    pub const fn raw(self) -> u8 {
        self.0
    }

    /// 内部値からPieceを生成
    ///
    /// # Safety
    /// `value` は正規エンコードの Piece 値でなければならない:
    /// `{0, 1..=14, 17..=30}`。15 と 16 は未使用（PieceType/Color の
    /// ビットレイアウトに対応する有効な駒が存在しない）。
    /// Index trait および `piece_type()` の transmute の安全性がこの不変条件に依拠する。
    #[inline]
    pub const unsafe fn from_raw(value: u8) -> Piece {
        debug_assert!(value < Self::NUM as u8 && value != 15 && value != 16);
        Piece(value)
    }
}

impl Default for Piece {
    fn default() -> Self {
        Piece::NONE
    }
}

// Piece で添字にする配列は [T; Piece::NUM] (31 要素) 以上という前提で、境界検査を省く。
// 探索の最内ループで通るため release では検査しない (検査を入れると探索が 2〜3% 遅くなる)。
impl<T> Index<Piece> for [T] {
    type Output = T;

    #[inline]
    fn index(&self, pc: Piece) -> &T {
        debug_assert!(pc.index() < self.len());
        // SAFETY: Piece の値域は 0..=30。Piece で添字にする配列は [T; Piece::NUM] (31 要素) 以上。
        unsafe { self.get_unchecked(pc.index()) }
    }
}

impl<T> IndexMut<Piece> for [T] {
    #[inline]
    fn index_mut(&mut self, pc: Piece) -> &mut T {
        debug_assert!(pc.index() < self.len());
        // SAFETY: Piece の値域は 0..=30。Piece で添字にする配列は [T; Piece::NUM] (31 要素) 以上。
        unsafe { self.get_unchecked_mut(pc.index()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_piece_new() {
        assert_eq!(Piece::new(Color::Black, PieceType::Pawn), Piece::B_PAWN);
        assert_eq!(Piece::new(Color::White, PieceType::Pawn), Piece::W_PAWN);
        assert_eq!(Piece::new(Color::Black, PieceType::King), Piece::B_KING);
        assert_eq!(Piece::new(Color::White, PieceType::Dragon), Piece::W_DRAGON);
    }

    #[test]
    fn test_piece_type() {
        assert_eq!(Piece::B_PAWN.piece_type(), PieceType::Pawn);
        assert_eq!(Piece::W_PAWN.piece_type(), PieceType::Pawn);
        assert_eq!(Piece::B_DRAGON.piece_type(), PieceType::Dragon);
        assert_eq!(Piece::W_DRAGON.piece_type(), PieceType::Dragon);
    }

    #[test]
    fn test_piece_color() {
        assert_eq!(Piece::B_PAWN.color(), Color::Black);
        assert_eq!(Piece::W_PAWN.color(), Color::White);
        assert_eq!(Piece::B_KING.color(), Color::Black);
        assert_eq!(Piece::W_KING.color(), Color::White);
    }

    #[test]
    fn test_piece_is_none() {
        assert!(Piece::NONE.is_none());
        assert!(!Piece::B_PAWN.is_none());
        assert!(Piece::NONE.is_none());
        assert!(Piece::B_PAWN.is_some());
    }

    #[test]
    fn test_piece_promote() {
        assert_eq!(Piece::B_PAWN.promote(), Some(Piece::B_PRO_PAWN));
        assert_eq!(Piece::W_BISHOP.promote(), Some(Piece::W_HORSE));
        assert_eq!(Piece::B_GOLD.promote(), None);
        assert_eq!(Piece::W_KING.promote(), None);
    }

    #[test]
    fn test_piece_unpromote() {
        assert_eq!(Piece::B_PRO_PAWN.unpromote(), Piece::B_PAWN);
        assert_eq!(Piece::W_HORSE.unpromote(), Piece::W_BISHOP);
        assert_eq!(Piece::B_PAWN.unpromote(), Piece::B_PAWN);
    }

    #[test]
    fn test_piece_index() {
        assert_eq!(Piece::NONE.index(), 0);
        assert_eq!(Piece::B_PAWN.index(), 1);
        assert_eq!(Piece::W_PAWN.index(), 17);
        assert_eq!(Piece::W_DRAGON.index(), 30);
    }
}
