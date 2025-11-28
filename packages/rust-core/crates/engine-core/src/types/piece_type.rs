//! 駒種（PieceType）

/// 駒種（先後の区別なし）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PieceType {
    // 生駒
    Pawn = 1,
    Lance = 2,
    Knight = 3,
    Silver = 4,
    Bishop = 5,
    Rook = 6,
    Gold = 7,
    King = 8,
    // 成駒
    ProPawn = 9,
    ProLance = 10,
    ProKnight = 11,
    ProSilver = 12,
    Horse = 13,  // 成角
    Dragon = 14, // 成飛
}

impl PieceType {
    /// 有効な駒種の数（1-14）
    pub const NUM: usize = 14;

    /// 手駒になる駒種の数
    pub const HAND_NUM: usize = 7;

    /// 手駒になる駒種一覧
    pub const HAND_PIECES: [PieceType; 7] = [
        PieceType::Pawn,
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Rook,
    ];

    /// 成れるかどうか
    #[inline]
    pub const fn can_promote(self) -> bool {
        matches!(
            self,
            PieceType::Pawn
                | PieceType::Lance
                | PieceType::Knight
                | PieceType::Silver
                | PieceType::Bishop
                | PieceType::Rook
        )
    }

    /// 成り駒を返す（成れない場合はNone）
    #[inline]
    pub const fn promote(self) -> Option<PieceType> {
        match self {
            PieceType::Pawn => Some(PieceType::ProPawn),
            PieceType::Lance => Some(PieceType::ProLance),
            PieceType::Knight => Some(PieceType::ProKnight),
            PieceType::Silver => Some(PieceType::ProSilver),
            PieceType::Bishop => Some(PieceType::Horse),
            PieceType::Rook => Some(PieceType::Dragon),
            _ => None,
        }
    }

    /// 生駒を返す（既に生駒の場合はそのまま）
    #[inline]
    pub const fn unpromote(self) -> PieceType {
        match self {
            PieceType::ProPawn => PieceType::Pawn,
            PieceType::ProLance => PieceType::Lance,
            PieceType::ProKnight => PieceType::Knight,
            PieceType::ProSilver => PieceType::Silver,
            PieceType::Horse => PieceType::Bishop,
            PieceType::Dragon => PieceType::Rook,
            _ => self,
        }
    }

    /// 成駒かどうか
    #[inline]
    pub const fn is_promoted(self) -> bool {
        self as u8 >= 9
    }

    /// 遠方駒（香角飛馬龍）かどうか
    #[inline]
    pub const fn is_slider(self) -> bool {
        matches!(
            self,
            PieceType::Lance
                | PieceType::Bishop
                | PieceType::Rook
                | PieceType::Horse
                | PieceType::Dragon
        )
    }

    /// インデックス（1-14）
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// u8から変換（範囲チェックあり）
    #[inline]
    pub const fn from_u8(n: u8) -> Option<PieceType> {
        if n >= 1 && n <= 14 {
            // SAFETY: 1 <= n <= 14 なので有効なPieceType値
            Some(unsafe { std::mem::transmute::<u8, PieceType>(n) })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_piece_type_promote() {
        assert_eq!(PieceType::Pawn.promote(), Some(PieceType::ProPawn));
        assert_eq!(PieceType::Bishop.promote(), Some(PieceType::Horse));
        assert_eq!(PieceType::Rook.promote(), Some(PieceType::Dragon));
        assert_eq!(PieceType::Gold.promote(), None);
        assert_eq!(PieceType::King.promote(), None);
        assert_eq!(PieceType::ProPawn.promote(), None);
    }

    #[test]
    fn test_piece_type_unpromote() {
        assert_eq!(PieceType::ProPawn.unpromote(), PieceType::Pawn);
        assert_eq!(PieceType::Horse.unpromote(), PieceType::Bishop);
        assert_eq!(PieceType::Dragon.unpromote(), PieceType::Rook);
        assert_eq!(PieceType::Pawn.unpromote(), PieceType::Pawn);
        assert_eq!(PieceType::Gold.unpromote(), PieceType::Gold);
    }

    #[test]
    fn test_piece_type_is_promoted() {
        assert!(!PieceType::Pawn.is_promoted());
        assert!(!PieceType::King.is_promoted());
        assert!(PieceType::ProPawn.is_promoted());
        assert!(PieceType::Horse.is_promoted());
        assert!(PieceType::Dragon.is_promoted());
    }

    #[test]
    fn test_piece_type_is_slider() {
        assert!(!PieceType::Pawn.is_slider());
        assert!(PieceType::Lance.is_slider());
        assert!(PieceType::Bishop.is_slider());
        assert!(PieceType::Rook.is_slider());
        assert!(PieceType::Horse.is_slider());
        assert!(PieceType::Dragon.is_slider());
        assert!(!PieceType::Gold.is_slider());
    }

    #[test]
    fn test_piece_type_can_promote() {
        assert!(PieceType::Pawn.can_promote());
        assert!(PieceType::Lance.can_promote());
        assert!(PieceType::Silver.can_promote());
        assert!(PieceType::Bishop.can_promote());
        assert!(PieceType::Rook.can_promote());
        assert!(!PieceType::Gold.can_promote());
        assert!(!PieceType::King.can_promote());
        assert!(!PieceType::ProPawn.can_promote());
    }

    #[test]
    fn test_piece_type_from_u8() {
        assert_eq!(PieceType::from_u8(0), None);
        assert_eq!(PieceType::from_u8(1), Some(PieceType::Pawn));
        assert_eq!(PieceType::from_u8(14), Some(PieceType::Dragon));
        assert_eq!(PieceType::from_u8(15), None);
    }
}
