#![doc = include_str!("../README.md")]

pub mod types;

/// このビルドで有効な feature のうち、探索・評価の結果を変えうるもの (名前順のカンマ区切り)。
/// 判定は `build/result_features.rs`。結果を変えないと確かめた feature 以外はすべて含む。
pub const RESULT_FEATURES: &str = env!("RSHOGI_CORE_RESULT_FEATURES");

#[cfg(feature = "allocation-stats")]
pub mod allocation_stats;

// 盤面表現
pub mod bitboard;
pub mod eval;
pub mod position;

// 合法手生成
pub mod movegen;

// NNUE評価
pub mod nnue;

// 置換表
pub mod tt;

//  探索
pub mod search;

pub(crate) mod prefetch;

// 時刻（Instant）抽象化
pub(crate) mod time;

// 1手詰め探索
pub mod mate;

pub use position::json_conversion;
