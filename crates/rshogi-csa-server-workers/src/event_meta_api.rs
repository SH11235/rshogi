//! イベントメタの管理 API。
//!
//! `PUT /api/v1/admin/events/{event}`: `{event}` (percent-encode した 1 階層) のメタを
//! D1 `event_meta` に upsert する。`Authorization: Bearer <ADMIN_API_TOKEN>` 必須、応答は
//! 常に `Cache-Control: no-store`。ブラウザから叩く API ではないので CORS ヘッダは付けない。
//!
//! 本文は `{ "title": "...", "organizer"?, "official_url"?, "kifu_source_url"?,
//! "terms_note"? }`。PUT なので全体の置き換えで、省略した任意項目は未登録に戻る。
//! 対局が 1 つも無い `event` にも登録できる。
//!
//! | 状況 | status |
//! |---|---|
//! | 保存した | 200 (`{ "event": "...", "updated_at_ms": ... }`) |
//! | JSON・`event`・各項目が不正 | 400 |
//! | 管理トークン不一致 | 403 |
//! | 本文が上限超え | 413 |
//! | D1 の障害 | 502 |

use worker::wasm_bindgen::JsValue;
use worker::{Date, Env, Request, Response, Result};

use crate::config::ConfigKeys;
use crate::event_meta::{EVENT_META_UPSERT_SQL, EventMetaRequest, event_from_path_segment};
use crate::kifu_import_api::read_json_body;
use crate::viewer_api::{is_admin_request, no_store_error};

/// リクエスト本文の上限。各項目の文字数上限 (合計 2,400 文字) がすべて JSON の
/// 6 byte エスケープ 2 個 (サロゲートペア) で来ても収まる大きさ。
const MAX_EVENT_META_BODY_BYTES: usize = 32 * 1024;

/// `PUT /api/v1/admin/events/{event}`。`segment` は path の `{event}` 部分 (encode 済み)。
pub(crate) async fn handle_put_event_meta(
    req: &Request,
    env: &Env,
    segment: &str,
) -> Result<Response> {
    if !is_admin_request(req, env)? {
        return no_store_error("Forbidden", 403);
    }
    let event = match event_from_path_segment(segment) {
        Ok(event) => event,
        Err(message) => return no_store_error(message, 400),
    };
    let body: EventMetaRequest = match read_json_body(req, MAX_EVENT_META_BODY_BYTES).await? {
        Ok(body) => body,
        Err(response) => return Ok(response),
    };
    let meta = match body.validate() {
        Ok(meta) => meta,
        Err(message) => return no_store_error(message, 400),
    };
    let updated_at_ms = Date::now().as_millis();
    let optional = |value: &Option<String>| match value {
        Some(value) => JsValue::from_str(value),
        None => JsValue::NULL,
    };
    let values = [
        JsValue::from_str(&event),
        JsValue::from_str(&meta.title),
        optional(&meta.organizer),
        optional(&meta.official_url),
        optional(&meta.kifu_source_url),
        optional(&meta.terms_note),
        JsValue::from_f64(updated_at_ms as f64),
    ];
    let stored = match env.d1(ConfigKeys::GAMES_SEARCH_DB_BINDING) {
        Ok(db) => match db.prepare(EVENT_META_UPSERT_SQL).bind(&values) {
            Ok(statement) => statement.run().await.map(|_| ()),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    if let Err(e) = stored {
        crate::structured_log!(
            event: "event_meta_upsert_failed",
            component: "event_meta",
            detail: e.to_string(),
        );
        return no_store_error("Storage error", 502);
    }
    crate::structured_log!(
        event: "event_meta_stored",
        component: "event_meta",
        event_tag: event,
    );
    let mut response = Response::from_json(&serde_json::json!({
        "event": event,
        "updated_at_ms": updated_at_ms,
    }))?;
    response.headers_mut().set("Cache-Control", "no-store")?;
    Ok(response)
}
