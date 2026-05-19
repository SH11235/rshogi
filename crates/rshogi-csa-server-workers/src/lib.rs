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
// `handle_auth` は `WORKERS_HANDLE_AUTH` whitelist の parse + password SHA256
// 比較 (issue #664) を担う。`HandleAuthRegistry` + `verify` は I/O 非依存の
// pure helper でホスト target からテストできるよう `pub mod` で公開する。
// wasm32 限定の `load_handle_auth_registry` (env 読み出し配線) は本 module 内で
// `#[cfg(target_arch = "wasm32")]` 化する。
pub mod handle_auth;
pub mod live_games_index;
pub mod lobby_protocol;
pub mod origin;
// `rate_limit` は host テスト可能な pure helper (TokenBucketState, threshold
// resolution, Retry-After 算出) と wasm32-only な `RateLimiter` Durable Object を
// 同居させる。DO 部分は `#[cfg(target_arch = "wasm32")]` で gate しているので、
// ホスト target からも `cargo test` で pure logic を直接走らせられる
// (issue #622 PR3a)。
pub mod rate_limit;
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

// `observability` は `structured_log!` macro を提供する。`#[macro_export]` は
// crate root に export するため、外部 crate からの `use` 用途で `pub mod` 化
// する必要はない (本 crate は cdylib として Workers ランタイム単独消費される
// 想定で、library API として外部公開もしない)。`pub(crate)` でシグナルを
// 「内部利用のみ」に絞り、誤って library 公開と誤読されないようにする。
// macro 定義は wasm32 限定の `worker::` 呼び出しを含むため、ホスト target
// では展開時に失敗する点に注意。
pub(crate) mod observability;

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
#[cfg(target_arch = "wasm32")]
pub use rate_limit::RateLimiter;

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

/// `[event(scheduled)]` ハンドラ (`scheduled`) を起動する単一 cron 文字列。
///
/// 0 / 15 / 30 / 45 分の 4 回発火する。`wrangler.{production,staging,toml.example}.toml`
/// の `[triggers] crons` 配列と一致させる (`tests/wrangler_environment_toml_consistency.rs`
/// / `wrangler_template_consistency.rs` で gate)。
///
/// 以前は backfill 用 (`0 * * * *`) と sweep-only 用 (`15,30,45 * * * *`) の 2 cron に
/// 分けていたが、Cloudflare の account あたり cron trigger 上限 (5) に収めるため 1 cron に
/// 統合した。発火時刻の分は `event.schedule()` から求め、[`BACKFILL_MINUTE`] のときだけ
/// backfill を実行する。
pub const SCHEDULED_CRON: &str = "0,15,30,45 * * * *";

/// backfill (`run_games_index_backfill`) を実行する分 (0-59)。
///
/// [`SCHEDULED_CRON`] の 4 発火のうち、スケジュール時刻の「時の何分か」がこの値に一致する
/// 発火でのみ backfill + sweep を行う。それ以外の発火は sweep のみ。
pub const BACKFILL_MINUTE: i64 = 0;

/// epoch ミリ秒から「時の何分か」(0-59) を求める純粋関数。
///
/// [`scheduled`] が単一 cron [`SCHEDULED_CRON`] の発火 (0/15/30/45 分) を
/// `event.schedule()` の値で区別するために使う。`event.schedule()` は f64 を
/// 返すが、現在の epoch ミリ秒 (~1.7e12) は f64 仮数部 (53bit ≈ 9e15) に収まり
/// 整数部に精度劣化はない。`scheduled` 本体は wasm32 限定のため host target からは
/// テスト経由でのみ到達する → `backfill` 等と同じく wasm32 + test ゲートする。
#[cfg(any(target_arch = "wasm32", test))]
fn minute_of_hour(epoch_ms: f64) -> i64 {
    // 60_000 ミリ秒で割って 60 で剰余を取る。epoch は常に非負なので剰余も 0-59。
    (epoch_ms / 60_000.0) as i64 % 60
}

