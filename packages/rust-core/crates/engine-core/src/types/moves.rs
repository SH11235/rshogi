//! 指し手（Move）

use super::{PieceType, Square};

/// 指し手（16bit）
///
/// bit 0-6:  移動先 (to)
/// bit 7-13: 移動元 (from) / 駒打ちの場合はPieceType
/// bit 14:   駒打ちフラグ
/// bit 15:   成りフラグ
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Move(u16);

impl Move {
    /// 無効な指し手
    pub const NONE: Move = Move(0);
    /// パス（null move）
    pub const NULL: Move = Move(0x0081);

    const TO_MASK: u16 = 0x007F; // bit 0-6
    const FROM_MASK: u16 = 0x3F80; // bit 7-13
    const FROM_SHIFT: u32 = 7;
    const DROP_FLAG: u16 = 0x4000; // bit 14
    const PROMOTE_FLAG: u16 = 0x8000; // bit 15

    /// 移動の指し手を生成
    #[inline]
    pub const fn new_move(from: Square, to: Square, promote: bool) -> Move {
        let mut m = (to.raw() as u16) | ((from.raw() as u16) << Self::FROM_SHIFT);
        if promote {
            m |= Self::PROMOTE_FLAG;
        }
        Move(m)
    }

    /// 駒打ちの指し手を生成
    #[inline]
    pub const fn new_drop(piece_type: PieceType, to: Square) -> Move {
        Move((to.raw() as u16) | ((piece_type as u16) << Self::FROM_SHIFT) | Self::DROP_FLAG)
    }

    /// 移動先を取得
    #[inline]
    pub const fn to(self) -> Square {
        // SAFETY: to は 0-80 の範囲（7bit）
        unsafe { Square::from_u8_unchecked((self.0 & Self::TO_MASK) as u8) }
    }

    /// 移動元を取得（駒打ちの場合は無効）
    #[inline]
    pub const fn from(self) -> Square {
        debug_assert!(!self.is_drop());
        // SAFETY: from は 0-80 の範囲（7bit）
        unsafe { Square::from_u8_unchecked(((self.0 & Self::FROM_MASK) >> Self::FROM_SHIFT) as u8) }
    }

    /// 打つ駒種を取得（駒打ちでない場合は無効）
    #[inline]
    pub const fn drop_piece_type(self) -> PieceType {
        debug_assert!(self.is_drop());
        // SAFETY: PieceType は 1-7 の範囲（手駒のみ）
        unsafe { std::mem::transmute(((self.0 & Self::FROM_MASK) >> Self::FROM_SHIFT) as u8) }
    }

    /// 駒打ちかどうか
    #[inline]
    pub const fn is_drop(self) -> bool {
        (self.0 & Self::DROP_FLAG) != 0
    }

    /// 成りかどうか
    #[inline]
    pub const fn is_promote(self) -> bool {
        (self.0 & Self::PROMOTE_FLAG) != 0
    }

    /// 無効な指し手かどうか
    #[inline]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// 有効な指し手かどうか
    #[inline]
    pub const fn is_some(self) -> bool {
        self.0 != 0
    }

    /// History用インデックス（0〜(81+7)*81-1）
    /// 盤上のマスは 81 個（0〜80）
    /// 「駒種の種類」が 7 個（歩〜飛）
    /// 「from or 打ち駒種」×「to」の組み合わせ総数は (81 + 7) * 81 個
    #[inline]
    pub const fn history_index(self) -> usize {
        if self.is_drop() {
            let piece_type_index = ((self.0 & Self::FROM_MASK) >> Self::FROM_SHIFT) as usize;
            let to = (self.0 & Self::TO_MASK) as usize;
            (81 + piece_type_index - 1) * 81 + to
        } else {
            let from = ((self.0 & Self::FROM_MASK) >> Self::FROM_SHIFT) as usize;
            let to = (self.0 & Self::TO_MASK) as usize;
            from * 81 + to
        }
    }

    /// 内部値を取得
    #[inline]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// u16からMoveを生成
    #[inline]
    pub const fn from_u16(value: u16) -> Move {
        Move(value)
    }

