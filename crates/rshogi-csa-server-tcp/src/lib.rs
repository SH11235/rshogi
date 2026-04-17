//! TCP フロントエンドのライブラリクレート。
//!
//! `rshogi-csa-server` コアを `tokio::net::TcpListener` で受け付ける 1 プロセスサーバーとして
//! 稼働させるための配線層。Phase 1 MVP（設計書 `.kiro/specs/rshogi-csa-server/`）の
//! タスク 7.x（TCP 受付、認証、レート制限、E2E）と 8.1（Phase 1→2 ゲート）を担う。
//!
//! 公開 API は以下の通り:
//! - [`transport::TcpTransport`]: 1 接続分の行 I/O を [`rshogi_csa_server::ClientTransport`] として提供。
//! - [`broadcaster::InMemoryBroadcaster`]: 同一プロセス内で観戦者接続を保持する `Broadcaster` 実装。
//! - [`rate_limit::IpLoginRateLimiter`]: 同一 IP からの LOGIN 試行を制限するイン・メモリ実装。
//! - [`auth`]: パスワードハッシュ照合と `RateStorage` 経由の認証経路。

pub mod auth;
pub mod broadcaster;
pub mod phase_gate;
pub mod rate_limit;
pub mod transport;

pub use auth::{AuthError, AuthOutcome, PasswordHasher, PlainPasswordHasher, authenticate};
pub use broadcaster::InMemoryBroadcaster;
pub use phase_gate::{CURRENT_PHASE, PHASE1_LOCK, PhaseGate, assert_phase1_only};
pub use rate_limit::IpLoginRateLimiter;
pub use transport::TcpTransport;
