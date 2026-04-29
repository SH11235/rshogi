//! viewer 配信 HTTP API (`/api/v1/games`) のルーティングと R2 アクセス。
//!
//! v3 設計 (Issue #542 issuecomment-4338088406) に準拠する 3 エンドポイント:
//!
//! - `GET /api/v1/games?cursor=<opaque>&limit=<N>` 一覧 (終局済)
//!   `KIFU_BUCKET.list({prefix: "games-index/", cursor, limit})` を 1 回呼び、
//!   各オブジェクト本文 (= [`crate::games_index::GamesIndexEntry`] の JSON) を
//!   そのまま `games[]` に詰めて返す。`next_cursor` は R2 list の cursor を
//!   opaque 転送する。
//! - `GET /api/v1/games/live?cursor=<opaque>&limit=<N>` 一覧 (進行中)
//!   `KIFU_BUCKET.list({prefix: "live-games-index/", cursor, limit})` を 1 回呼び、
//!   各オブジェクト本文 (= [`crate::live_games_index::LiveGamesIndexEntry`] の JSON)
//!   を `live_games[]` に詰めて返す (Issue #549)。終局済 list と同じ pagination
//!   semantics、`/api/v1/games/<id>` のような単局エンドポイントは進行中対局には
//!   設けない (= viewer 側は live entry を **発見手段** として扱い、行クリック時に
//!   WS spectate 接続で実状態を確認する)。
//! - `GET /api/v1/games/<game_id>` 単局 (終局済)
//!   `kifu-by-id/<encoded_game_id>.csa` を直接 get する。本文 (CSA V2) と
//!   `games-index/` から取得した meta を合わせて返す。
//!
//! いずれも GameRoom DO を経由せず、Worker 直 fetch のみで完結する (R2 read
//! 1 ホップ)。CORS は staging では `WS_ALLOWED_ORIGINS` をそのまま流用して
//! ramu-shogi origin に絞る (実装の柔軟性で OK)。
//!
//! # access control レビュー必須
//!
//! `try_handle` 配下に新しい `/api/v1/*` エンドポイントを追加する場合は、
//! `check_origin` / `with_cors` の通過と `WS_ALLOWED_ORIGINS` allowlist 体系
//! (Issue #550 で強化予定) に確実に乗ることを必ずレビューする。allowlist 未設定
//! 環境での挙動 (= Origin ヘッダなしで通る) も含めて回帰させない。

use serde::Serialize;
use worker::{Env, Headers, Method, Request, Response, Result, Url};

use crate::config::{ConfigKeys, OriginAllowList, is_viewer_api_enabled};
use crate::games_index::KEY_PREFIX as GAMES_INDEX_PREFIX;
use crate::live_games_index::LIVE_KEY_PREFIX;
use crate::origin::{OriginDecision, evaluate};
use crate::x1_paths::kifu_by_id_object_key;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 100;
const MIN_LIMIT: u32 = 1;

/// `/api/v1/games[/...]` 配下のリクエストを判定して該当ハンドラに振り分ける。
///
/// 戻り値 `Some(_)` はマッチしたことを示す。`None` の場合は既存ルーティングに
/// 引き継ぐ (404 までの fallthrough)。
pub async fn try_handle(req: &Request, env: &Env) -> Result<Option<Response>> {
    if req.method() != Method::Get {
        return Ok(None);
    }
    // viewer 配信 API は `ALLOW_VIEWER_API` で opt-in 有効化する。無効化 / 未設定
    // / 値不正のいずれも `Ok(None)` を返して既存ルーティングへフォールスルー
    // させる（最終的に 404 になる）。production rollout 中の kill-switch も同経路。
    if !is_viewer_api_enabled(env) {
        return Ok(None);
    }
    let url = req.url()?;
    let path = url.path().to_owned();

    if path == "/api/v1/games" {
        return Ok(Some(handle_list(req, env, &url).await?));
    }
    // `/api/v1/games/live` は `/api/v1/games/<game_id>` より先にマッチさせる
    // (`live` という ID の単局取得を 1 件目で誤って受けないため)。
    if path == "/api/v1/games/live" {
        return Ok(Some(handle_list_live(req, env, &url).await?));
    }
    if let Some(rest) = path.strip_prefix("/api/v1/games/") {
        if rest.is_empty() || rest.contains('/') {
            // 余分な階層 (`/api/v1/games/x/y`) や末尾 `/` は 404 で扱う。
            return Ok(Some(Response::error("Not Found", 404)?));
        }
        return Ok(Some(handle_get(req, env, rest).await?));
    }

    Ok(None)
}

