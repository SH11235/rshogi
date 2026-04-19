//! マッチング・リーグ管理・ペアリング戦略をまとめるモジュール。
//!
//! Phase 1 ではプレイヤ状態機械と League の最小実装、および
//! [`pairing::DirectMatchStrategy`] のみを提供する。Phase 4 で
//! Floodgate スケジューラや LeastDiff ペアリングを `pairing` モジュールへ追加する。

pub mod league;
pub mod pairing;
