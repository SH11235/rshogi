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
//! `CryptoProvider` が起動時に明示登録されていることを要求する。**呼び忘れた
//! 状態で `wss://` 接続を試みると TLS ハンドシェイク時に panic する**。
//!
//! **本 crate からは provider を install しない**（複数 consumer が同 process
//! に同居したときに二重 install を避けるため）。consumer 側 `main()` 起動時に
//! 1 度だけ次のいずれかを呼ぶこと:
//!
//! ```ignore
//! let _ = rustls::crypto::ring::default_provider().install_default();
//! ```
//!
//! 本 crate 同梱の `csa_client` バイナリ (`src/main.rs`) はこれを行っているが、
//! library として取り込む consumer は自分で同等の初期化を行う必要がある。
//!
//! # Panics
//!
//! `websocket` feature 有効時、上記 `CryptoProvider` の install を行わずに
//! `wss://` 経路で `CsaConnection::connect_with_target` 等を呼ぶと `rustls`
//! 内部で panic する。consumer 側で起動時 install を必ず行うこと。
//!
//! # SessionEventSink (対局途中の進捗通知)
//!
//! `run_game_session_with_events` / `run_resumed_session_with_events` に
//! [`SessionEventSink`] 実装を渡すと、対局ループの各イベント
//! ([`SessionProgress`]) が consumer に push 通知される。詳細は
//! [`events`] モジュールの doc を参照。
//!
//! - sink の `on_event` は対局メインループ thread 上で同期呼び出しされる。
//!   重い処理 (DB write / network publish 等) を直接行うと対局ループ全体が
//!   遅延し USI engine 探索や CSA サーバ応答に影響する。consumer は軽量な
//!   channel 送信のみ行うこと。
//! - sink が `SinkError::NonFatal` を返した場合は warn ログのみで対局継続。
//!   `SinkError::Fatal` を返した場合は best-effort attempt at clean closure
//!   (`%CHUDAN` → `LOGOUT` → transport close → `on_error` → `Disconnected`)
//!   を行ってから [`SessionError::SinkAborted`] で return する。
//! - resume 経路では指し手 history の replay は emit しない。consumer は
//!   [`ReconnectState::last_sfen`] から盤面を再構築する責任を持つ。
//! - [`SearchInfoSnapshot`] は累積 snapshot で、observed したぶんだけ field を
//!   上書きする (差分ではなく常に「現時点までの最新値の集合」)。

pub mod config;
pub mod engine;
pub mod event;
pub mod events;
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
pub use events::{
    BestMoveEvent, DisconnectReason, GameEndEvent, GameEndReason, MoveEvent, MovePlayer,
    NoopSessionEventSink, ReconnectState, SearchInfoEmitPolicy, SearchInfoSnapshot, SearchOrigin,
    SessionError, SessionEventSink, SessionOutcome, SessionProgress, Side, SinkError,
};
pub use protocol::{CsaConnection, GameResult, GameSummary};
pub use record::{GameRecord, RecordedMove};
pub use session::{
    run_game_session, run_game_session_with_events, run_resumed_session,
    run_resumed_session_with_events,
};
pub use transport::{ConnectOpts, CsaTransport, TransportTarget};