/// 一覧 API (`/api/v1/games`) レスポンスの wire 形状。
#[derive(Debug, Serialize)]
struct ListResponse {
    /// `games-index/` のオブジェクト本文をそのまま吐き出すのが契約 (本モジュールでは
    /// meta の再構築をしない)。MVP では `serde_json::Value` でラウンドトリップ parse
    /// する素朴実装。要素数 1 ページ最大 100 件のため性能影響は許容。`RawValue` 化は
    /// レイテンシが顕在化したときの将来拡張で検討する。
    games: Vec<serde_json::Value>,
    next_cursor: Option<String>,
}

/// 進行中対局一覧 API (`/api/v1/games/live`) レスポンスの wire 形状。
///
/// `games` ではなく `live_games` をキーに使うことで、終局済一覧との混在を
/// client 側で取り違えないように分離する (viewer 側は配列キー名を見て描画
/// ルートを切り替えられる)。
#[derive(Debug, Serialize)]
struct LiveListResponse {
    live_games: Vec<serde_json::Value>,
    next_cursor: Option<String>,
}

/// 単局 API レスポンスの wire 形状。
#[derive(Debug, Serialize)]
struct GameResponse<'a> {
    game_id: &'a str,
    csa: String,
    /// `games-index/` から取得した meta。MVP では index に entry が無い場合
    /// 404 を返す前提で、ここは常に `Some` だが、JSON 上は serde 既定で field
    /// として出る。
    meta: serde_json::Value,
}

/// 終局済一覧ハンドラ。`games-index/` を 1 ページ list する。
async fn handle_list(req: &Request, env: &Env, url: &Url) -> Result<Response> {
    let entries = match collect_index_page(req, env, url, GAMES_INDEX_PREFIX, "games_index").await?
    {
        CollectOutcome::Page(p) => p,
        CollectOutcome::ErrorResponse(r) => return Ok(r),
    };
    let payload = ListResponse {
        games: entries.entries,
        next_cursor: entries.next_cursor,
    };
    let resp = Response::from_json(&payload)?;
    with_cors(resp, req, env)
}

/// 進行中対局一覧ハンドラ。`live-games-index/` を 1 ページ list する。
///
/// 終局済の `handle_list` と同じ pagination semantics を持ち、prefix と
/// レスポンス key (`live_games`) のみが異なる。
async fn handle_list_live(req: &Request, env: &Env, url: &Url) -> Result<Response> {
    let entries =
        match collect_index_page(req, env, url, LIVE_KEY_PREFIX, "live_games_index").await? {
            CollectOutcome::Page(p) => p,
            CollectOutcome::ErrorResponse(r) => return Ok(r),
        };
    let payload = LiveListResponse {
        live_games: entries.entries,
        next_cursor: entries.next_cursor,
    };
    let resp = Response::from_json(&payload)?;
    with_cors(resp, req, env)
}

/// `collect_index_page` の戻り値。原則 [`Self::Page`] を返すが、early return が
/// 必要なエラー (Origin 拒否 / limit パース失敗 / R2 binding 失敗 / R2 list 失敗)
/// は完成済 `Response` を [`Self::ErrorResponse`] に詰めて返す。
enum CollectOutcome {
    Page(IndexPage),
    ErrorResponse(Response),
}

/// 1 ページぶんの index entry を bytes get → JSON value 化したもの。
struct IndexPage {
    entries: Vec<serde_json::Value>,
    next_cursor: Option<String>,
}

