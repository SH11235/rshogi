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
//! 2. `kifu-by-id/<id>.meta.json` — 正準メタ (保存済み判定のキー)
//! 3. `games-index/<inv>-<id>.json` — 一覧用の派生 index
//! 4. D1 `games_search_index` の 1 行
//!
//! この順に書き、どれか 1 つでも失敗したら 502 を返す。日付別コピーと
//! `floodgate-history/` は書かない。
//!
//! # 失敗からの復旧
//!
//! 検索用 D1 の cron backfill は全走査を終えた後は最新 100 件しか再訪しないので、過去
//! 日付の対局を取り込む本 API では cron による補完を当てにできない。代わりに:
//!
//! - 502 のあと同じリクエストを再実行すれば直る。meta まで書けていた場合は「保存済み」
//!   として扱い、既存 meta から games-index と D1 を書き直す (wdoor への再取得はしない、
//!   応答は `created: false`)。meta が書けていなかった場合は最初からやり直す。
//! - `force` 上書きが途中で失敗すると、本文と meta の版がずれたり一覧から一時的に
//!   消えたりしうる。`force` の再実行は必ず本文・meta・index・D1 を全部書き直す。
//!
//! # ステータスコード
//!
//! | 状況 | status |
//! |---|---|
//! | 保存した / 保存済みだった | 200 (`created` で区別) |
//! | JSON・対局 ID・`source`・`event` が不正 | 400 |
//! | 管理トークン不一致 | 403 |
//! | wdoor に棋譜が無い | 404 (本文は [`WDOOR_NOT_FOUND_BODY`]) |
//! | wdoor の棋譜が未終局 | 409 |
//! | 本文が 512KB 超 | 413 |
//! | 再生できない棋譜 / `source=wdoor` で `$EVENT` が対局 ID でない | 422 |
//! | wdoor の障害・タイムアウト・別対局の本文、R2 / D1 の障害 | 502 |
//!
//! エラー応答の本文は JSON ではなく短い英文。クライアントが本文で分岐してよいのは
//! [`WDOOR_NOT_FOUND_BODY`] だけ (経路そのものが無いときの 404 と区別するため)。

use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::{Either, select};
use serde::Deserialize;
use worker::{
    AbortController, AbortSignal, Bucket, ByteStream, Delay, Env, Fetch, Headers, Method, Request,
    RequestInit, RequestRedirect, Response, Result,
};

use crate::config::ConfigKeys;
use crate::games_index::{
    KEY_PREFIX as GAMES_INDEX_PREFIX, SOURCE_UPLOAD, SOURCE_WDOOR, games_index_key,
    is_games_index_key_of,
};
use crate::games_search_index::OwnedGamesIndexEntry;
use crate::kifu_import::{
    ImportError, ImportedGame, MAX_CSA_BYTES, WDOOR_NOT_FOUND_BODY, WdoorGameId, import_upload,
    import_wdoor_archive, import_wdoor_fetched, normalize_event,
};
use crate::viewer_api::{is_admin_request, no_store_error};
use crate::x1_paths::{kifu_by_id_meta_key, kifu_by_id_object_key};

/// `import-wdoor` のリクエスト本文の上限。対局 ID と `force` だけなので小さく切る。
const MAX_IMPORT_WDOOR_BODY_BYTES: usize = 4 * 1024;

/// `upload` のリクエスト本文の上限。CSA 本文 (512KB) が JSON 文字列として
/// エスケープされる分 (最悪 1 byte → `\uXXXX` の 6 byte) を見込む。
const MAX_UPLOAD_BODY_BYTES: usize = 6 * MAX_CSA_BYTES + 4 * 1024;

/// 壊れた meta を `force` で上書きするときに、古い games-index を探して走査する
/// R2 list の最大ページ数 (1 ページ 1000 件)。
///
/// 走査は `games-index/` 全体で、list 回数は「終局済み対局数 / 1000」の切り上げ。
/// 現状の数百局なら 1 回、1 万局で 10 回。壊れた meta への `force` という稀な経路でしか
/// 走らないので、1 リクエストの list を 30 回 (3 万局ぶん) までに抑え、それを超えたら
/// 走査を打ち切ってログに残し、手動整理 (`docs/csa-server/deployment.md` §3.2.2) に任せる。
const STALE_INDEX_SCAN_MAX_PAGES: u32 = 30;

