//! TCP フロントエンドのライブラリクレート。
//!
//! `rshogi-csa-server` コアを `tokio::net::TcpListener` で受け付ける 1 プロセス
//! サーバーとして稼働させるための配線層。
//!
//! 公開 API:
//! - [`transport::TcpTransport`]: 1 接続分の行 I/O を [`rshogi_csa_server::ClientTransport`] として提供。
//! - [`broadcaster::InMemoryBroadcaster`]: 同一プロセス内で観戦者接続を保持する `Broadcaster` 実装。
//! - [`rate_limit::IpLoginRateLimiter`]: 同一 IP からの LOGIN 試行を制限するイン・メモリ実装。
//! - [`auth`]: パスワードハッシュ照合と `RateStorage` 経由の認証経路。
//! - [`server::run_server`]: accept ループと 1 接続分のタスク spawn を担うエントリ関数。

// Workers 側のアダプタを本クレートに取り込んでしまうのは設計上の事故。
// feature unification で誤って立ってしまった場合、コンパイル時点で止める。
#[cfg(feature = "workers")]
compile_error!(
    "rshogi-csa-server-tcp does not support the `workers` feature; \
     this crate wires the native tokio runtime and must not link the \
     Cloudflare Workers adapter."
);

pub mod auth;
pub mod broadcaster;
pub mod metrics;
pub mod rate_limit;
pub mod server;
pub mod transport;

pub use auth::{AuthError, AuthOutcome, PasswordHasher, PlainPasswordHasher, authenticate};
pub use broadcaster::InMemoryBroadcaster;
pub use rate_limit::IpLoginRateLimiter;
pub use server::{ServerConfig, run_server};
pub use transport::TcpTransport;
