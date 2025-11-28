//! 千日手状態（RepetitionState）

/// 千日手状態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RepetitionState {
    /// 千日手ではない
    #[default]
    None,
    /// 通常の千日手（引き分け）
    Draw,
    /// 連続王手の千日手で勝ち
    Win,
    /// 連続王手の千日手で負け
    Lose,
    /// 優等局面
    Superior,
    /// 劣等局面
    Inferior,
}

impl RepetitionState {
    /// 千日手かどうか（通常の千日手または連続王手）
    #[inline]
    pub const fn is_repetition(self) -> bool {
        matches!(self, RepetitionState::Draw | RepetitionState::Win | RepetitionState::Lose)
    }

    /// 勝敗が決まる千日手かどうか
    #[inline]
    pub const fn is_decisive(self) -> bool {
        matches!(self, RepetitionState::Win | RepetitionState::Lose)
    }

    /// 優等/劣等局面かどうか
    #[inline]
    pub const fn is_superior_inferior(self) -> bool {
        matches!(self, RepetitionState::Superior | RepetitionState::Inferior)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repetition_state_is_repetition() {
        assert!(!RepetitionState::None.is_repetition());
        assert!(RepetitionState::Draw.is_repetition());
        assert!(RepetitionState::Win.is_repetition());
        assert!(RepetitionState::Lose.is_repetition());
        assert!(!RepetitionState::Superior.is_repetition());
        assert!(!RepetitionState::Inferior.is_repetition());
    }

    #[test]
    fn test_repetition_state_is_decisive() {
        assert!(!RepetitionState::None.is_decisive());
        assert!(!RepetitionState::Draw.is_decisive());
        assert!(RepetitionState::Win.is_decisive());
        assert!(RepetitionState::Lose.is_decisive());
        assert!(!RepetitionState::Superior.is_decisive());
        assert!(!RepetitionState::Inferior.is_decisive());
    }

    #[test]
    fn test_repetition_state_is_superior_inferior() {
        assert!(!RepetitionState::None.is_superior_inferior());
        assert!(!RepetitionState::Draw.is_superior_inferior());
        assert!(!RepetitionState::Win.is_superior_inferior());
        assert!(!RepetitionState::Lose.is_superior_inferior());
        assert!(RepetitionState::Superior.is_superior_inferior());
        assert!(RepetitionState::Inferior.is_superior_inferior());
    }

    #[test]
    fn test_repetition_state_default() {
        assert_eq!(RepetitionState::default(), RepetitionState::None);
    }
}
