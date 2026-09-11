//! 置換表モジュール
//!
//! 探索結果をキャッシュする置換表（Transposition Table）。
//!
//! - `TTEntry`: エントリ（10バイト、16bitキー）
//! - `Cluster`: エントリのグループ（32バイト）
//! - `TranspositionTable`: テーブル本体
//! - 世代管理
//! - prefetch
//!
//! # YaneuraOu（CLUSTER_SIZE=3）準拠
//!
//! クラスターインデックスは64bitキーの上位ビットで決定し、
//! クラスター内マッチングに下位16bitを使用する。
//! payload の AtomicU64 × 3 + 短縮キーの AtomicU16 × 3 + 2バイトパディング
//! = 32バイト/クラスター。排他は取らず、payload 内のフィールド対応だけを不可分に保つ。
//! 短縮キーと payload は別ワードなので、異なる書込みの組が観測されうる。

mod alloc;
mod entry;
mod table;

pub use entry::{TTData, TTEntry};
pub use table::{ProbeResult, TranspositionTable};

/// クラスターサイズ（エントリ数）
/// YaneuraOu準拠: payload 8bytes × 3 + 短縮キー 2bytes × 3 + padding 2bytes = 32bytes
pub const CLUSTER_SIZE: usize = 3;

/// Generation関連の定数
pub const GENERATION_BITS: u32 = 3;
pub const GENERATION_DELTA: u8 = 1 << GENERATION_BITS; // 8
pub const GENERATION_CYCLE: u16 = 255 + GENERATION_DELTA as u16;
pub const GENERATION_MASK: u16 = 0xF8; // (0xFF << GENERATION_BITS) as u8
