//! マッチング・リーグ管理・ペアリング戦略をまとめるモジュール。
//!
//! 現状はプレイヤ状態機械と League の最小実装、および
//! [`pairing::DirectMatchStrategy`] のみを提供する。Floodgate スケジューラや
//! LeastDiff ペアリングは `pairing` モジュールへ追加する形で拡張する。

pub mod league;
pub mod pairing;
pub mod registry;
