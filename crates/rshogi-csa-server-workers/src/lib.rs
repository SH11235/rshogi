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

pub mod admin_auth;
pub mod attachment;
// `client_kind` は `X-Client` ヘッダ値を運用ログ向けに正規化する純粋ロジック。
// `viewer_api` (wasm32 専用) から呼ばれるが、本体は I/O 非依存なのでホスト
// target でもテストできるよう公開モジュールとして配置する。
pub mod client_kind;
// `backfill` は cron trigger (`#[event(scheduled)]`) からのみ消費される。
// ホスト target で参照する公開 API は Stats / Deserialize 型のみで、IO 関数本体
// は wasm32 ゲートで切り離している。それでもホスト通常ビルド (cargo build) では
// 消費者が無くなり dead_code 警告が出るため、`persistence` 等と同様に
// wasm32 + test ゲーティングで揃える。
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) mod backfill;
pub mod config;
pub mod datetime;
// `export_retry` は finalize_if_ended / handle_export_retry_alarm から消費される
// 純粋ロジック (RETRY_DELAYS_SEC 等)。ホスト target でも cargo test から到達できる
// ように `pub mod` で公開する。
pub mod export_retry;
pub mod floodgate_history;
pub mod games_index;
pub mod live_games_index;
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
// `spectator_snapshot` は DO ランタイムから消費される I/O 非依存の純粋関数で、
// `persistence` モジュール (`PersistedConfig` / `MoveRow` / `FinishedState`) を
// 参照する。`persistence` と同じ wasm32 + test ゲーティングで揃え、ホスト
// target の `cargo test` から到達可能にする。
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) mod spectator_snapshot;
pub mod ws_route;
pub mod x1_paths;

// `observability` は `structured_log!` macro を提供する。`#[macro_export]` で
// crate root に export されるため `pub use` 等は不要。本 mod 自体はホスト
// target でも import 可能 (macro 定義は wasm32 限定の `worker::` 呼び出しを
// 含むため、展開はホストでは失敗する点に注意)。
pub mod observability;

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

/// Backfill + sweep を両方走らせる cron 文字列 (毎時 0 分)。
///
/// `wrangler.{production,staging,toml.example}.toml` の `[triggers] crons` 配列
/// 第 1 要素と一致させる。`scheduled` ハンドラは `event.cron()` の文字列マッチで
/// backfill 実行可否を分岐するため、wrangler 側の表記を変える場合は本定数も
/// 同期更新する (`tests/wrangler_environment_toml_consistency.rs` /
/// `wrangler_template_consistency.rs` で gate)。
pub const BACKFILL_CRON: &str = "0 * * * *";

/// Sweep のみ走らせる cron 文字列 (15 / 30 / 45 分)。
///
/// https://github.com/SH11235/rshogi/issues/629 で導入した orphan sweep 高頻度化用。`try_delete_live_games_index`
/// が R2 transient で失敗した場合の復旧遅延を 1 時間 → 15 分に短縮する目的で、
/// 本 cron では backfill (Class A put 増) を走らせず sweep のみ実行する。
pub const SWEEP_ONLY_CRON: &str = "15,30,45 * * * *";

/// Workers ランタイムの scheduled イベント (cron trigger)。
///
/// `wrangler.toml` の `[triggers] crons` で 2 つの cron を登録している
/// (https://github.com/SH11235/rshogi/issues/551 / https://github.com/SH11235/rshogi/issues/629):
///
/// - [`BACKFILL_CRON`] (`0 * * * *`): `run_games_index_backfill` →
///   `run_live_orphan_sweep` を順次実行する (= 既存挙動)。
/// - [`SWEEP_ONLY_CRON`] (`15,30,45 * * * *`): `run_live_orphan_sweep` のみ。
///   delete best-effort 失敗時の復旧遅延を 15 分以内に詰めるための高頻度経路。
///
/// `event.cron()` 文字列で分岐し、未知の cron 文字列に対しては安全側に倒して
/// 既存挙動 (backfill + sweep) と等価動作する (= 設定変更時の暫定挙動を保証)。
///
/// 各ジョブは内部的に best-effort で失敗を握り潰し `Result::Ok` で返すため、
/// scheduled handler 側ではさらに伝播禁止 (cron の継続可用性を最優先する) で
/// `Err` も logfmt のみ記録する。`env` は `&env` 経由で渡し `Env: Clone` 前提
/// を作らない (設計 v2 §4)。
#[cfg(target_arch = "wasm32")]
#[worker::event(scheduled)]
pub async fn scheduled(
    event: worker::ScheduledEvent,
    env: worker::Env,
    _ctx: worker::ScheduleContext,
) {
    let cron = event.cron();
    // SWEEP_ONLY_CRON では backfill を skip。それ以外は既存挙動 (backfill +
    // sweep) を保つ。未知 cron 文字列は安全側 = 全部走らせる。
    let run_backfill = cron != SWEEP_ONLY_CRON;

    if run_backfill {
        if let Err(e) = backfill::run_games_index_backfill(&env).await {
            crate::structured_log!(
                event: "games_index_backfill_failed",
                component: "scheduled",
                cron: cron,
                err: format!("{e:?}"),
            );
        }
    }
    if let Err(e) = backfill::run_live_orphan_sweep(&env).await {
        crate::structured_log!(
            event: "live_orphan_sweep_failed",
            component: "scheduled",
            cron: cron,
            err: format!("{e:?}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_constants_are_distinct() {
        // 同一文字列に揃えると `event.cron()` 分岐が崩れる (sweep-only cron で
        // backfill が走る、または backfill cron で sweep がスキップされる)。
        assert_ne!(BACKFILL_CRON, SWEEP_ONLY_CRON);
    }

    #[test]
    fn cron_constants_have_5_fields() {
        // Cloudflare Workers cron expression は 5 field (minute hour dom month dow)。
        // 6 field (秒つき) や 7 field (年つき) は受け付けないため、定数側で固定。
        for cron in [BACKFILL_CRON, SWEEP_ONLY_CRON] {
            let fields: Vec<&str> = cron.split_whitespace().collect();
            assert_eq!(fields.len(), 5, "cron expression must have 5 fields, got {cron:?}");
        }
    }
}