/// R2 list の 1 ページの件数 (R2 の上限)。
const STALE_INDEX_SCAN_PAGE_SIZE: u32 = 1000;

/// wdoor への fetch (応答ヘッダ + 本文の読み切り) にかけてよい時間。
const WDOOR_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// wdoor の管理者が取得元を特定して連絡できる User-Agent。
const WDOOR_USER_AGENT: &str = "rshogi-csa-server (+https://github.com/SH11235/rshogi)";

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
    if let Some(existing) = existing.as_ref().filter(|_| !body.force) {
        // 保存済みなら wdoor に取りに行かず、派生データの欠けだけ直す。
        return repair_and_respond(env, &bucket, &game_id, existing).await;
    }

    let origin_url = id.kifu_url();
    let csa_bytes = match fetch_wdoor_csa(&origin_url).await {
        Ok(WdoorFetch::Body(bytes)) => bytes,
        Ok(WdoorFetch::NotFound) => return no_store_error(WDOOR_NOT_FOUND_BODY, 404),
        Ok(WdoorFetch::TooLarge) => return no_store_error("CSA body too large", 413),
        Ok(WdoorFetch::BadStatus(status)) => {
            log_import_failed("kifu_import_wdoor_status", &game_id, &format!("status={status}"));
            return no_store_error("wdoor returned an error", 502);
        }
        Ok(WdoorFetch::TimedOut) => {
            log_import_failed("kifu_import_wdoor_timeout", &game_id, "");
            return no_store_error("wdoor fetch timed out", 502);
        }
        Err(e) => {
            log_import_failed("kifu_import_wdoor_fetch", &game_id, &e.to_string());
            return no_store_error("wdoor fetch failed", 502);
        }
    };
    // wdoor の棋譜は ASCII が基本。万一非 UTF-8 の byte があっても、保存は取得した
    // bytes そのまま、メタ導出だけを置換文字入りの文字列で行う。
    let csa_text = String::from_utf8_lossy(&csa_bytes);
    let game = match import_wdoor_fetched(&id, &csa_text, origin_url) {
        Ok(game) => game,
        Err(e) => return import_error_response(&game_id, &e),
    };
    store_and_respond(env, &bucket, &game, csa_bytes, existing.as_ref()).await
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
        // ゲーム名が `event` になるので、リクエストの `event` は使わない。
        Some(SOURCE_WDOOR) => import_wdoor_archive(&body.csa),
        Some(_) => return no_store_error("source must be upload or wdoor", 400),
    };
    let mut game = match imported {
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
    if let Some(existing) = existing.as_ref().filter(|_| !body.force) {
        return repair_and_respond(env, &bucket, &game.game_id, existing).await;
    }
    game.inherit_origin_url(
        existing
            .as_ref()
            .and_then(|existing| existing.entry.as_ref())
            .and_then(|entry| entry.origin_url.as_deref()),
    );
    store_and_respond(env, &bucket, &game, body.csa.into_bytes(), existing.as_ref()).await
}

/// 保存済みの棋譜 1 局分 (`kifu-by-id/<id>.meta.json` の内容)。
struct ExistingGame {
    meta_bytes: Vec<u8>,
    /// meta が壊れていて読めないときは `None`。
    entry: Option<OwnedGamesIndexEntry>,
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
    let meta_bytes = match object.body() {
        Some(body) => body.bytes().await.map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    let entry = serde_json::from_slice(&meta_bytes).ok();
    Ok(Some(ExistingGame { meta_bytes, entry }))
}

/// 保存済みの対局について、meta から派生する games-index と D1 を書き直す。
///
/// 前回の取り込みが games-index / D1 の書き込みで失敗していても、同じリクエストの
/// 再実行で直るようにする。games-index は無いときだけ put する。D1 は保存時と同じ値
/// (`clock_json` も `canonical_clock_json` で同じ文字列) を upsert するので、内容が
/// 違うときだけ書き換わる。欠けが無ければ R2 head 1 回 + 1 行も変えない D1 文 1 回で終わる。
async fn repair_and_respond(
    env: &Env,
    bucket: &Bucket,
    game_id: &str,
    existing: &ExistingGame,
) -> Result<Response> {
    let Some(entry) = existing.entry.as_ref() else {
        log_import_failed("kifu_import_existing_meta_unreadable", game_id, "");
        return no_store_error("Stored meta is unreadable; retry with force", 502);
    };
    let index_key = match games_index_key(entry.ended_at_ms, &entry.game_id) {
        Ok(key) => key,
        Err(e) => return storage_error("kifu_import_index_key", game_id, &format!("{e:?}")),
    };
    match bucket.head(&index_key).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            if let Err(e) = bucket.put(&index_key, existing.meta_bytes.clone()).execute().await {
                return storage_error("kifu_import_index_put", game_id, &e.to_string());
            }
        }
        Err(e) => return storage_error("kifu_import_index_head", game_id, &e.to_string()),
    }
    if let Err(e) = crate::games_search_index::upsert_owned(env, entry).await {
        return storage_error("kifu_import_search_upsert", game_id, &e.to_string());
    }
    import_response(game_id, false)
}