/// `games-index/` / `live-games-index/` 共通の 1 ページ走査ロジック。
///
/// `event_root` はログ event 名の prefix (`games_index` / `live_games_index`)。
/// 失敗時の logfmt event はこれを土台に `<root>_list` / `<root>_get` /
/// `<root>_read` / `<root>_parse` を組み立てる。
async fn collect_index_page(
    req: &Request,
    env: &Env,
    url: &Url,
    prefix: &str,
    event_root: &str,
) -> Result<CollectOutcome> {
    if let Some(blocked) = check_origin(req, env)? {
        return Ok(CollectOutcome::ErrorResponse(blocked));
    }

    // クエリパラメータを 1 度だけ走査して `cursor` / `limit` を取り出す。
    let mut cursor: Option<String> = None;
    let mut limit_raw: Option<String> = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "cursor" => cursor = Some(v.into_owned()),
            "limit" => limit_raw = Some(v.into_owned()),
            _ => {}
        }
    }

    let limit = match limit_raw.as_deref() {
        None => DEFAULT_LIMIT,
        Some(s) => match s.parse::<u32>() {
            Ok(n) if (MIN_LIMIT..=MAX_LIMIT).contains(&n) => n,
            _ => {
                let err = with_cors(
                    Response::error(format!("limit must be {MIN_LIMIT}..={MAX_LIMIT}"), 400)?,
                    req,
                    env,
                )?;
                return Ok(CollectOutcome::ErrorResponse(err));
            }
        },
    };

    let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
        Ok(b) => b,
        Err(e) => {
            console_log_failed("kifu_bucket_binding", &e.to_string());
            let err = with_cors(Response::error("Storage unavailable", 503)?, req, env)?;
            return Ok(CollectOutcome::ErrorResponse(err));
        }
    };

    let mut builder = bucket.list().prefix(prefix).limit(limit);
    if let Some(c) = cursor.as_deref() {
        builder = builder.cursor(c);
    }

    let page = match builder.execute().await {
        Ok(p) => p,
        Err(e) => {
            console_log_failed(&format!("{event_root}_list"), &e.to_string());
            let err = with_cors(Response::error("Storage error", 502)?, req, env)?;
            return Ok(CollectOutcome::ErrorResponse(err));
        }
    };

    let mut entries: Vec<serde_json::Value> = Vec::with_capacity(page.objects().len());
    for obj in page.objects() {
        let key = obj.key();
        // 各 entry を取得 → bytes → JSON value。bytes 経由なのは本文が
        // そのまま `*IndexEntry` の JSON 形式である契約のため。
        let fetched = match bucket.get(&key).execute().await {
            Ok(o) => o,
            Err(e) => {
                console_log_failed(&format!("{event_root}_get"), &format!("key={key} err={e}"));
                continue;
            }
        };
        let Some(fetched) = fetched else {
            // list と get の間に削除されたケース。live entry の場合は終局
            // (delete) と list のレースに該当する。pagination 整合の観点で
            // 落としても問題ない (= live は entry が瞬間的に消えうる契約)。
            continue;
        };
        let Some(body) = fetched.body() else {
            continue;
        };
        let bytes = match body.bytes().await {
            Ok(b) => b,
            Err(e) => {
                console_log_failed(&format!("{event_root}_read"), &format!("key={key} err={e}"));
                continue;
            }
        };
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(v) => entries.push(v),
            Err(e) => {
                console_log_failed(&format!("{event_root}_parse"), &format!("key={key} err={e}"));
                // 1 件壊れても他を返す (best-effort)。
            }
        }
    }

    let next_cursor = if page.truncated() {
        page.cursor()
    } else {
        None
    };
    Ok(CollectOutcome::Page(IndexPage {
        entries,
        next_cursor,
    }))
}

/// 単局ハンドラ。`<game_id>` (URL-decoded path 残部) を受け取り、kifu-by-id を
/// 直接 get する。
async fn handle_get(req: &Request, env: &Env, game_id: &str) -> Result<Response> {
    if let Some(blocked) = check_origin(req, env)? {
        return Ok(blocked);
    }

    let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
        Ok(b) => b,
        Err(e) => {
            console_log_failed("kifu_bucket_binding", &e.to_string());
            return with_cors(Response::error("Storage unavailable", 503)?, req, env);
        }
    };

    let by_id_key = kifu_by_id_object_key(game_id);
    let csa_obj = match bucket.get(&by_id_key).execute().await {
        Ok(o) => o,
        Err(e) => {
            console_log_failed("kifu_by_id_get", &format!("key={by_id_key} err={e}"));
            return with_cors(Response::error("Storage error", 502)?, req, env);
        }
    };
    let Some(csa_obj) = csa_obj else {
        return with_cors(Response::error("Not Found", 404)?, req, env);
    };
    let Some(body) = csa_obj.body() else {
        return with_cors(Response::error("Not Found", 404)?, req, env);
    };
    let csa_text = match body.text().await {
        Ok(t) => t,
        Err(e) => {
            console_log_failed("kifu_by_id_read", &format!("key={by_id_key} err={e}"));
            return with_cors(Response::error("Storage error", 502)?, req, env);
        }
    };

    // meta は `games-index/` を prefix list で 1 件だけ走査して見つける。
    // index key は `<inv_ms>-<game_id>.json` 形式なので game_id 単独では
    // 完全な key を再構築できない (inv_ms が分からない)。MVP では list で
    // 1 件目を見つけ次第 break する単純戦略を採る (per game_id で 1 件のみ
    // 存在する不変条件下では効率より簡潔さを優先)。
    let meta = match find_meta_for(&bucket, game_id).await {
        Ok(Some(m)) => m,
        Ok(None) => {
            // CSA 本文はあるが index に entry が無い (backfill 未実施 or
            // index put 失敗)。MVP 仕様として 404 を返す (本文表示には
            // meta が必須なため)。
            return with_cors(Response::error("Not Found", 404)?, req, env);
        }
        Err(e) => {
            console_log_failed("games_index_lookup", &format!("game_id={game_id} err={e}"));
            return with_cors(Response::error("Storage error", 502)?, req, env);
        }
    };

    let payload = GameResponse {
        game_id,
        csa: csa_text,
        meta,
    };
    let resp = Response::from_json(&payload)?;
    with_cors(resp, req, env)
}

