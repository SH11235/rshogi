//! 入玉宣言ルール（Entering King Rule）の設定を表現する型。
//!
//! やねうら王の `EnteringKingRule` にほぼ対応するが、
//! Rust 側では `SearchLimits` 等から `Option<EnteringKingRule>` で参照する。

/// 入玉宣言ルールの種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnteringKingRule {
    /// 入玉ルールなし
    None,
    /// 24点法 (31点以上で宣言勝ち)
    Csa24,
    /// 24点法, 駒落ち対応
    Csa24Handicap,
    /// 27点法 = CSAルール (先手28点, 後手27点)
    Csa27,
    /// 27点法, 駒落ち対応
    Csa27Handicap,
    /// トライルール
    TryRule,
}
