//! rshogi-csa-server-workers — Cloudflare Workers フロントエンド。
//!
//! コアの I/O 非依存な `GameRoom::handle_line` を Workers の Durable Object
//! (`GameRoom` DO) 上で駆動し、WebSocket Hibernation でアイドル時のアプリ
//! 常時実行を避ける。
//!
//! # ビルドターゲット
//!
//! Cloudflare Workers の wasm32-unknown-unknown 向け cdylib として
//! `worker-build` からビルドされる。純粋ロジックのモジュール
//! (`attachment`, `config`, `datetime`, `origin`, `room_id`, `session_state`)
//! はホスト target でも rlib としてコンパイル・テストでき、workspace 全体の
//! `cargo check` / `cargo test` を壊さない。
//! WebSocket 受付や Durable Object 関連モジュール (`router`, `game_room`) は
//! wasm32 でのみ有効化され、`wrangler dev` (Miniflare) 下で統合検証する。

// wasm32 ランタイムは tokio multi-threaded primitive を扱えない。TCP 側の
// feature が何らかの経路で混入した場合はコンパイル時点で停止する。
#[cfg(feature = "tokio-transport")]
compile_error!(
    "rshogi-csa-server-workers does not support the `tokio-transport` feature; \
     the wasm32 runtime cannot use tokio multi-threaded primitives."
);

pub mod attachment;
pub mod config;
pub mod datetime;
pub mod floodgate_history;
pub mod games_index;
pub mod lobby_protocol;
pub mod origin;
// `persistence` は DO ランタイム (`game_room`) からのみ消費される I/O 非依存の
// 純粋ロジックを置く。ホスト target の通常ビルドでは消費者が存在しないので
// `cargo build` の dead-code 解析と整合させるため、wasm32 ビルドとテスト時のみ
// コンパイルする。テストはホスト target で `cargo test` から到達できる。
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) mod persistence;
// `reconnect` も `persistence` と同じく DO ランタイム専用の I/O 非依存ロジック
// (grace registry のスキーマ、`PendingAlarmKind`、`build_resume_message` 等)。
// ホスト target からはテスト経由でしか到達しないため、wasm32 とテスト時のみ
// コンパイルする。
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) mod reconnect;
pub mod room_id;
pub mod session_state;
pub mod spectator_control;
pub mod ws_route;
pub mod x1_paths;

#[cfg(target_arch = "wasm32")]
mod game_room;
#[cfg(target_arch = "wasm32")]
mod lobby;
#[cfg(target_arch = "wasm32")]
mod router;
#[cfg(target_arch = "wasm32")]
mod viewer_api;

#[cfg(target_arch = "wasm32")]
pub use game_room::GameRoom;
#[cfg(target_arch = "wasm32")]
pub use lobby::Lobby;

/// Workers ランタイムの fetch イベント。`router::handle_fetch` に委譲する
/// 薄いエントリポイント。
///
/// `#[event(fetch)]` マクロが呼び出し側の wasm-bindgen 配線を生成する。
#[cfg(target_arch = "wasm32")]
#[worker::event(fetch)]
pub async fn fetch(
    req: worker::Request,
    env: worker::Env,
    _ctx: worker::Context,
) -> worker::Result<worker::Response> {
    router::handle_fetch(req, env).await
}
