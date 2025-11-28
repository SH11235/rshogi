//! 合法手生成の型定義

use crate::types::Move;

/// 1局面での最大合法手数
/// 理論上の最大は593手だが、余裕を持たせる
pub const MAX_MOVES: usize = 600;

/// 指し手生成のタイプ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenType {
    /// 駒を取る指し手 + 歩の敵陣での成り
    CapturesProPlus,
    /// 駒を取らない指し手 - 歩の敵陣での成り
    QuietsProMinus,
    /// 王手回避手
    Evasions,
    /// 王手がかかっていないときの全ての手
    NonEvasions,
    /// 合法手すべて（is_legal()チェック付き）
    Legal,
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
