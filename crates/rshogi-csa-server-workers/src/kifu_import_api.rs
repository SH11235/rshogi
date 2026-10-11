//! 外部棋譜取り込みの管理 API。
//!
//! - `POST /api/v1/admin/games/import-wdoor`: wdoor の対局 ID を受け取り、Worker が
//!   wdoor の公開 CSA を 1 回だけ取得して保存する (参照取り込み)。
//! - `POST /api/v1/admin/games/upload`: CSA 本文を受け取って保存する。
//!
//! どちらも `Authorization: Bearer <ADMIN_API_TOKEN>` 必須で、応答は常に
//! `Cache-Control: no-store`。ブラウザから叩く API ではないので CORS ヘッダは付けない。
//!
//! # 保存するもの
//!
//! 1. `kifu-by-id/<id>.csa` — 受け取った本文そのまま
//! 2. `kifu-by-id/<id>.meta.json` — 正準メタ (既存判定のキー)
//! 3. `games-index/<inv>-<id>.json` — 一覧用の派生 index
//! 4. D1 `games_search_index` の 1 行
//!
//! 1 → 2 の順に書き、どちらかが失敗したら 502 を返す (meta があれば本文もある、を
//! 保つ)。3 と 4 は best-effort で、失敗しても cron backfill が meta を起点に補完する。
//! 日付別コピーと `floodgate-history/` は書かない。
//!
//! # ステータスコード
//!
//! | 状況 | status |
//! |---|---|
//! | 保存した / 保存済みだった | 200 (`created` で区別) |
//! | JSON・対局 ID・`source`・`event` が不正 | 400 |
//! | 管理トークン不一致 | 403 |
//! | wdoor に棋譜が無い | 404 |
//! | wdoor の棋譜が未終局 | 409 |
//! | 本文が 512KB 超 | 413 |
//! | 再生できない棋譜 / `source=wdoor` で `$EVENT` が対局 ID でない | 422 |
//! | wdoor・R2 の障害 | 502 |

use serde::Deserialize;
use worker::{Bucket, Env, Fetch, Request, Response, Result, Url};

use crate::backfill::MetaForIndexKey;
use crate::config::ConfigKeys;
use crate::games_index::{SOURCE_UPLOAD, SOURCE_WDOOR, games_index_key};
use crate::kifu_import::{
    ImportError, ImportedGame, MAX_CSA_BYTES, WdoorGameId, import_upload, import_wdoor,
    import_wdoor_archive, normalize_event,
};
use crate::viewer_api::{is_admin_request, no_store_error};
use crate::x1_paths::{kifu_by_id_meta_key, kifu_by_id_object_key};

/// `import-wdoor` のリクエスト本文の上限。対局 ID と `force` だけなので小さく切る。
const MAX_IMPORT_WDOOR_BODY_BYTES: usize = 4 * 1024;

/// `upload` のリクエスト本文の上限。CSA 本文 (512KB) が JSON 文字列として
/// エスケープされる分 (最悪 1 byte → `\uXXXX` の 6 byte) を見込む。
const MAX_UPLOAD_BODY_BYTES: usize = 6 * MAX_CSA_BYTES + 4 * 1024;

#[derive(Debug, Deserialize)]
struct ImportWdoorRequest {
    wdoor_game_id: String,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
struct UploadRequest {
    csa: String,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    force: bool,
}

/// `POST /api/v1/admin/games/import-wdoor`
pub(crate) async fn handle_import_wdoor(req: &Request, env: &Env) -> Result<Response> {
    if !is_admin_request(req, env)? {
        return no_store_error("Forbidden", 403);
    }
    let body: ImportWdoorRequest = match read_json_body(req, MAX_IMPORT_WDOOR_BODY_BYTES).await? {
        Ok(body) => body,
        Err(response) => return Ok(response),
    };
    let Some(id) = WdoorGameId::parse(&body.wdoor_game_id) else {
        return no_store_error("Invalid wdoor_game_id", 400);
    };
    let game_id = id.game_id();
    let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
        Ok(bucket) => bucket,
        Err(e) => return storage_error("kifu_import_bucket_binding", &game_id, &e.to_string()),
    };
    let existing = match load_existing(&bucket, &game_id).await {
        Ok(existing) => existing,
        Err(e) => return storage_error("kifu_import_existing_lookup", &game_id, &e),
    };
    if existing.is_some() && !body.force {
        // 保存済みなら wdoor に取りに行かない。
        return import_response(&game_id, false);
    }