/// wdoor への fetch の結果。
enum WdoorFetch {
    Body(Vec<u8>),
    NotFound,
    TooLarge,
    /// 200 / 404 以外。リダイレクト (3xx) もここに入る。
    BadStatus(u16),
    TimedOut,
}

/// wdoor の公開 CSA を取得する。
///
/// - リダイレクトは追従しない (取得先を固定 host の外へ出さない)。
/// - 応答ヘッダと本文の読み切りを合わせて [`WDOOR_FETCH_TIMEOUT`] で打ち切る。
/// - 本文は [`MAX_CSA_BYTES`] を超えた時点で読むのをやめる。
async fn fetch_wdoor_csa(url: &str) -> Result<WdoorFetch> {
    let headers = Headers::new();
    headers.set("User-Agent", WDOOR_USER_AGENT)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Manual);
    let request = Request::new_with_init(url, &init)?;

    let controller = AbortController::default();
    let signal = controller.signal();
    let fetch = std::pin::pin!(fetch_wdoor_body(request, &signal));
    let timeout = std::pin::pin!(Delay::from(WDOOR_FETCH_TIMEOUT));
    match select(fetch, timeout).await {
        Either::Left((outcome, _)) => outcome,
        Either::Right(_) => {
            controller.abort();
            Ok(WdoorFetch::TimedOut)
        }
    }
}

async fn fetch_wdoor_body(request: Request, signal: &AbortSignal) -> Result<WdoorFetch> {
    let mut response = Fetch::Request(request).send_with_signal(signal).await?;
    match response.status_code() {
        200 => {}
        404 => return Ok(WdoorFetch::NotFound),
        status => return Ok(WdoorFetch::BadStatus(status)),
    }
    if declared_length_exceeds(response.headers().get("Content-Length")?, MAX_CSA_BYTES) {
        return Ok(WdoorFetch::TooLarge);
    }
    Ok(match read_limited(response.stream()?, MAX_CSA_BYTES).await? {
        Some(bytes) => WdoorFetch::Body(bytes),
        None => WdoorFetch::TooLarge,
    })
}

/// `Content-Length` が上限超えを宣言しているか。ヘッダが無い / 読めない場合は
/// `false` (本文を読みながら累積サイズで判定する)。
fn declared_length_exceeds(content_length: Option<String>, limit: usize) -> bool {
    content_length
        .and_then(|value| value.trim().parse::<usize>().ok())
        .is_some_and(|length| length > limit)
}

/// 本文を chunk 単位で読み、累積が `limit` を超えた時点で読むのをやめて `None` を返す。
///
/// `Content-Length` が無い / 実際より小さい場合でも、上限を超える量をメモリに溜めない。
async fn read_limited(stream: ByteStream, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut stream = std::pin::pin!(stream);
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > limit {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes))
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
    // ルーターは `&Request` を渡すので、本文を読むための複製を作る。本文の無い
    // リクエストは stream を取れないので、空の本文 (= 不正な JSON) として扱う。
    let bytes = match req.clone()?.stream() {
        Ok(stream) => match read_limited(stream, limit).await? {
            Some(bytes) => bytes,
            None => return Ok(Err(no_store_error("Request body too large", 413)?)),
        },
        Err(_) => Vec::new(),
    };
    match serde_json::from_slice(&bytes) {
        Ok(body) => Ok(Ok(body)),
        Err(_) => Ok(Err(no_store_error("Invalid JSON body", 400)?)),
    }
}

