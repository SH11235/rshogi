//! rshogi-csa-server-workers — Cloudflare Workers フロントエンド (Phase 2)。
//!
//! コアの I/O 非依存な `GameRoom::handle_line` / `League` を Workers の
//! Durable Object (`GameRoom` DO) 上で駆動し、WebSocket Hibernation で
//! アイドル時のアプリ常時実行を避ける。設計の出典は
//! `docs/csa-server/design.md` §8、タスク定義は
//! `.kiro/specs/rshogi-csa-server/tasks.md` §9〜10。
//!
//! # ビルドターゲット
//!
//! 本 crate は Cloudflare Workers の wasm32-unknown-unknown 向け cdylib として
//! `worker-build` からビルドされる。純粋ロジック (`phase_gate`, `origin`,
//! `config`) はホスト target でも `rlib` としてコンパイル・テストでき、
//! workspace 全体の `cargo check` / `cargo test` を壊さない。
//! WebSocket 受付や Durable Object 関連モジュール (`router`, `game_room`) は
//! wasm32 でのみ有効化され、`wrangler dev` (Miniflare) 下で統合検証する。

pub mod attachment;
pub mod config;
pub mod datetime;
pub mod origin;
pub mod phase_gate;
pub mod session_state;

#[cfg(target_arch = "wasm32")]
mod game_room;
#[cfg(target_arch = "wasm32")]
mod router;

#[cfg(target_arch = "wasm32")]
pub use game_room::GameRoom;

/// Workers ランタイムの fetch イベント。axum 等を経由せず直接
/// [`router::handle_fetch`] に委譲する薄いエントリポイント。
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