    let origin_url = id.kifu_url();
    let csa_bytes = match fetch_wdoor_csa(&origin_url, &game_id).await? {
        Ok(bytes) => bytes,
        Err(response) => return Ok(response),
    };
    // wdoor の棋譜は ASCII が基本。万一非 UTF-8 の byte があっても、保存は取得した
    // bytes そのまま、メタ導出だけを置換文字入りの文字列で行う。
    let csa_text = String::from_utf8_lossy(&csa_bytes);
    let game = match import_wdoor(&id, &csa_text, Some(origin_url), true) {
        Ok(game) => game,
        Err(e) => return import_error_response(&game_id, &e),
    };
    store_and_respond(env, &bucket, &game, csa_bytes, existing).await
}

/// `POST /api/v1/admin/games/upload`
pub(crate) async fn handle_upload(req: &Request, env: &Env) -> Result<Response> {
    if !is_admin_request(req, env)? {
        return no_store_error("Forbidden", 403);
    }
    let body: UploadRequest = match read_json_body(req, MAX_UPLOAD_BODY_BYTES).await? {
        Ok(body) => body,
        Err(response) => return Ok(response),
    };
    if body.csa.len() > MAX_CSA_BYTES {
        return no_store_error("CSA body too large", 413);
    }
    let imported = match body.source.as_deref() {
        None | Some(SOURCE_UPLOAD) => match normalize_event(body.event.as_deref()) {
            Ok(event) => import_upload(&body.csa, event),
            Err(message) => return no_store_error(message, 400),
        },
        Some(SOURCE_WDOOR) => import_wdoor_archive(&body.csa),
        Some(_) => return no_store_error("source must be upload or wdoor", 400),
    };
    let game = match imported {
        Ok(game) => game,
        Err(e) => return import_error_response("", &e),
    };
    let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
        Ok(bucket) => bucket,
        Err(e) => {
            return storage_error("kifu_import_bucket_binding", &game.game_id, &e.to_string());
        }
    };
    let existing = match load_existing(&bucket, &game.game_id).await {
        Ok(existing) => existing,
        Err(e) => return storage_error("kifu_import_existing_lookup", &game.game_id, &e),
    };
    if existing.is_some() && !body.force {
        return import_response(&game.game_id, false);
    }
    store_and_respond(env, &bucket, &game, body.csa.into_bytes(), existing).await
}

/// 保存済みの棋譜 1 局分。`force` 上書き時に古い games-index key を消すために
/// 終了時刻を持つ (meta が壊れていて読めないときは `None`)。
struct ExistingGame {
    ended_at_ms: Option<u64>,
}

/// `kifu-by-id/<id>.meta.json` の有無で保存済みかを判定する。
async fn load_existing(
    bucket: &Bucket,
    game_id: &str,
) -> std::result::Result<Option<ExistingGame>, String> {
    let meta_key = kifu_by_id_meta_key(game_id);
    let Some(object) = bucket.get(&meta_key).execute().await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let ended_at_ms = match object.body() {
        Some(body) => {
            let bytes = body.bytes().await.map_err(|e| e.to_string())?;
            serde_json::from_slice::<MetaForIndexKey>(&bytes)
                .ok()
                .map(|meta| meta.ended_at_ms)
        }
        None => None,
    };
    Ok(Some(ExistingGame { ended_at_ms }))
}

/// wdoor の公開 CSA を取得する。外側の `Err` は Worker の内部エラー、内側の `Err` は
/// クライアントへ返す応答 (404 / 413 / 502)。
async fn fetch_wdoor_csa(
    url: &str,
    game_id: &str,
) -> Result<std::result::Result<Vec<u8>, Response>> {
    let parsed = Url::parse(url).map_err(|e| worker::Error::RustError(e.to_string()))?;
    let mut response = match Fetch::Url(parsed).send().await {
        Ok(response) => response,
        Err(e) => {
            log_import_failed("kifu_import_wdoor_fetch", game_id, &e.to_string());
            return Ok(Err(no_store_error("wdoor fetch failed", 502)?));
        }
    };
    match response.status_code() {
        200 => {}
        404 => return Ok(Err(no_store_error("Not Found on wdoor", 404)?)),
        status => {
            log_import_failed("kifu_import_wdoor_status", game_id, &format!("status={status}"));
            return Ok(Err(no_store_error("wdoor returned an error", 502)?));
        }
    }
    if declared_length_exceeds(response.headers().get("Content-Length")?, MAX_CSA_BYTES) {
        return Ok(Err(no_store_error("CSA body too large", 413)?));
    }
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            log_import_failed("kifu_import_wdoor_read", game_id, &e.to_string());
            return Ok(Err(no_store_error("wdoor fetch failed", 502)?));
        }
    };
    if bytes.len() > MAX_CSA_BYTES {
        return Ok(Err(no_store_error("CSA body too large", 413)?));
    }
    Ok(Ok(bytes))
}