/// `games-index/` を走査して `game_id` に対応する meta を 1 件取得する。
///
/// pagination で `truncated` の間ループするが、ヒット時点で打ち切る。
/// 1 game_id あたり 1 entry の不変条件を活用し、見つかった瞬間返す。
async fn find_meta_for(
    bucket: &worker::Bucket,
    game_id: &str,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let mut cursor: Option<String> = None;
    loop {
        let mut builder = bucket.list().prefix(GAMES_INDEX_PREFIX);
        if let Some(c) = cursor.as_deref() {
            builder = builder.cursor(c);
        }
        let page = builder.execute().await.map_err(|e| e.to_string())?;
        for obj in page.objects() {
            let key = obj.key();
            // key 形式: `games-index/<inv_ms:14>-<game_id>.json`
            // 末尾 `.json` を除去 → 先頭 `games-index/<inv_ms>-` を除去 → game_id
            let Some(stripped) = key.strip_prefix(GAMES_INDEX_PREFIX) else {
                continue;
            };
            let Some(without_ext) = stripped.strip_suffix(".json") else {
                continue;
            };
            // `<inv_ms:14>-<game_id>` から `-` 1 個目以降を game_id として取り出す。
            let Some(dash_idx) = without_ext.find('-') else {
                continue;
            };
            let key_game_id = &without_ext[dash_idx + 1..];
            if key_game_id != game_id {
                continue;
            }
            let fetched = bucket.get(&key).execute().await.map_err(|e| e.to_string())?;
            let Some(fetched) = fetched else {
                continue;
            };
            let Some(body) = fetched.body() else {
                continue;
            };
            let bytes = body.bytes().await.map_err(|e| e.to_string())?;
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            return Ok(Some(value));
        }
        if !page.truncated() {
            return Ok(None);
        }
        cursor = page.cursor();
        if cursor.is_none() {
            return Ok(None);
        }
    }
}

/// CORS / Origin チェック。viewer 配信 API では allowlist の設定が **必須**
/// であり、`WS_ALLOWED_ORIGINS` が空 / 未設定の場合は Origin の有無にかかわらず
/// 403 を返す（ブラウザ・ネイティブ問わず CSRF / 無認可公開を防ぐ）。
///
/// allowlist が非空の場合は [`evaluate`] と同じ semantics で判定する: Origin が
/// 許可リストに含まれていれば通し、含まれない場合は 403。Origin ヘッダ未送信の
/// クライアント (curl 等) は allowlist 非空のときのみ素通しする
/// （[`evaluate`] の仕様）。
fn check_origin(req: &Request, env: &Env) -> Result<Option<Response>> {
    let allow_csv = env
        .var(ConfigKeys::WS_ALLOWED_ORIGINS)
        .ok()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let allow_list = OriginAllowList::from_csv(&allow_csv);
    if allow_list.is_empty() {
        // allowlist 未設定は viewer API では fail-closed。設定漏れを 403 で
        // 顕在化させ、無認可公開を防ぐ。
        return Ok(Some(Response::error("Forbidden Origin", 403)?));
    }
    let origin_header = req.headers().get("Origin")?;
    match evaluate(origin_header.as_deref(), allow_list.iter()) {
        OriginDecision::Allow => Ok(None),
        OriginDecision::NotAllowed => Ok(Some(Response::error("Forbidden Origin", 403)?)),
    }
}

/// 既存レスポンスに CORS ヘッダを乗せ直す。
///
/// `Origin` が許可済みの場合のみ `Access-Control-Allow-Origin` をリクエスト
/// Origin そのものに echo back する。`check_origin` が allowlist 未設定 + Origin 付き
/// を 403 で先に弾いているため、本関数に到達した時点では allowlist は非空かつ
/// Origin はリストに含まれている (もしくは Origin ヘッダなし)。
fn with_cors(mut resp: Response, req: &Request, env: &Env) -> Result<Response> {
    let allow_csv = env
        .var(ConfigKeys::WS_ALLOWED_ORIGINS)
        .ok()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let allow_list = OriginAllowList::from_csv(&allow_csv);
    let origin_header = req.headers().get("Origin")?;
    let allow_origin = match origin_header.as_deref() {
        Some(o) if allow_list.iter().any(|allowed| allowed == o) => Some(o.to_owned()),
        _ => None,
    };
    if let Some(origin) = allow_origin {
        let headers: &mut Headers = resp.headers_mut();
        headers.set("Access-Control-Allow-Origin", &origin)?;
        headers.set("Vary", "Origin")?;
    }
    Ok(resp)
}

/// 失敗ログを logfmt で出す統一窓口。viewer API の経路別 event 名を持たせる。
fn console_log_failed(event: &str, detail: &str) {
    worker::console_log!("[viewer_api] event={event} detail={detail}");
}
