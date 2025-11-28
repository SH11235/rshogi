//! 手駒（Hand）

use super::PieceType;

/// 手駒（32bit packed）
///
/// ビット配置:
/// - bit 0-4:   歩 (5bit, 最大18枚)
/// - bit 5-7:   香 (3bit, 最大4枚)
/// - bit 8-10:  桂 (3bit, 最大4枚)
/// - bit 11-13: 銀 (3bit, 最大4枚)
/// - bit 14-16: 金 (3bit, 最大4枚)
/// - bit 17-18: 角 (2bit, 最大2枚)
/// - bit 19-20: 飛 (2bit, 最大2枚)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Hand(u32);

impl Hand {
    /// 空の手駒
    pub const EMPTY: Hand = Hand(0);

    // ビットシフト・マスク定数
    const PAWN_SHIFT: u32 = 0;
    const PAWN_MASK: u32 = 0x1F; // 5bit (最大18枚)
    const LANCE_SHIFT: u32 = 5;
    const LANCE_MASK: u32 = 0x07; // 3bit (最大4枚)
    const KNIGHT_SHIFT: u32 = 8;
    const KNIGHT_MASK: u32 = 0x07;
    const SILVER_SHIFT: u32 = 11;
    const SILVER_MASK: u32 = 0x07;
    const GOLD_SHIFT: u32 = 14;
    const GOLD_MASK: u32 = 0x07;
    const BISHOP_SHIFT: u32 = 17;
    const BISHOP_MASK: u32 = 0x03; // 2bit (最大2枚)
    const ROOK_SHIFT: u32 = 19;
    const ROOK_MASK: u32 = 0x03;

    /// 指定駒種の枚数を取得
    #[inline]
    pub const fn count(self, pt: PieceType) -> u32 {
        let (shift, mask) = Self::shift_mask(pt);
        (self.0 >> shift) & mask
    }

    /// 指定駒種を持っているか
    #[inline]
    pub const fn has(self, pt: PieceType) -> bool {
        self.count(pt) > 0
    }

    /// 1枚追加
    #[inline]
    pub const fn add(self, pt: PieceType) -> Hand {
        let (shift, _) = Self::shift_mask(pt);
        Hand(self.0 + (1 << shift))
    }

    /// 1枚減らす
    #[inline]
    pub const fn sub(self, pt: PieceType) -> Hand {
        debug_assert!(self.has(pt));
        let (shift, _) = Self::shift_mask(pt);
        Hand(self.0 - (1 << shift))
    }

    /// 指定枚数をセット
    #[inline]
    pub const fn set(self, pt: PieceType, count: u32) -> Hand {
        let (shift, mask) = Self::shift_mask(pt);
        Hand((self.0 & !(mask << shift)) | ((count & mask) << shift))
    }

    /// 優等局面判定: self >= other（全ての駒種で自分以上）
    #[inline]
    pub const fn is_superior_or_equal(self, other: Hand) -> bool {
        self.count(PieceType::Pawn) >= other.count(PieceType::Pawn)
            && self.count(PieceType::Lance) >= other.count(PieceType::Lance)
            && self.count(PieceType::Knight) >= other.count(PieceType::Knight)
            && self.count(PieceType::Silver) >= other.count(PieceType::Silver)
            && self.count(PieceType::Gold) >= other.count(PieceType::Gold)
            && self.count(PieceType::Bishop) >= other.count(PieceType::Bishop)
            && self.count(PieceType::Rook) >= other.count(PieceType::Rook)
    }

    /// 空かどうか
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// 内部値を取得
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// 内部値から生成
    #[inline]
    pub const fn from_raw(raw: u32) -> Hand {
        Hand(raw)
    }

