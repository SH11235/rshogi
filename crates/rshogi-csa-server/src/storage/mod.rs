//! 永続化ポートとアダプタ実装。
//!
//! `floodgate_history` モジュールはランタイム非依存な trait + entry 型を
//! 公開し、tokio ベースの JSONL 実装は `tokio-transport` feature 配下で
//! のみコンパイルされる。Workers (Cloudflare DO) など他のランタイムは同 trait
//! を実装する形で別 crate から接続する。
//!
//! 残りのアダプタ（`buoy` / `file` / `players_yaml`）は現状 tokio 前提のため
//! `tokio-transport` 配下のみでコンパイルされる。

pub mod floodgate_history;

#[cfg(feature = "tokio-transport")]
pub mod buoy;
#[cfg(feature = "tokio-transport")]
pub mod file;
#[cfg(feature = "tokio-transport")]
pub mod players_yaml;
