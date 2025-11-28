//! 評価値（Value）

/// 評価値
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Value(i32);

impl Value {
    /// ゼロ
    pub const ZERO: Value = Value(0);
    /// 引き分け
    pub const DRAW: Value = Value(0);
    /// 詰み（勝ち側の最大スコア）
    pub const MATE: Value = Value(32000);
    /// 無限大
    pub const INFINITE: Value = Value(32001);
    /// 無効値
    pub const NONE: Value = Value(32002);

    /// 最大探索深度内での詰みスコア
    pub const MATE_IN_MAX_PLY: Value = Value(Self::MATE.0 - 128);
    /// 最大探索深度内での詰まされスコア
    pub const MATED_IN_MAX_PLY: Value = Value(-Self::MATE_IN_MAX_PLY.0);

    /// 値から生成
    #[inline]
    pub const fn new(v: i32) -> Value {
        Value(v)
    }

    /// ply手で詰ますスコア
    #[inline]
    pub const fn mate_in(ply: i32) -> Value {
        Value(Self::MATE.0 - ply)
    }

    /// ply手で詰まされるスコア
    #[inline]
    pub const fn mated_in(ply: i32) -> Value {
        Value(-Self::MATE.0 + ply)
    }

    /// 勝ちスコアかどうか
    #[inline]
    pub const fn is_win(self) -> bool {
        self.0 >= Self::MATE_IN_MAX_PLY.0
    }

    /// 負けスコアかどうか
    #[inline]
    pub const fn is_loss(self) -> bool {
        self.0 <= Self::MATED_IN_MAX_PLY.0
    }

    /// 詰みスコア（勝ちまたは負け）かどうか
    #[inline]
    pub const fn is_mate_score(self) -> bool {
        self.is_win() || self.is_loss()
    }

    /// 生の値を取得
    #[inline]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// 詰み手数を取得（詰みスコアの場合のみ有効）
    #[inline]
    pub const fn mate_ply(self) -> i32 {
        if self.is_win() {
            Self::MATE.0 - self.0
        } else if self.is_loss() {
            self.0 + Self::MATE.0
        } else {
            0
        }
    }
}

impl Default for Value {
    fn default() -> Self {
        Value::ZERO
    }
}

impl std::ops::Neg for Value {
    type Output = Value;

    #[inline]
    fn neg(self) -> Value {
        Value(-self.0)
    }
}

impl std::ops::Add for Value {
    type Output = Value;

    #[inline]
    fn add(self, rhs: Value) -> Value {
        Value(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Value {
    type Output = Value;

    #[inline]
    fn sub(self, rhs: Value) -> Value {
        Value(self.0 - rhs.0)
    }
}

impl std::ops::AddAssign for Value {
    #[inline]
    fn add_assign(&mut self, rhs: Value) {
        self.0 += rhs.0;
    }
}

impl std::ops::SubAssign for Value {
    #[inline]
    fn sub_assign(&mut self, rhs: Value) {
        self.0 -= rhs.0;
    }
}

impl std::ops::Mul<i32> for Value {
    type Output = Value;

    #[inline]
    fn mul(self, rhs: i32) -> Value {
        Value(self.0 * rhs)
    }
}

impl std::ops::Div<i32> for Value {
    type Output = Value;

    #[inline]
    fn div(self, rhs: i32) -> Value {
        Value(self.0 / rhs)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Value {
        Value(v)
    }
}

impl From<Value> for i32 {
    fn from(v: Value) -> i32 {
        v.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_constants() {
        assert_eq!(Value::ZERO.raw(), 0);
        assert_eq!(Value::DRAW.raw(), 0);
        assert_eq!(Value::MATE.raw(), 32000);
        assert_eq!(Value::INFINITE.raw(), 32001);
        assert_eq!(Value::NONE.raw(), 32002);
    }

    #[test]
    fn test_value_mate_in() {
        let v = Value::mate_in(5);
        assert!(v.is_win());
        assert!(!v.is_loss());
        assert!(v.is_mate_score());
        assert_eq!(v.mate_ply(), 5);
    }

    #[test]
    fn test_value_mated_in() {
        let v = Value::mated_in(3);
        assert!(!v.is_win());
        assert!(v.is_loss());
        assert!(v.is_mate_score());
        assert_eq!(v.mate_ply(), 3);
    }

    #[test]
    fn test_value_is_win_loss() {
        assert!(Value::MATE.is_win());
        assert!(!Value::MATE.is_loss());

        let v = Value::mated_in(1);
        assert!(!v.is_win());
        assert!(v.is_loss());

        assert!(!Value::ZERO.is_win());
        assert!(!Value::ZERO.is_loss());
        assert!(!Value::ZERO.is_mate_score());
    }

    #[test]
    fn test_value_neg() {
        assert_eq!(-Value::new(100), Value::new(-100));
        assert_eq!(-Value::ZERO, Value::ZERO);
    }

    #[test]
    fn test_value_add_sub() {
        let a = Value::new(100);
        let b = Value::new(50);
        assert_eq!(a + b, Value::new(150));
        assert_eq!(a - b, Value::new(50));
    }

    #[test]
    fn test_value_mul_div() {
        let v = Value::new(100);
        assert_eq!(v * 3, Value::new(300));
        assert_eq!(v / 2, Value::new(50));
    }

    #[test]
    fn test_value_ordering() {
        assert!(Value::MATE > Value::ZERO);
        assert!(Value::ZERO > Value::mated_in(1));
        assert!(Value::mate_in(1) > Value::mate_in(10));
        assert!(Value::mated_in(10) > Value::mated_in(1));
    }

    #[test]
    fn test_value_from() {
        let v: Value = 100.into();
        assert_eq!(v.raw(), 100);

        let i: i32 = v.into();
        assert_eq!(i, 100);
    }
}