    /// u16からMoveを生成（範囲チェック付き）
    #[inline]
    pub const fn from_u16_checked(value: u16) -> Option<Move> {
        let to = value & Self::TO_MASK;
        let from = (value & Self::FROM_MASK) >> Self::FROM_SHIFT;
        if to >= Square::NUM as u16 {
            return None;
        }

        if (value & Self::DROP_FLAG) != 0 {
            let piece = (value & Self::FROM_MASK) >> Self::FROM_SHIFT;
            if piece == 0 || piece > PieceType::Gold as u16 {
                return None;
            }
        } else if from >= Square::NUM as u16 {
            return None;
        }

        Some(Move(value))
    }

    /// u16に変換
    #[inline]
    pub const fn to_u16(self) -> u16 {
        self.0
    }

    /// USI形式の文字列に変換
    pub fn to_usi(self) -> String {
        if self.is_none() {
            return "none".to_string();
        }
        if self.is_drop() {
            let pt_char = match self.drop_piece_type() {
                PieceType::Pawn => 'P',
                PieceType::Lance => 'L',
                PieceType::Knight => 'N',
                PieceType::Silver => 'S',
                PieceType::Gold => 'G',
                PieceType::Bishop => 'B',
                PieceType::Rook => 'R',
                _ => unreachable!(),
            };
            let to = self.to().to_usi();
            format!("{pt_char}*{to}")
        } else {
            let promote = if self.is_promote() { "+" } else { "" };
            let from = self.from().to_usi();
            let to = self.to().to_usi();
            format!("{from}{to}{promote}")
        }
    }

    /// USI形式の文字列からMoveに変換
    pub fn from_usi(s: &str) -> Option<Move> {
        if s == "none" {
            return Some(Move::NONE);
        }

        let chars: Vec<char> = s.chars().collect();
        if chars.len() < 4 {
            return None;
        }

        // 駒打ち判定（"P*7f" 形式）
        if chars.len() >= 4 && chars[1] == '*' {
            let pt = match chars[0] {
                'P' => PieceType::Pawn,
                'L' => PieceType::Lance,
                'N' => PieceType::Knight,
                'S' => PieceType::Silver,
                'G' => PieceType::Gold,
                'B' => PieceType::Bishop,
                'R' => PieceType::Rook,
                _ => return None,
            };
            let to_str: String = chars[2..4].iter().collect();
            let to = Square::from_usi(&to_str)?;
            return Some(Move::new_drop(pt, to));
        }

        // 通常の移動（"7g7f" または "7g7f+" 形式）
        if chars.len() >= 4 {
            let from_str: String = chars[0..2].iter().collect();
            let to_str: String = chars[2..4].iter().collect();
            let from = Square::from_usi(&from_str)?;
            let to = Square::from_usi(&to_str)?;
            let promote = chars.len() >= 5 && chars[4] == '+';
            return Some(Move::new_move(from, to, promote));
        }

        None
    }
}

