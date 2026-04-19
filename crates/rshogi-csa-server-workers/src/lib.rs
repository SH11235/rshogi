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
//! `worker-build` からビルドされる。ホスト target（x86_64 等）では
//! [`phase_gate`] を除く実装モジュールは空になり、workspace 全体の
//! `cargo check` / `cargo test` を壊さない。
//!
//! Workers 固有の統合テストは `wrangler dev` (Miniflare) 下で実行する。
//! wasm-bindgen-test などホスト非依存のハーネスは今後追加する。

pub mod phase_gate;