/// Workers ランタイムの scheduled イベント (cron trigger)。
///
/// `wrangler.toml` の `[triggers] crons` で単一 cron [`SCHEDULED_CRON`]
/// (`0,15,30,45 * * * *`) を登録しており、0 / 15 / 30 / 45 分の 4 回発火する。
/// 単一 cron なので `event.cron()` は 4 発火すべてで同じ文字列を返す。発火の
/// 区別は [`minute_of_hour`] に `event.schedule()` を渡して「時の何分か」を求めて
/// 行う。`event.schedule()` は cron の予定時刻 (実起動時刻ではない) の epoch
/// ミリ秒なので、起動が数秒遅延しても分判定は予定どおり安定する:
///
/// - 分が [`BACKFILL_MINUTE`] (0 分) のとき: `run_games_index_backfill` →
///   `run_live_orphan_sweep` を順次実行する。
/// - それ以外 (15 / 30 / 45 分) のとき: `run_live_orphan_sweep` のみ。
///   delete best-effort 失敗時の復旧遅延を 15 分以内に詰めるための高頻度経路
///   (https://github.com/SH11235/rshogi/issues/629)。
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
    // 単一 cron のため発火の区別は cron 文字列でなくスケジュール時刻の分で行う。
    let minute = minute_of_hour(event.schedule());
    let run_backfill = minute == BACKFILL_MINUTE;

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
    fn scheduled_cron_has_5_fields() {
        // Cloudflare Workers cron expression は 5 field (minute hour dom month dow)。
        let fields: Vec<&str> = SCHEDULED_CRON.split_whitespace().collect();
        assert_eq!(fields.len(), 5, "cron expression must have 5 fields, got {SCHEDULED_CRON:?}");
    }

    #[test]
    fn scheduled_cron_minute_field_contains_backfill_minute() {
        // SCHEDULED_CRON の分フィールドに BACKFILL_MINUTE が含まれないと backfill が
        // 永久に発火しない。分フィールドがカンマ区切りリスト形式 (`"0,15,30,45"`)
        // であることを前提とする (range `"0-59"` / step `"*/15"` 記法は
        // SCHEDULED_CRON では未使用なので考慮しない)。
        let minute_field = SCHEDULED_CRON.split_whitespace().next().expect("cron must have fields");
        let minutes: Vec<&str> = minute_field.split(',').collect();
        assert!(
            minutes.contains(&BACKFILL_MINUTE.to_string().as_str()),
            "SCHEDULED_CRON minute field {minute_field:?} must include BACKFILL_MINUTE ({BACKFILL_MINUTE})",
        );
    }

    #[test]
    fn minute_of_hour_maps_epoch_ms_to_minute() {
        // 0/15/30/45 分の各発火がそのまま分に落ちる。
        assert_eq!(minute_of_hour(0.0), 0);
        assert_eq!(minute_of_hour(900_000.0), 15);
        assert_eq!(minute_of_hour(1_800_000.0), 30);
        assert_eq!(minute_of_hour(2_700_000.0), 45);
        // 1 時間ちょうど (3_600_000 ms) は剰余で 0 分に巻き戻る。
        assert_eq!(minute_of_hour(3_600_000.0), 0);
        assert_eq!(minute_of_hour(4_500_000.0), 15);
    }

    #[test]
    fn minute_of_hour_truncates_sub_minute_delay() {
        // cron 予定時刻からの起動遅延 (< 60s) は整数切り捨てで吸収され、
        // 予定分に丸められる。45s 遅れた 0 分発火は 0 分、30s 遅れた 15 分発火は
        // 15 分のまま。
        assert_eq!(minute_of_hour(45_000.0), 0);
        assert_eq!(minute_of_hour(900_000.0 + 30_000.0), 15);
    }

    #[test]
    fn minute_of_hour_handles_current_epoch_scale() {
        // 現在スケール (~1.7e12 ms) の epoch でも 0/15/30/45 分が正しく落ち、
        // f64 仮数部の精度劣化が無いことを確認する。
        // base = 2026-01-01T00:00:00Z = 1_767_225_600_000 ms (60_000 の倍数)。
        let base = 1_767_225_600_000.0_f64;
        assert_eq!(minute_of_hour(base), 0);
        assert_eq!(minute_of_hour(base + 900_000.0), 15);
        assert_eq!(minute_of_hour(base + 1_800_000.0), 30);
        assert_eq!(minute_of_hour(base + 2_700_000.0), 45);
        // 分境界の前後: :00:00.001 と :00:59.999 はどちらも 0 分に落ちる。
        assert_eq!(minute_of_hour(base + 1.0), 0);
        assert_eq!(minute_of_hour(base + 59_999.0), 0);
    }
}
