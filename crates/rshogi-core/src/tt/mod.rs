//! 置換表モジュール
//!
//! 探索結果をキャッシュする置換表（Transposition Table）。
//!
//! - `TTEntry`: エントリ（16バイト、64bitキー対応）
//! - `Cluster`: エントリのグループ（64バイト、キャッシュライン最適化）
//! - `TranspositionTable`: テーブル本体
//! - 世代管理
//! - prefetch
//!
//! # 64bitキー
//!
//! YaneuraOuの拡張方式に準拠し、64bitキーでマッチングを行う。
//! 16bitキーでは衝突確率が高く棋力低下の原因となっていたため、
//! 64bitキーに拡張して衝突確率を大幅に低減（2^16 → 2^64）。

mod alloc;
mod entry;
mod table;

pub use entry::{TTData, TTEntry};
pub use table::{ProbeResult, TranspositionTable};

/// クラスターサイズ（エントリ数）
/// 64bitキー対応: 16bytes × 3 = 48bytes
/// キャッシュライン（64バイト）に収まる
pub const CLUSTER_SIZE: usize = 3;

/// Generation関連の定数
pub const GENERATION_BITS: u32 = 3;
pub const GENERATION_DELTA: u8 = 1 << GENERATION_BITS; // 8
pub const GENERATION_CYCLE: u16 = 255 + GENERATION_DELTA as u16;
pub const GENERATION_MASK: u16 = 0xF8; // (0xFF << GENERATION_BITS) as u8
