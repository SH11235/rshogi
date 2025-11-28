//! 境界値種別（Bound）

use super::Value;

/// 境界値種別（置換表に格納する値の種類）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Bound {
    /// なし
    #[default]
    None = 0,
    /// 上界（fail-low: 真の値はこれ以下）
    Upper = 1,
    /// 下界（fail-high: 真の値はこれ以上）
    Lower = 2,
    /// 正確な値
    Exact = 3,
}

impl Bound {
    /// TTカットオフ判定に使用
    #[inline]
    pub const fn can_cutoff(self, value: Value, beta: Value) -> bool {
        match self {
            Bound::Exact => true,
            Bound::Lower => value.raw() >= beta.raw(),
            Bound::Upper => value.raw() < beta.raw(),
            Bound::None => false,
        }
    }

    /// u8から変換
    #[inline]
    pub const fn from_u8(n: u8) -> Option<Bound> {
        match n {
            0 => Some(Bound::None),
            1 => Some(Bound::Upper),
            2 => Some(Bound::Lower),
            3 => Some(Bound::Exact),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bound_from_u8() {
        assert_eq!(Bound::from_u8(0), Some(Bound::None));
        assert_eq!(Bound::from_u8(1), Some(Bound::Upper));
        assert_eq!(Bound::from_u8(2), Some(Bound::Lower));
        assert_eq!(Bound::from_u8(3), Some(Bound::Exact));
        assert_eq!(Bound::from_u8(4), None);
    }

    #[test]
    fn test_bound_can_cutoff() {
        let value = Value::new(100);
        let beta = Value::new(50);

        // Exact は常にカットオフ可能
        assert!(Bound::Exact.can_cutoff(value, beta));

        // Lower: value >= beta でカットオフ
        assert!(Bound::Lower.can_cutoff(value, beta));
        assert!(!Bound::Lower.can_cutoff(Value::new(30), beta));

        // Upper: value < beta でカットオフ
        assert!(!Bound::Upper.can_cutoff(value, beta));
        assert!(Bound::Upper.can_cutoff(Value::new(30), beta));

        // None は常にカットオフ不可
        assert!(!Bound::None.can_cutoff(value, beta));
    }

    #[test]
    fn test_bound_default() {
        assert_eq!(Bound::default(), Bound::None);
    }
}