/// 新規保存または `force` 上書き。本文 → meta → games-index → D1 の順に書く。
async fn store_and_respond(
    env: &Env,
    bucket: &Bucket,
    game: &ImportedGame,
    csa_bytes: Vec<u8>,
    existing: Option<&ExistingGame>,
) -> Result<Response> {
    let entry = game.entry();
    let meta_body = serde_json::to_vec(&entry)?;
    let index_key = match games_index_key(game.ended_at_ms, &game.game_id) {
        Ok(key) => key,
        Err(e) => return storage_error("kifu_import_index_key", &game.game_id, &format!("{e:?}")),
    };

    // 上書きで終了時刻が変わると games-index の key も変わる。古い key を残すと一覧に
    // 同じ対局が 2 件出るので、何かを書き換える前に消す。消せなければ何も変えずに返す。
    let stale_index_keys = match existing {
        None => Vec::new(),
        Some(ExistingGame {
            entry: Some(previous),
            ..
        }) => games_index_key(previous.ended_at_ms, &previous.game_id)
            .ok()
            .filter(|previous_key| *previous_key != index_key)
            .into_iter()
            .collect(),
        // meta が壊れていて前回の終了時刻が分からない。games-index 全体から、この対局の
        // key を探す。
        Some(ExistingGame { entry: None, .. }) => {
            match find_index_keys_of(bucket, &game.game_id, &index_key).await {
                Ok(keys) => keys,
                Err(e) => return storage_error("kifu_import_stale_index_scan", &game.game_id, &e),
            }
        }
    };
    for stale_key in &stale_index_keys {
        if let Err(e) = bucket.delete(stale_key).await {
            return storage_error("kifu_import_stale_index_delete", &game.game_id, &e.to_string());
        }
    }

    let csa_key = kifu_by_id_object_key(&game.game_id);
    if let Err(e) = bucket.put(&csa_key, csa_bytes).execute().await {
        return storage_error("kifu_import_csa_put", &game.game_id, &e.to_string());
    }
    let meta_key = kifu_by_id_meta_key(&game.game_id);
    if let Err(e) = bucket.put(&meta_key, meta_body.clone()).execute().await {
        return storage_error("kifu_import_meta_put", &game.game_id, &e.to_string());
    }
    if let Err(e) = bucket.put(&index_key, meta_body).execute().await {
        return storage_error("kifu_import_index_put", &game.game_id, &e.to_string());
    }
    if let Err(e) = crate::games_search_index::upsert_entry(env, &entry).await {
        return storage_error("kifu_import_search_upsert", &game.game_id, &e.to_string());
    }

    crate::structured_log!(
        event: "kifu_import_stored",
        component: "kifu_import",
        game_id: game.game_id,
        source: game.source,
    );
    import_response(&game.game_id, true)
}

/// `games-index/` 全体を走査して、`game_id` の key のうち `keep_key` 以外を集める。
///
/// 保存済みの meta が読めず、前回の終了時刻から key を計算できないときだけ使う。
/// [`STALE_INDEX_SCAN_MAX_PAGES`] に達しても続きがある場合は、見つかった分だけ返して
/// warn ログを残す (残りがあれば一覧に重複が出るので、手動で整理する)。
async fn find_index_keys_of(
    bucket: &Bucket,
    game_id: &str,
    keep_key: &str,
) -> std::result::Result<Vec<String>, String> {
    crate::structured_log!(
        event: "kifu_import_stale_index_scan_started",
        component: "kifu_import",
        level: "warn",
        game_id: game_id,
        detail: "stored meta is unreadable; scanning games-index for stale entries",
    );
    let mut found = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..STALE_INDEX_SCAN_MAX_PAGES {
        let mut list = bucket.list().prefix(GAMES_INDEX_PREFIX).limit(STALE_INDEX_SCAN_PAGE_SIZE);
        if let Some(cursor) = cursor.as_deref() {
            list = list.cursor(cursor);
        }
        let page = list.execute().await.map_err(|e| e.to_string())?;
        found.extend(
            page.objects()
                .into_iter()
                .map(|object| object.key())
                .filter(|key| key != keep_key && is_games_index_key_of(key, game_id)),
        );
        if !page.truncated() {
            return Ok(found);
        }
        cursor = page.cursor();
        if cursor.is_none() {
            return Err("games-index list is truncated without a cursor".to_owned());
        }
    }
    crate::structured_log!(
        event: "kifu_import_stale_index_scan_truncated",
        component: "kifu_import",
        level: "warn",
        game_id: game_id,
        detail: "games-index scan stopped at the page limit; a stale entry may remain",
    );
    Ok(found)
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
        ImportError::EventMismatch => 502,
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
