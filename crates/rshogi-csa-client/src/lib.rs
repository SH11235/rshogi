//! `rshogi-csa-client` — USI エンジンを CSA プロトコル対局サーバー
//! （Floodgate / 自リポの Workers 版 / TCP 版など）に接続する CLI ブリッジ。
//!
//! `cargo run -p rshogi-csa-client -- <config.toml>` で利用する。
//! TCP / WebSocket transport を `host` 設定文字列の scheme で切り替える。

pub mod config;
pub mod engine;
pub mod event;
pub mod jsonl;
pub mod protocol;
pub mod record;
pub mod session;
pub mod transport;
