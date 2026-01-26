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

/// 駒種の集合（やねうら王の合成駒種に対応するビットマスク）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PieceTypeSet(u16);

impl PieceTypeSet {
    const ALL_MASK: u16 = (1u16 << PieceType::NUM) - 1;

    #[inline]
    const fn bit(pt: PieceType) -> u16 {
        1u16 << (pt as u16 - 1)
    }

    /// 空集合
    pub const EMPTY: Self = Self(0);

    /// 全ての実駒種
    pub const ALL: Self = Self(Self::ALL_MASK);

    /// 金相当の駒（GOLDS）
    pub const fn golds() -> Self {
        Self::from_bits(
            Self::bit(PieceType::Gold)
                | Self::bit(PieceType::ProPawn)
                | Self::bit(PieceType::ProLance)
                | Self::bit(PieceType::ProKnight)
                | Self::bit(PieceType::ProSilver),
        )
    }

    /// 馬・龍・玉（HDK）
    pub const fn hdk() -> Self {
        Self::from_bits(
            Self::bit(PieceType::Horse) | Self::bit(PieceType::Dragon) | Self::bit(PieceType::King),
        )
    }

    /// 角・馬（BISHOP_HORSE）
    pub const fn bishop_horse() -> Self {
        Self::from_bits(Self::bit(PieceType::Bishop) | Self::bit(PieceType::Horse))
    }

    /// 飛・龍（ROOK_DRAGON）
    pub const fn rook_dragon() -> Self {
        Self::from_bits(Self::bit(PieceType::Rook) | Self::bit(PieceType::Dragon))
    }

    /// 銀 + HDK（SILVER_HDK）
    pub const fn silver_hdk() -> Self {
        Self::from_bits(Self::bit(PieceType::Silver) | Self::hdk().0)
    }

    /// GOLDS + HDK（GOLDS_HDK）
    pub const fn golds_hdk() -> Self {
        Self::from_bits(Self::golds().0 | Self::hdk().0)
    }

    /// 内部ビット列から生成（不要ビットはマスク）
    #[inline]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & Self::ALL_MASK)
    }

    /// 単一駒種から生成
    #[inline]
    pub const fn from_piece(pt: PieceType) -> Self {
        Self::from_bits(Self::bit(pt))
    }

    /// ビット列を取得
    #[inline]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// 空かどうか
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// 全駒種かどうか（実駒のみ）
    #[inline]
    pub const fn is_all(self) -> bool {
        self.0 == Self::ALL_MASK
    }

    /// 駒種を含むか
    #[inline]
    pub const fn contains(self, pt: PieceType) -> bool {
        (self.0 & Self::bit(pt)) != 0
    }

    /// イテレータを返す（下位ビット優先）
    pub fn iter(self) -> PieceTypeSetIter {
        PieceTypeSetIter { bits: self.0 }
    }
}

impl From<PieceType> for PieceTypeSet {
    #[inline]
    fn from(value: PieceType) -> Self {
        Self::from_piece(value)
    }
}

impl std::ops::BitOr for PieceTypeSet {
    type Output = PieceTypeSet;

    #[inline]
    fn bitor(self, rhs: PieceTypeSet) -> PieceTypeSet {
        PieceTypeSet(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for PieceTypeSet {
    #[inline]
    fn bitor_assign(&mut self, rhs: PieceTypeSet) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitOr<PieceType> for PieceTypeSet {
    type Output = PieceTypeSet;

    #[inline]
    fn bitor(self, rhs: PieceType) -> PieceTypeSet {
        self | PieceTypeSet::from(rhs)
    }
}

impl std::ops::BitOr<PieceTypeSet> for PieceType {
    type Output = PieceTypeSet;

    #[inline]
    fn bitor(self, rhs: PieceTypeSet) -> PieceTypeSet {
        PieceTypeSet::from(self) | rhs
    }
}

impl std::ops::BitAnd for PieceTypeSet {
    type Output = PieceTypeSet;

    #[inline]
    fn bitand(self, rhs: PieceTypeSet) -> PieceTypeSet {
        PieceTypeSet(self.0 & rhs.0)
    }
}

impl std::ops::BitAnd<PieceType> for PieceTypeSet {
    type Output = PieceTypeSet;

    #[inline]
    fn bitand(self, rhs: PieceType) -> PieceTypeSet {
        PieceTypeSet(self.0 & PieceTypeSet::bit(rhs))
    }
}

/// PieceTypeSet用のイテレータ
pub struct PieceTypeSetIter {
    bits: u16,
}

impl Iterator for PieceTypeSetIter {
    type Item = PieceType;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.bits == 0 {
            return None;
        }

        let lsb = self.bits.trailing_zeros() as u8;
        self.bits &= self.bits - 1;
        // trailing_zerosは0-basedで、PieceTypeは1始まり
        PieceType::from_u8(lsb + 1)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.bits.count_ones() as usize;
        (count, Some(count))
    }
}

impl ExactSizeIterator for PieceTypeSetIter {}

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

    #[test]
    fn test_piece_type_set_basic() {
        let set = PieceTypeSet::from_piece(PieceType::Gold) | PieceType::Silver;
        assert!(set.contains(PieceType::Gold));
        assert!(set.contains(PieceType::Silver));
        assert!(!set.contains(PieceType::Pawn));

        let mut iterated: Vec<_> = set.iter().collect();
        iterated.sort_by_key(|pt| *pt as u8);
        assert_eq!(iterated, vec![PieceType::Silver, PieceType::Gold]);
    }

    #[test]
    fn test_piece_type_set_presets() {
        let golds = PieceTypeSet::golds();
        assert!(golds.contains(PieceType::Gold));
        assert!(golds.contains(PieceType::ProPawn));
        assert!(golds.contains(PieceType::ProSilver));
        assert!(!golds.contains(PieceType::King));

        let hdk = PieceTypeSet::hdk();
        assert!(hdk.contains(PieceType::King));
        assert!(hdk.contains(PieceType::Dragon));
        assert!(!hdk.contains(PieceType::Gold));

        let combo = PieceTypeSet::golds_hdk();
        assert!(combo.contains(PieceType::Gold));
        assert!(combo.contains(PieceType::King));
    }
}
