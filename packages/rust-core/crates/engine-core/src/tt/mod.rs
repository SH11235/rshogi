//! 置換表モジュール
//!
//! 探索結果をキャッシュする置換表（Transposition Table）。
//!
//! - `TTEntry`: エントリ（10バイト）
//! - `Cluster`: エントリのグループ（キャッシュライン効率化）
//! - `TranspositionTable`: テーブル本体
//! - 世代管理
//! - prefetch

mod entry;
mod table;

pub use entry::{TTData, TTEntry};
pub use table::{ProbeResult, TranspositionTable};

/// クラスターサイズ（エントリ数）
/// 10bytes × 3 + 2padding = 32bytes
pub const CLUSTER_SIZE: usize = 3;

/// Generation関連の定数
pub const GENERATION_BITS: u32 = 3;
pub const GENERATION_DELTA: u8 = 1 << GENERATION_BITS; // 8
pub const GENERATION_CYCLE: u16 = 255 + GENERATION_DELTA as u16;
pub const GENERATION_MASK: u16 = 0xF8; // (0xFF << GENERATION_BITS) as u8
