//! 構造化ログ macro。
//!
//! Cloudflare Workers Logs / Tail Workers / dashboard から JSON フィールドで
//! aggregate / grep できるよう、全 log を 1 行 JSON で出力する。
//! (Cloudflare Workers Logs Phase A,
//! <https://github.com/SH11235/rshogi/issues/625>)
//!
//! # 必須キー
//!
//! - `event`: snake_case の event 名 (例: `"logout_lobby"`, `"start_match_aborted"`)。
//!   alert / aggregation の primary 軸。
//! - `component`: 発火元 module 名 (例: `"lobby"`, `"game_room"`, `"backfill"`)。
//!   `[component]` プレフィックス文化からの移行先。
//!
//! # 任意キー
//!
//! 文脈に応じて `game_id` / `room_id` / `handle` / `role` / `err` 等、
//! [`serde::Serialize`] を実装した任意の値を乗せられる。識別子名がそのまま
//! JSON キーになる ([`stringify!`] 経由)。
//!
//! # `ts_ms` の自動付与
//!
//! Cloudflare Logs Engine 側でも timestamp は付くが、Tail Workers / R2 archive
//! 経路で再度 millisec 精度を引き直したいケースのために、本 macro は
//! [`worker::Date::now`] から `ts_ms` を自動付与する。
//!
//! # 使い方
//!
//! ```ignore
//! use crate::structured_log;
//!
//! structured_log!(
//!     event: "logout_lobby",
//!     component: "lobby",
//!     handle: handle,
//!     attachment_id: attachment_id,
//! );
//! ```
//!
//! `err={:?}` のような Debug 表示が欲しい場合は呼び出し側で
//! [`format!`] してから渡す:
//!
//! ```ignore
//! structured_log!(
//!     event: "games_index_backfill_get_failed",
//!     component: "backfill",
//!     key: key,
//!     err: format!("{e:?}"),
//! );
//! ```
//!
//! # ホスト target
//!
//! このマクロは [`worker::Date::now`] と [`worker::console_log!`] を経由する
//! ため、wasm32 でのみ展開される ([`worker`] crate がホスト target で参照
//! 不可)。ホスト test 経路では呼び出さない契約。

/// 構造化ログ (Cloudflare Workers Logs Phase A) の唯一の出力経路。
///
/// 詳細は [`crate::observability`] module の doc を参照。
#[macro_export]
macro_rules! structured_log {
    (
        event: $event:expr,
        component: $component:expr
        $(, $k:ident : $v:expr)* $(,)?
    ) => {{
        let payload = ::serde_json::json!({
            "ts_ms": ::worker::Date::now().as_millis(),
            "event": $event,
            "component": $component,
            $(stringify!($k): $v,)*
        });
        ::worker::console_log!("{}", payload);
    }};
}