    const fn shift_mask(pt: PieceType) -> (u32, u32) {
        match pt {
            PieceType::Pawn => (Self::PAWN_SHIFT, Self::PAWN_MASK),
            PieceType::Lance => (Self::LANCE_SHIFT, Self::LANCE_MASK),
            PieceType::Knight => (Self::KNIGHT_SHIFT, Self::KNIGHT_MASK),
            PieceType::Silver => (Self::SILVER_SHIFT, Self::SILVER_MASK),
            PieceType::Gold => (Self::GOLD_SHIFT, Self::GOLD_MASK),
            PieceType::Bishop => (Self::BISHOP_SHIFT, Self::BISHOP_MASK),
            PieceType::Rook => (Self::ROOK_SHIFT, Self::ROOK_MASK),
            _ => (0, 0), // King, 成駒は手駒にならない
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hand_empty() {
        let hand = Hand::EMPTY;
        assert!(hand.is_empty());
        assert_eq!(hand.count(PieceType::Pawn), 0);
        assert!(!hand.has(PieceType::Pawn));
    }

    #[test]
    fn test_hand_add() {
        let hand = Hand::EMPTY;
        let hand = hand.add(PieceType::Pawn);
        assert_eq!(hand.count(PieceType::Pawn), 1);
        assert!(hand.has(PieceType::Pawn));

        let hand = hand.add(PieceType::Pawn);
        assert_eq!(hand.count(PieceType::Pawn), 2);
    }

    #[test]
    fn test_hand_sub() {
        let hand = Hand::EMPTY.add(PieceType::Rook).add(PieceType::Rook);
        assert_eq!(hand.count(PieceType::Rook), 2);

        let hand = hand.sub(PieceType::Rook);
        assert_eq!(hand.count(PieceType::Rook), 1);

        let hand = hand.sub(PieceType::Rook);
        assert_eq!(hand.count(PieceType::Rook), 0);
        assert!(!hand.has(PieceType::Rook));
    }

    #[test]
    fn test_hand_set() {
        let hand = Hand::EMPTY.set(PieceType::Pawn, 5);
        assert_eq!(hand.count(PieceType::Pawn), 5);

        let hand = hand.set(PieceType::Gold, 3);
        assert_eq!(hand.count(PieceType::Pawn), 5);
        assert_eq!(hand.count(PieceType::Gold), 3);
    }

    #[test]
    fn test_hand_multiple_pieces() {
        let hand = Hand::EMPTY
            .add(PieceType::Pawn)
            .add(PieceType::Pawn)
            .add(PieceType::Lance)
            .add(PieceType::Bishop)
            .add(PieceType::Rook);

        assert_eq!(hand.count(PieceType::Pawn), 2);
        assert_eq!(hand.count(PieceType::Lance), 1);
        assert_eq!(hand.count(PieceType::Knight), 0);
        assert_eq!(hand.count(PieceType::Silver), 0);
        assert_eq!(hand.count(PieceType::Gold), 0);
        assert_eq!(hand.count(PieceType::Bishop), 1);
        assert_eq!(hand.count(PieceType::Rook), 1);
    }

    #[test]
    fn test_hand_is_superior_or_equal() {
        let hand1 = Hand::EMPTY.add(PieceType::Pawn).add(PieceType::Pawn);
        let hand2 = Hand::EMPTY.add(PieceType::Pawn);

        assert!(hand1.is_superior_or_equal(hand2));
        assert!(!hand2.is_superior_or_equal(hand1));

        // 等しい場合
        assert!(hand1.is_superior_or_equal(hand1));

        // 異なる駒種を比較
        let hand3 = Hand::EMPTY.add(PieceType::Rook);
        assert!(!hand1.is_superior_or_equal(hand3));
        assert!(!hand3.is_superior_or_equal(hand1));
    }

    #[test]
    fn test_hand_max_values() {
        // 歩の最大値（18枚）
        let mut hand = Hand::EMPTY;
        for _ in 0..18 {
            hand = hand.add(PieceType::Pawn);
        }
        assert_eq!(hand.count(PieceType::Pawn), 18);

        // 飛車の最大値（2枚）
        let hand = Hand::EMPTY.add(PieceType::Rook).add(PieceType::Rook);
        assert_eq!(hand.count(PieceType::Rook), 2);
    }
}
