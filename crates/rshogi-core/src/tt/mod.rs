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
//! 10バイトatomic格納 × 3 + 1バイト排他 + 1パディング = 32バイト/クラスター。
//! 読み書きはクラスタ単位の短い排他でsnapshotの整合性を保証し、競合時は待たずにmiss/書込み省略とする。

mod alloc;
mod entry;
mod table;

pub use entry::{TTData, TTEntry};
pub use table::{ProbeResult, TranspositionTable};

/// クラスターサイズ（エントリ数）
/// YaneuraOu準拠: 10bytes × 3 + lock/padding 2bytes = 32bytes
pub const CLUSTER_SIZE: usize = 3;

/// Generation関連の定数
pub const GENERATION_BITS: u32 = 3;
pub const GENERATION_DELTA: u8 = 1 << GENERATION_BITS; // 8
pub const GENERATION_CYCLE: u16 = 255 + GENERATION_DELTA as u16;
pub const GENERATION_MASK: u16 = 0xF8; // (0xFF << GENERATION_BITS) as u8