impl Default for Move {
    fn default() -> Self {
        Move::NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_move_new_move() {
        let from = Square::new(File::File7, Rank::Rank7);
        let to = Square::new(File::File7, Rank::Rank6);
        let m = Move::new_move(from, to, false);

        assert!(!m.is_drop());
        assert!(!m.is_promote());
        assert_eq!(m.from(), from);
        assert_eq!(m.to(), to);
    }

    #[test]
    fn test_move_new_move_promote() {
        let from = Square::new(File::File2, Rank::Rank3);
        let to = Square::new(File::File2, Rank::Rank2);
        let m = Move::new_move(from, to, true);

        assert!(!m.is_drop());
        assert!(m.is_promote());
        assert_eq!(m.from(), from);
        assert_eq!(m.to(), to);
    }

    #[test]
    fn test_move_new_drop() {
        let to = Square::new(File::File5, Rank::Rank5);
        let m = Move::new_drop(PieceType::Pawn, to);

        assert!(m.is_drop());
        assert!(!m.is_promote());
        assert_eq!(m.drop_piece_type(), PieceType::Pawn);
        assert_eq!(m.to(), to);
    }

    #[test]
    fn test_move_encoding_matches_yaneuraou_spec() {
        // MOVE_NULL は (1 << 7) + 1
        assert_eq!(Move::NULL.raw(), 0x0081);

        // 通常手: to(60) | from(60 << 7)
        let m = Move::new_move(
            Square::new(File::File7, Rank::Rank7),
            Square::new(File::File7, Rank::Rank7),
            false,
        );
        assert_eq!(m.raw(), 0x1E3C);

        // 打ち: to(40) | piece_type(Pawn=1 << 7) | DROP_FLAG
        let drop = Move::new_drop(PieceType::Pawn, Square::SQ_55);
        assert_eq!(drop.raw(), 0x40A8);
    }

    #[test]
    fn test_move_from_u16_checked() {
        // valid move
        let m = Move::new_move(Square::SQ_11, Square::SQ_55, false);
        assert_eq!(Move::from_u16_checked(m.raw()), Some(m));

        // invalid square
        let invalid_square = (Square::NUM as u16) | ((Square::NUM as u16) << 7);
        assert_eq!(Move::from_u16_checked(invalid_square), None);

        // invalid drop piece type
        let raw = 0x4000 | (8 << 7) | Square::SQ_55.raw() as u16;
        assert_eq!(Move::from_u16_checked(raw), None);
    }

    #[test]
    fn test_move_none() {
        assert!(Move::NONE.is_none());
        assert!(!Move::NONE.is_some());
    }

    #[test]
    fn test_move_to_usi() {
        // 通常移動
        let from = Square::new(File::File7, Rank::Rank7);
        let to = Square::new(File::File7, Rank::Rank6);
        let m = Move::new_move(from, to, false);
        assert_eq!(m.to_usi(), "7g7f");

        // 成り
        let from = Square::new(File::File2, Rank::Rank3);
        let to = Square::new(File::File2, Rank::Rank2);
        let m = Move::new_move(from, to, true);
        assert_eq!(m.to_usi(), "2c2b+");

        // 駒打ち
        let to = Square::new(File::File5, Rank::Rank5);
        let m = Move::new_drop(PieceType::Gold, to);
        assert_eq!(m.to_usi(), "G*5e");

        // 無効な指し手
        assert_eq!(Move::NONE.to_usi(), "none");
    }

    #[test]
    fn test_move_from_usi() {
        // 通常移動
        let m = Move::from_usi("7g7f").unwrap();
        assert!(!m.is_drop());
        assert!(!m.is_promote());
        assert_eq!(m.from(), Square::new(File::File7, Rank::Rank7));
        assert_eq!(m.to(), Square::new(File::File7, Rank::Rank6));

        // 成り
        let m = Move::from_usi("2c2b+").unwrap();
        assert!(!m.is_drop());
        assert!(m.is_promote());

        // 駒打ち
        let m = Move::from_usi("G*5e").unwrap();
        assert!(m.is_drop());
        assert_eq!(m.drop_piece_type(), PieceType::Gold);
        assert_eq!(m.to(), Square::new(File::File5, Rank::Rank5));

        // 無効な指し手
        let m = Move::from_usi("none").unwrap();
        assert!(m.is_none());

        // 不正な文字列
        assert!(Move::from_usi("").is_none());
        assert!(Move::from_usi("abc").is_none());
    }

    #[test]
    fn test_move_history_index() {
        // 通常移動
        let from = Square::new(File::File7, Rank::Rank7);
        let to = Square::new(File::File7, Rank::Rank6);
        let m = Move::new_move(from, to, false);
        let idx = m.history_index();
        // File7 = 6 (0-indexed), Rank7 = 6, Rank6 = 5
        // from = 6*9+6 = 60, to = 6*9+5 = 59
        // index = 60 * 81 + 59
        assert_eq!(idx, 60 * 81 + 59);

        // 駒打ち（歩）
        let m = Move::new_drop(PieceType::Pawn, to);
        let idx = m.history_index();
        // pt = 1, to = 59
        // index = (81 + 1 - 1) * 81 + 59 = 81 * 81 + 59
        assert_eq!(idx, 81 * 81 + 59);
    }

    #[test]
    fn test_move_roundtrip() {
        // USI形式の往復変換テスト
        let test_cases = ["7g7f", "2c2b+", "P*5e", "G*1a", "none"];
        for s in test_cases {
            let m = Move::from_usi(s).unwrap();
            assert_eq!(m.to_usi(), s);
        }
    }
}
