//! CSA対局クライアント
//!
//! USIエンジンをCSAプロトコル対局サーバー（floodgate等）に接続するブリッジ。

pub mod config;
pub mod engine;
pub mod event;
pub mod protocol;
pub mod record;
pub mod session;