/// `Content-Length` が上限超えを宣言しているか。ヘッダが無い / 読めない場合は
/// `false` (本文を読んだ後の実サイズで判定する)。
fn declared_length_exceeds(content_length: Option<String>, limit: usize) -> bool {
    content_length
        .and_then(|value| value.trim().parse::<usize>().ok())
        .is_some_and(|length| length > limit)
}

/// リクエスト本文を上限つきで読み、JSON として decode する。内側の `Err` は
/// クライアントへ返す 400 / 413 応答。
async fn read_json_body<T: serde::de::DeserializeOwned>(
    req: &Request,
    limit: usize,
) -> Result<std::result::Result<T, Response>> {
    if declared_length_exceeds(req.headers().get("Content-Length")?, limit) {
        return Ok(Err(no_store_error("Request body too large", 413)?));
    }
    // ルーターは `&Request` を渡すので、本文を読むための複製を作る。
    let bytes = req.clone()?.bytes().await?;
    if bytes.len() > limit {
        return Ok(Err(no_store_error("Request body too large", 413)?));
    }
    match serde_json::from_slice(&bytes) {
        Ok(body) => Ok(Ok(body)),
        Err(_) => Ok(Err(no_store_error("Invalid JSON body", 400)?)),
    }
}

async fn store_and_respond(
    env: &Env,
    bucket: &Bucket,
    game: &ImportedGame,
    csa_bytes: Vec<u8>,
    existing: Option<ExistingGame>,
) -> Result<Response> {
    let entry = game.entry();
    let meta_body = serde_json::to_vec(&entry)?;
    let index_key = match games_index_key(game.ended_at_ms, &game.game_id) {
        Ok(key) => key,
        Err(e) => return storage_error("kifu_import_index_key", &game.game_id, &format!("{e:?}")),
    };

    let csa_key = kifu_by_id_object_key(&game.game_id);
    if let Err(e) = bucket.put(&csa_key, csa_bytes).execute().await {
        return storage_error("kifu_import_csa_put", &game.game_id, &e.to_string());
    }
    let meta_key = kifu_by_id_meta_key(&game.game_id);
    if let Err(e) = bucket.put(&meta_key, meta_body.clone()).execute().await {
        return storage_error("kifu_import_meta_put", &game.game_id, &e.to_string());
    }

    // 上書きで終了時刻が変わると games-index の key も変わる。古い key を残すと
    // 一覧に同じ対局が 2 件出るので消す。
    if let Some(previous_ended_at_ms) = existing.and_then(|existing| existing.ended_at_ms)
        && let Ok(previous_key) = games_index_key(previous_ended_at_ms, &game.game_id)
        && previous_key != index_key
        && let Err(e) = bucket.delete(&previous_key).await
    {
        log_import_failed("kifu_import_stale_index_delete", &game.game_id, &e.to_string());
    }

    // games-index と D1 は派生データ。失敗しても cron backfill が meta から補完する。
    match bucket.put(&index_key, meta_body).execute().await {
        Ok(_) => {
            if let Err(e) = crate::games_search_index::upsert_entry(env, &entry).await {
                log_import_failed("kifu_import_search_upsert", &game.game_id, &e.to_string());
            }
        }
        Err(e) => log_import_failed("kifu_import_index_put", &game.game_id, &e.to_string()),
    }

    crate::structured_log!(
        event: "kifu_import_stored",
        component: "kifu_import",
        game_id: game.game_id,
        source: game.source,
    );
    import_response(&game.game_id, true)
}

fn import_response(game_id: &str, created: bool) -> Result<Response> {
    let mut response = Response::from_json(&serde_json::json!({
        "game_id": game_id,
        "created": created,
    }))?;
    response.headers_mut().set("Cache-Control", "no-store")?;
    Ok(response)
}

fn import_error_response(game_id: &str, error: &ImportError) -> Result<Response> {
    let status = match error {
        ImportError::Unfinished => 409,
        ImportError::NotWdoorEvent | ImportError::Unplayable(_) => 422,
    };
    crate::structured_log!(
        event: "kifu_import_rejected",
        component: "kifu_import",
        game_id: game_id,
        status: status,
        detail: error.to_string(),
    );
    no_store_error(error.to_string(), status)
}

fn storage_error(event: &str, game_id: &str, detail: &str) -> Result<Response> {
    log_import_failed(event, game_id, detail);
    no_store_error("Storage error", 502)
}

fn log_import_failed(event: &str, game_id: &str, detail: &str) {
    crate::structured_log!(
        event: event,
        component: "kifu_import",
        game_id: game_id,
        detail: detail,
    );
}
