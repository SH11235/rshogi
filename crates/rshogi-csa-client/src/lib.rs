//! `rshogi-csa-client` — USI エンジンを CSA プロトコル対局サーバー
//! （Floodgate / 自リポの Workers 版 / TCP 版など）に接続する library + CLI。
//!
//! `cargo run -p rshogi-csa-client -- <config.toml>` で CLI として利用するほか、
//! 別 crate (例: Tauri 製デスクトップ frontend) から library として組み込むこと
//! もできる。`host` 設定文字列の scheme で TCP / WebSocket transport を切り替える。
//!
//! # Features
//!
//! - `tcp` (既定有効): `std::net` ベースの TCP transport。常時利用可能。
//! - `websocket` (既定有効): `tungstenite` + `rustls` の sync WebSocket transport。
//!   無効化すると `WsTransport` / `CsaTransport::WebSocket` / `TransportTarget::WebSocket`
//!   が消え、`TransportTarget::from_host_port` に `ws://` / `wss://` URL を渡すと
//!   `Err` を返す。
//! - `cli` (既定有効): `csa_client` バイナリと clap / ctrlc / env_logger を pull
//!   する。library として取り込む consumer は `default-features = false` で
//!   無効化することで CLI 系依存を切り落とせる。
//!
//! # rustls CryptoProvider に関する注意
//!
//! `websocket` feature を有効化した場合、`rustls 0.23` は process-level の
//! `CryptoProvider` が起動時に明示登録されていることを要求する（未登録だと TLS
//! ハンドシェイク時に panic する）。**本 crate からは provider を install しない**
//! （複数 consumer が同 process に同居したときに二重 install を避けるため）。
//!
//! consumer 側 `main()` 起動時に 1 度だけ次のいずれかを呼ぶこと:
//!
//! ```ignore
//! let _ = rustls::crypto::ring::default_provider().install_default();
//! ```
//!
//! 本 crate 同梱の `csa_client` バイナリ (`src/main.rs`) はこれを行っているが、
//! library として取り込む consumer は自分で同等の初期化を行う必要がある。

pub mod config;
pub mod engine;
pub mod event;
pub mod jsonl;
pub mod protocol;
pub mod record;
pub mod session;
pub mod transport;

// crate root に主要 API を再エクスポート。consumer は
// `use rshogi_csa_client::{CsaClientConfig, UsiEngine, ...}` で参照できる。
// 型名は実装側に合わせており、別名は付与しない。
pub use config::CsaClientConfig;
pub use engine::{BestMoveResult, SearchInfo, SearchOutcome, UsiEngine};
pub use event::Event;
pub use protocol::{CsaConnection, GameResult, GameSummary};
pub use record::{GameRecord, RecordedMove};
pub use session::{run_game_session, run_resumed_session};
pub use transport::{ConnectOpts, CsaTransport, TransportTarget};
