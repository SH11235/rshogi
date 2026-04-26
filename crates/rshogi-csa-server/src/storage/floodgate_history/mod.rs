//! Floodgate 履歴の永続化ポート。
//!
//! `FloodgateHistoryStorage` trait と履歴 entry の型 (`FloodgateHistoryEntry`,
//! `HistoryColor`) はランタイムに依存しないため `port` / `types` モジュールで
//! 常時コンパイル可能とし、TCP / Workers の双方から trait を実装できる形に
//! しておく。
//!
//! 具体実装はランタイム別に分かれる:
//!
//! - `JsonlFloodgateHistoryStorage`: tokio ベースの JSONL append-only 実装。
//!   `tokio-transport` feature 配下でのみコンパイル
//! - Workers (Cloudflare DO) 向けの実装は `rshogi-csa-server-workers` 側に置き、
//!   本モジュールの trait を実装する

mod port;
mod types;

#[cfg(feature = "tokio-transport")]
mod jsonl;

pub use port::FloodgateHistoryStorage;
pub use types::{FloodgateHistoryEntry, HistoryColor};

#[cfg(feature = "tokio-transport")]
pub use jsonl::JsonlFloodgateHistoryStorage;
