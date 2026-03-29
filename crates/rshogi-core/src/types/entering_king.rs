//! 入玉ルール（EnteringKingRule）
//!
//! YaneuraOu の `EnteringKingRule` に準拠。
//! 入玉宣言勝ちの判定条件を決定するルール設定。

/// 入玉ルール
///
/// USI オプション `EnteringKingRule` で選択する。
/// デフォルトは CSA 27点法（`Point27`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EnteringKingRule {
    /// ルールなし（宣言勝ち無効）
    None,
    /// 24点法（先手後手共に31点以上で宣言勝ち）
    Point24,
    /// 24点法（駒落ち対応）
    Point24H,
    /// 27点法 = CSAルール（先手28点以上、後手27点以上）
    #[default]
    Point27,
    /// 27点法（駒落ち対応）
    Point27H,
    /// トライルール（敵の初期玉位置に自玉を移動）
    TryRule,
}

impl EnteringKingRule {
    /// USI オプション文字列からの変換
    pub fn from_usi(s: &str) -> Option<Self> {
        match s {
            "NoEnteringKing" => Some(Self::None),
            "CSARule24" => Some(Self::Point24),
            "CSARule24H" => Some(Self::Point24H),
            "CSARule27" => Some(Self::Point27),
            "CSARule27H" => Some(Self::Point27H),
            "TryRule" => Some(Self::TryRule),
            _ => Option::None,
        }
    }

    /// USI オプション文字列への変換
    pub fn to_usi(self) -> &'static str {
        match self {
            Self::None => "NoEnteringKing",
            Self::Point24 => "CSARule24",
            Self::Point24H => "CSARule24H",
            Self::Point27 => "CSARule27",
            Self::Point27H => "CSARule27H",
            Self::TryRule => "TryRule",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_point27() {
        assert_eq!(EnteringKingRule::default(), EnteringKingRule::Point27);
    }

    #[test]
    fn test_usi_roundtrip() {
        let rules = [
            EnteringKingRule::None,
            EnteringKingRule::Point24,
            EnteringKingRule::Point24H,
            EnteringKingRule::Point27,
            EnteringKingRule::Point27H,
            EnteringKingRule::TryRule,
        ];
        for rule in rules {
            let s = rule.to_usi();
            let parsed = EnteringKingRule::from_usi(s).unwrap();
            assert_eq!(parsed, rule, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn test_from_usi_unknown() {
        assert_eq!(EnteringKingRule::from_usi("Unknown"), Option::None);
        assert_eq!(EnteringKingRule::from_usi(""), Option::None);
    }
}
