//! viewer 配信 HTTP API (`/api/v1/games`) のルーティングと R2 アクセス。
//!
//! v3 設計 (https://github.com/SH11235/rshogi/issues/542 issuecomment-4338088406) に準拠する 3 エンドポイント:
//!
//! - `GET /api/v1/games?cursor=<opaque>&limit=<N>` 一覧 (終局済)
//!   `KIFU_BUCKET.list({prefix: "games-index/", cursor, limit})` を 1 回呼び、
//!   各オブジェクト本文 (= [`crate::games_index::GamesIndexEntry`] の JSON) を
//!   そのまま `games[]` に詰めて返す。`next_cursor` は R2 list の cursor を
//!   opaque 転送する。
//! - `GET /api/v1/games/live?cursor=<opaque>&limit=<N>` 一覧 (進行中)
//!   `KIFU_BUCKET.list({prefix: "live-games-index/", cursor, limit})` を 1 回呼び、
//!   各オブジェクト本文 (= [`crate::live_games_index::LiveGamesIndexEntry`] の JSON)
//!   を `live_games[]` に詰めて返す (https://github.com/SH11235/rshogi/issues/549)。終局済 list と同じ pagination
//!   semantics、`/api/v1/games/<id>` のような単局エンドポイントは進行中対局には
//!   設けない (= viewer 側は live entry を **発見手段** として扱い、行クリック時に
//!   WS spectate 接続で実状態を確認する)。
//! - `GET /api/v1/games/<game_id>` 単局 (終局済)
//!   `kifu-by-id/<encoded_game_id>.csa` を直接 get する。本文 (CSA V2) と
//!   `kifu-by-id/<encoded_game_id>.meta.json` から取得した正準メタ (https://github.com/SH11235/rshogi/issues/551
//!   設計 v3 §12) を合わせて返す。
//!
//! いずれも GameRoom DO を経由せず、Worker 直 fetch のみで完結する (R2 read
//! 1 ホップ)。CORS は staging では `WS_ALLOWED_ORIGINS` をそのまま流用して
//! ramu-shogi origin に絞る (実装の柔軟性で OK)。
//!
//! # access control レビュー必須
//!
//! `try_handle` 配下に新しい `/api/v1/*` エンドポイントを追加する場合は、
//! `check_origin` / `with_cors` の通過と `WS_ALLOWED_ORIGINS` allowlist 体系
//! (https://github.com/SH11235/rshogi/issues/550 で強化予定) に確実に乗ることを必ずレビューする。allowlist 未設定
//! 環境での挙動 (= Origin ヘッダなしで通る) も含めて回帰させない。
//!
//! # Cloudflare Cache API による R2 Class B ops 削減 (https://github.com/SH11235/rshogi/issues/636)
//!
//! 1 リクエストで `bucket.list` (1 RTT) + 各 entry `bucket.get` (最大 N=100 RTT)
//! を消費する N+1 パターンを Cloudflare の `caches.default` で短期キャッシュする。
//! cache key は **request URL 完全一致** (= path + query + method)。同一 cursor +
//! 同一 limit の重複アクセスのみが hit する。
//!
//! 重要な不変条件 (round-2 review で確定):
//! - cache に保存する `Response` は **origin-neutral**。`Access-Control-Allow-Origin`
//!   と `Vary: Origin, ...` を含めず、`Cache-Control: public, s-maxage=<TTL>,
//!   max-age=0, must-revalidate` と `Content-Type: application/json` のみを残す。
//! - cache hit / miss いずれの返却経路でも、最終ステップで [`with_cors`] により
//!   現在 request の Origin に対する ACAO + Vary を被せる。
//! - エラー応答 (400 / 403 / 404 / 502 / 503) は cache しない。`Cache-Control: no-store`
//!   を明示して edge cache が誤って保持しないように倒す。
//! - `ALLOW_VIEWER_API` kill-switch は [`try_handle`] 冒頭で評価されるため、
//!   無効化中は cache lookup に到達しない。既存 cache entry は s-maxage 経過まで
//!   edge に残るが viewer から参照されない (browser は max-age=0 + must-revalidate
//!   により毎回 worker に再検証を投げるため、kill-switch / allowlist 変更は即時に
//!   ブラウザにも反映される)。
//!
//! ## Cache-Control directive 設計
//!
//! 初期実装では `public, max-age=<TTL>` を採用していたが、これだと
//! `caches.default` だけでなくブラウザ・共有 HTTP cache にも `max-age` が効いて
//! しまう。`WS_ALLOWED_ORIGINS` から Origin を除外したり `ALLOW_VIEWER_API` を
//! 落としても、すでに 200 を取得済みのブラウザは worker に再到達せず TTL 満了まで
//! cache を返し続ける (`check_origin` / kill-switch は worker 到達時にしか効かない)。
//!
//! 現行設計: `public, s-maxage=<TTL>, max-age=0, must-revalidate`
//! - `s-maxage=<TTL>`: shared cache (Cloudflare edge `caches.default` を含む) は
//!   TTL 秒保存する。worker `Cache` API も `cache-control` の `max-age` か
//!   `s-maxage` のいずれかが必要 (worker 0.8 の `Cache::put` 契約) で、
//!   `s-maxage` 単独でも `cache.put` は受理される。
//! - `max-age=0, must-revalidate`: ブラウザ・private HTTP cache は毎リクエスト
//!   worker に再検証を要求する (origin = worker)。これにより allowlist / kill-switch
//!   の変更が即時に効く。
//! - 留意点: `s-maxage` は Cloudflare edge 専用ではなく、一般の shared proxy にも
//!   TTL 秒キャッシュを許す。HTTPS 前提なので実害は限定的だが、仕様上はそう。
//!   さらに Cloudflare 側で「Browser Cache TTL」や「Cache Rules」を設定している
//!   と `max-age=0` を上書きされる可能性がある。本 worker と同居する staging /
//!   production の Cache Rules では override しない運用前提。
//!
//! TTL は path 種別ごとに 2 値固定 (測定なし最適化禁止に従い細分化しない):
//! - 終局済 list / live list: **60 秒**
//! - 単局 GET (CSA + meta): **600 秒** (CSA / meta は immutable、edge 退避時の
//!   再 fetch コストを下げるためやや長めに置く)
//!
//! ## live list の 60 秒キャッシュが意味すること
//!
//! `live-games-index/` の整合性モデル (`live_games_index.rs` 冒頭参照) はもとより
//! best-effort eventual で、対局開始 / 終局のいずれでも瞬間整合は保証しない。
//! cache 60 秒を被せたことで以下の 2 点が **最大 60 秒** 遅延しうるが、live list
//! は viewer が候補発見に使うのみで、行クリック時に WS spectate で実状態確認する
//! 設計のため許容範囲とみなす。
//!
//! - 対局開始 → live list に現れるまで (R2 put 反映遅延 + 最大 60 秒 cache stale)
//! - 終局 → live list から消えるまで (R2 delete 反映遅延 + 最大 60 秒 cache stale)

use serde::Serialize;
use worker::{Env, Headers, Method, Request, Response, Result, Url};

use crate::client_kind::normalize_client_kind;
use crate::config::{ConfigKeys, OriginAllowList, is_viewer_api_enabled};
use crate::games_index::KEY_PREFIX as GAMES_INDEX_PREFIX;
use crate::live_games_index::LIVE_KEY_PREFIX;
use crate::origin::{OriginDecision, evaluate};
use crate::x1_paths::{kifu_by_id_meta_key, kifu_by_id_object_key};

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 100;
const MIN_LIMIT: u32 = 1;

/// `/api/v1/games[/...]` 配下のリクエストを判定して該当ハンドラに振り分ける。
///
/// 戻り値 `Some(_)` はマッチしたことを示す。`None` の場合は既存ルーティングに
/// 引き継ぐ (404 までの fallthrough)。
///
/// `OPTIONS` は CORS preflight として viewer API 配下のパスでのみ受理する
/// (https://github.com/SH11235/rshogi/issues/564 設計 v4 §3)。`X-Client` を custom request header として送る
/// クライアント (ramu-shogi web / desktop) のために `Access-Control-Allow-Headers`
/// を返す必要がある。`ALLOW_VIEWER_API` 無効時は GET と同様 404 へフォールスルー
/// し、preflight の段階で kill-switch を効かせる。
pub async fn try_handle(req: &Request, env: &Env) -> Result<Option<Response>> {
    let method = req.method();

    if method == Method::Options {
        let url = req.url()?;
        let path = url.path();
        if !is_viewer_api_path(path) {
            return Ok(None);
        }
        return Ok(Some(handle_options(req, env)?));
    }

    if method != Method::Get {
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
            // viewer API 配下のエラー応答は一律 `Cache-Control: no-store` を
            // 付け、edge cache が誤って 404 を保持しないように倒す
            // (https://github.com/SH11235/rshogi/issues/636 review v1)。with_cors も通して ACAO + Vary を
            // 揃える。
            let resp = no_store_error("Not Found", 404)?;
            return Ok(Some(with_cors(resp, req, env)?));
        }
        return Ok(Some(handle_get(req, env, rest).await?));
    }

    Ok(None)
}

/// viewer 配信 API 配下のパスかどうかを判定する純粋ロジック。
///
/// `OPTIONS` preflight 経路で対象パスをゲートするためにも使用する。
/// `/api/v1/games` (一覧)、`/api/v1/games/live` (live 一覧)、
/// `/api/v1/games/<id>` (単局) のみを true とする。
fn is_viewer_api_path(path: &str) -> bool {
    if path == "/api/v1/games" || path == "/api/v1/games/live" {
        return true;
    }
    if let Some(rest) = path.strip_prefix("/api/v1/games/") {
        return !rest.is_empty() && !rest.contains('/');
    }
    false
}

/// CORS preflight (OPTIONS) を返す。
///
/// `ALLOW_VIEWER_API` 無効時は viewer API 全体が無効なので preflight も 404 を
/// 返す (kill-switch を preflight 段階でも効かせる)。`check_origin` は GET と
/// 同じ経路を踏み、allowlist 未設定 / Origin 非一致は 403。許可された Origin
/// に対しては `Access-Control-Allow-Methods: GET, OPTIONS`、
/// `Access-Control-Allow-Headers: X-Client`、`Access-Control-Max-Age: 86400`
/// を返す。`Access-Control-Allow-Origin` と `Vary` は `with_cors` が共通付与する。
fn handle_options(req: &Request, env: &Env) -> Result<Response> {
    if !is_viewer_api_enabled(env) {
        return Response::error("Not Found", 404);
    }
    if let Some(blocked) = check_origin(req, env)? {
        return Ok(blocked);
    }
    let mut resp = Response::empty()?.with_status(204);
    {
        let headers: &mut Headers = resp.headers_mut();
        headers.set("Access-Control-Allow-Methods", "GET, OPTIONS")?;
        headers.set("Access-Control-Allow-Headers", "X-Client")?;
        headers.set("Access-Control-Max-Age", "86400")?;
    }
    with_cors(resp, req, env)
}

/// `X-Client` ヘッダを運用ログ用の `client_kind` 文字列に正規化する。
///
/// 実装は [`normalize_client_kind`] に委譲する純粋ロジックで、本関数は
/// `worker::Request` から生のヘッダ値を取り出す薄いラッパに留めている
/// (ホスト target でユニットテスト可能にするため)。
fn extract_client_kind(req: &Request) -> String {
    let raw = req.headers().get("X-Client").ok().flatten();
    normalize_client_kind(raw.as_deref())
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
    /// `kifu-by-id/<id>.meta.json` から取得した正準メタ (https://github.com/SH11235/rshogi/issues/551 設計 v3 §12)。
    /// meta 不在時は 404 を返す前提なので、ここは常に `Some` 相当だが JSON 上は
    /// serde 既定で field として出る。
    meta: serde_json::Value,
}

/// cache TTL を path 種別から決める純粋ロジック。
///
/// https://github.com/SH11235/rshogi/issues/636 で導入した `caches.default` per-URL cache の TTL 値固定化。
/// 値は `viewer_api` モジュール doc に書いた契約と一致させ、テストで固定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheableKind {
    /// 終局済 / live いずれの一覧も 60 秒
    List,
    /// 単局 (CSA + meta は immutable) は 600 秒
    SingleGame,
}

impl CacheableKind {
    /// `Cache-Control` ヘッダ値 (200 OK 用) を返す。
    ///
    /// `s-maxage` は Cloudflare edge (`caches.default`) を含む shared cache で
    /// TTL 秒保存させる。`max-age=0, must-revalidate` でブラウザ・private cache は
    /// 毎回 worker に再検証要求を投げ、`check_origin` / `ALLOW_VIEWER_API` の
    /// 変更を即時に反映できる。worker 0.8 `Cache` API は
    /// `max-age` か `s-maxage` のいずれかがあれば `cache.put` を受理する。
    pub(crate) fn cache_control_header(self) -> &'static str {
        match self {
            CacheableKind::List => "public, s-maxage=60, max-age=0, must-revalidate",
            CacheableKind::SingleGame => "public, s-maxage=600, max-age=0, must-revalidate",
        }
    }
}

/// 終局済一覧ハンドラ。`games-index/` を 1 ページ list する。
///
/// https://github.com/SH11235/rshogi/issues/636 の cache 経路:
/// 1. allowlist チェック → 403 origin block の場合は no-store で即返却
/// 2. cache.get (key = request URL) → hit なら ACAO 被せて返す
/// 3. miss なら collect_index_page で R2 list + N×get → origin-neutral Response
///    を生成して cache.put、ACAO 被せて返す
async fn handle_list(req: &Request, env: &Env, url: &Url) -> Result<Response> {
    if let Some(blocked) = check_origin(req, env)? {
        return Ok(blocked);
    }
    let client_kind = extract_client_kind(req);
    let config = ListCacheConfig {
        kind: CacheableKind::List,
        prefix: GAMES_INDEX_PREFIX,
        event_root: "games_index",
        client_kind: &client_kind,
    };
    serve_cached_list(req, env, url, &config, |entries, next_cursor| ListResponse {
        games: entries,
        next_cursor,
    })
    .await
}

/// 進行中対局一覧ハンドラ。`live-games-index/` を 1 ページ list する。
///
/// 終局済の `handle_list` と同じ pagination semantics を持ち、prefix と
/// レスポンス key (`live_games`) のみが異なる。
async fn handle_list_live(req: &Request, env: &Env, url: &Url) -> Result<Response> {
    if let Some(blocked) = check_origin(req, env)? {
        return Ok(blocked);
    }
    let client_kind = extract_client_kind(req);
    let config = ListCacheConfig {
        kind: CacheableKind::List,
        prefix: LIVE_KEY_PREFIX,
        event_root: "live_games_index",
        client_kind: &client_kind,
    };
    serve_cached_list(req, env, url, &config, |entries, next_cursor| LiveListResponse {
        live_games: entries,
        next_cursor,
    })
    .await
}

/// 一覧系 (`/api/v1/games`, `/api/v1/games/live`) 共通の cache + R2 list 経路の
/// 設定値をまとめる。`payload_builder` は generic 型パラメータのため別引数で
/// 渡す (config に入れると `serve_cached_list` 全体に型パラメータが波及する)。
struct ListCacheConfig<'a> {
    /// edge cache TTL (List = 60 秒)。
    kind: CacheableKind,
    /// R2 list の prefix (`games-index/` / `live-games-index/`)。
    prefix: &'a str,
    /// logfmt event 名の root (`games_index` / `live_games_index`)。失敗時は
    /// `<root>_list` / `<root>_get` 等に展開する。`*_cache_get` / `*_cache_put`
    /// にも使う。
    event_root: &'a str,
    /// 呼出元クライアント識別 (`X-Client` 正規化済み)。
    client_kind: &'a str,
}

/// 一覧系 (`/api/v1/games`, `/api/v1/games/live`) 共通の cache + R2 list 経路。
///
/// `payload_builder` は entries / next_cursor から最終 wire payload を組み立てる。
/// 終局済 / 進行中で wire の root key (`games` / `live_games`) のみが異なるため、
/// 関数オブジェクトで切り替える。
async fn serve_cached_list<P, B>(
    req: &Request,
    env: &Env,
    url: &Url,
    config: &ListCacheConfig<'_>,
    payload_builder: B,
) -> Result<Response>
where
    P: Serialize,
    B: FnOnce(Vec<serde_json::Value>, Option<String>) -> P,
{
    let cache_key = req.url()?.to_string();
    let cache_get_event = format!("{}_cache_get", config.event_root);
    if let Some(hit) =
        cache_get_origin_neutral(&cache_key, &cache_get_event, config.client_kind).await
    {
        return with_cors(hit, req, env);
    }

    // miss: R2 list + N×get を実施し、origin-neutral Response を組み立てる。
    let outcome =
        collect_index_page(env, url, config.prefix, config.event_root, config.client_kind).await?;
    match outcome {
        BuildOutcome::Page(page) => {
            let payload = payload_builder(page.entries, page.next_cursor);
            let mut resp = Response::from_json(&payload)?;
            set_cache_control(&mut resp, config.kind.cache_control_header())?;
            cache_put_origin_neutral(&cache_key, &mut resp, config.event_root, config.client_kind)
                .await;
            with_cors(resp, req, env)
        }
        BuildOutcome::ErrorNoStore(resp) => with_cors(resp, req, env),
    }
}

/// `collect_index_page` の戻り値。Page は cache 可能、ErrorNoStore は cache しない。
enum BuildOutcome {
    Page(IndexPage),
    /// 400 / 502 / 503 を `Cache-Control: no-store` 付きで返す。
    ErrorNoStore(Response),
}

/// 1 ページぶんの index entry を bytes get → JSON value 化したもの。
struct IndexPage {
    entries: Vec<serde_json::Value>,
    next_cursor: Option<String>,
}

/// `games-index/` / `live-games-index/` 共通の 1 ページ走査ロジック。
///
/// Origin チェックは呼び出し側 (`handle_list` / `handle_list_live`) で
/// 済ませる契約。本関数は R2 にしかアクセスしない (= cache miss でのみ走る)。
///
/// `event_root` はログ event 名の prefix (`games_index` / `live_games_index`)。
/// 失敗時の logfmt event はこれを土台に `<root>_list` / `<root>_get` /
/// `<root>_read` / `<root>_parse` を組み立てる。
async fn collect_index_page(
    env: &Env,
    url: &Url,
    prefix: &str,
    event_root: &str,
    client_kind: &str,
) -> Result<BuildOutcome> {
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
                let err = no_store_error(format!("limit must be {MIN_LIMIT}..={MAX_LIMIT}"), 400)?;
                return Ok(BuildOutcome::ErrorNoStore(err));
            }
        },
    };

    let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
        Ok(b) => b,
        Err(e) => {
            log_viewer_api_failed("kifu_bucket_binding", client_kind, &e.to_string());
            let err = no_store_error("Storage unavailable", 503)?;
            return Ok(BuildOutcome::ErrorNoStore(err));
        }
    };

    let mut builder = bucket.list().prefix(prefix).limit(limit);
    if let Some(c) = cursor.as_deref() {
        builder = builder.cursor(c);
    }

    let page = match builder.execute().await {
        Ok(p) => p,
        Err(e) => {
            log_viewer_api_failed(&format!("{event_root}_list"), client_kind, &e.to_string());
            let err = no_store_error("Storage error", 502)?;
            return Ok(BuildOutcome::ErrorNoStore(err));
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
                log_viewer_api_failed(
                    &format!("{event_root}_get"),
                    client_kind,
                    &format!("key={key} err={e}"),
                );
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
                log_viewer_api_failed(
                    &format!("{event_root}_read"),
                    client_kind,
                    &format!("key={key} err={e}"),
                );
                continue;
            }
        };
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(v) => entries.push(v),
            Err(e) => {
                log_viewer_api_failed(
                    &format!("{event_root}_parse"),
                    client_kind,
                    &format!("key={key} err={e}"),
                );
                // 1 件壊れても他を返す (best-effort)。
            }
        }
    }

    let next_cursor = if page.truncated() {
        page.cursor()
    } else {
        None
    };
    Ok(BuildOutcome::Page(IndexPage {
        entries,
        next_cursor,
    }))
}

/// 単局ハンドラ。`<game_id>` (URL-decoded path 残部) を受け取り、kifu-by-id を
/// 直接 get する。
///
/// 終局済棋譜は immutable のため Cache TTL を 600 秒で固定する (https://github.com/SH11235/rshogi/issues/636)。
async fn handle_get(req: &Request, env: &Env, game_id: &str) -> Result<Response> {
    if let Some(blocked) = check_origin(req, env)? {
        return Ok(blocked);
    }
    let client_kind = extract_client_kind(req);
    let cache_key = req.url()?.to_string();
    if let Some(hit) =
        cache_get_origin_neutral(&cache_key, "kifu_by_id_cache_get", &client_kind).await
    {
        return with_cors(hit, req, env);
    }

    match build_single_game(env, game_id, &client_kind).await? {
        SingleBuildOutcome::Game { game_id, csa, meta } => {
            let payload = GameResponse {
                game_id: game_id.as_str(),
                csa,
                meta,
            };
            let mut resp = Response::from_json(&payload)?;
            set_cache_control(&mut resp, CacheableKind::SingleGame.cache_control_header())?;
            cache_put_origin_neutral(&cache_key, &mut resp, "kifu_by_id", &client_kind).await;
            with_cors(resp, req, env)
        }
        SingleBuildOutcome::ErrorNoStore(resp) => with_cors(resp, req, env),
    }
}

/// 単局 build の結果。Game は cache する、それ以外は no-store。
enum SingleBuildOutcome {
    Game {
        game_id: String,
        csa: String,
        meta: serde_json::Value,
    },
    ErrorNoStore(Response),
}

async fn build_single_game(
    env: &Env,
    game_id: &str,
    client_kind: &str,
) -> Result<SingleBuildOutcome> {
    let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
        Ok(b) => b,
        Err(e) => {
            log_viewer_api_failed("kifu_bucket_binding", client_kind, &e.to_string());
            return Ok(SingleBuildOutcome::ErrorNoStore(no_store_error(
                "Storage unavailable",
                503,
            )?));
        }
    };

    let by_id_key = kifu_by_id_object_key(game_id);
    let csa_obj = match bucket.get(&by_id_key).execute().await {
        Ok(o) => o,
        Err(e) => {
            log_viewer_api_failed(
                "kifu_by_id_get",
                client_kind,
                &format!("key={by_id_key} err={e}"),
            );
            return Ok(SingleBuildOutcome::ErrorNoStore(no_store_error("Storage error", 502)?));
        }
    };
    let Some(csa_obj) = csa_obj else {
        return Ok(SingleBuildOutcome::ErrorNoStore(no_store_error("Not Found", 404)?));
    };
    let Some(body) = csa_obj.body() else {
        return Ok(SingleBuildOutcome::ErrorNoStore(no_store_error("Not Found", 404)?));
    };
    let csa_text = match body.text().await {
        Ok(t) => t,
        Err(e) => {
            log_viewer_api_failed(
                "kifu_by_id_read",
                client_kind,
                &format!("key={by_id_key} err={e}"),
            );
            return Ok(SingleBuildOutcome::ErrorNoStore(no_store_error("Storage error", 502)?));
        }
    };

    // meta は `kifu-by-id/<id>.meta.json` を直接 get で取得する (https://github.com/SH11235/rshogi/issues/551
    // 設計 v3 §12)。primary meta が常設される契約なので、`games-index/` を
    // prefix list で走査する旧経路 (O(N) コスト) は廃止し、O(1) get に置換した。
    let meta = match find_meta_for(&bucket, game_id).await {
        Ok(Some(m)) => m,
        Ok(None) => {
            // primary meta が無い (backfill 未実施 or meta put 失敗)。本 issue
            // scope では legacy fallback を行わず 404 を返す (Non-goals 設計
            // v3 §10)。
            return Ok(SingleBuildOutcome::ErrorNoStore(no_store_error("Not Found", 404)?));
        }
        Err(e) => {
            log_viewer_api_failed(
                "kifu_by_id_meta_lookup",
                client_kind,
                &format!("game_id={game_id} err={e}"),
            );
            return Ok(SingleBuildOutcome::ErrorNoStore(no_store_error("Storage error", 502)?));
        }
    };

    Ok(SingleBuildOutcome::Game {
        game_id: game_id.to_owned(),
        csa: csa_text,
        meta,
    })
}

/// `kifu-by-id/<id>.meta.json` を直接 get して `game_id` の meta を返す。
///
/// https://github.com/SH11235/rshogi/issues/551 設計 v3 §12 で meta primary 化したことにより、`games-index/`
/// を prefix list で走査する旧経路 (O(N)) は廃止し O(1) get に統一した。
/// meta が存在しない場合は `Ok(None)` を返し、呼び出し側で 404 を返す。
async fn find_meta_for(
    bucket: &worker::Bucket,
    game_id: &str,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let meta_key = kifu_by_id_meta_key(game_id);
    let fetched = bucket.get(&meta_key).execute().await.map_err(|e| e.to_string())?;
    let Some(fetched) = fetched else {
        return Ok(None);
    };
    let Some(body) = fetched.body() else {
        return Ok(None);
    };
    let bytes = body.bytes().await.map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok(Some(value))
}

/// CORS / Origin チェック。viewer 配信 API では allowlist の設定が **必須**
/// であり、`WS_ALLOWED_ORIGINS` が空 / 未設定の場合は Origin の有無にかかわらず
/// 403 を返す（ブラウザ・ネイティブ問わず CSRF / 無認可公開を防ぐ）。
///
/// allowlist が非空の場合は [`evaluate`] と同じ semantics で判定する: Origin が
/// 許可リストに含まれていれば通し、含まれない場合は 403。Origin ヘッダ未送信の
/// クライアント (curl 等) は allowlist 非空のときのみ素通しする
/// （[`evaluate`] の仕様）。
///
/// 403 origin block レスポンスには `Cache-Control: no-store` を付与し、
/// edge / ブラウザ cache が誤って blocked 応答を保持しないようにする。
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
        return Ok(Some(no_store_error("Forbidden Origin", 403)?));
    }
    let origin_header = req.headers().get("Origin")?;
    match evaluate(origin_header.as_deref(), allow_list.iter()) {
        OriginDecision::Allow => Ok(None),
        OriginDecision::NotAllowed => Ok(Some(no_store_error("Forbidden Origin", 403)?)),
    }
}

/// 既存レスポンスに CORS ヘッダを乗せ直す。
///
/// `Origin` が許可済みの場合のみ `Access-Control-Allow-Origin` をリクエスト
/// Origin そのものに echo back する。`check_origin` が allowlist 未設定 + Origin 付き
/// を 403 で先に弾いているため、本関数に到達した時点では allowlist は非空かつ
/// Origin はリストに含まれている (もしくは Origin ヘッダなし)。
///
/// `Vary` は `Origin, Access-Control-Request-Headers` を一括して設定する
/// (https://github.com/SH11235/rshogi/issues/564 設計 v4 §3 review v1 (e))。preflight (OPTIONS) と GET の
/// 双方に適用することで、CDN / ブラウザキャッシュが Origin あるいは
/// preflight で送られた `Access-Control-Request-Headers` を取り違えて
/// レスポンスを共有する事故を防ぐ。
///
/// https://github.com/SH11235/rshogi/issues/636: cache から取り出した origin-neutral な Response も最終的に本関数で
/// ACAO + Vary を被せる。cache に保存する側では ACAO を含めない (origin-neutral
/// 化) ことで Origin ごとに cache が分散しないようにしている。
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
    {
        let headers: &mut Headers = resp.headers_mut();
        if let Some(origin) = allow_origin {
            headers.set("Access-Control-Allow-Origin", &origin)?;
        }
        // GET / OPTIONS いずれの応答にも Vary を付ける。Origin が一致しなかった
        // (= ACAO を付けない) 場合でも、CDN がキャッシュキーから Origin と
        // preflight 用の `Access-Control-Request-Headers` を分離するために
        // 同じヘッダを露出させる。
        headers.set("Vary", "Origin, Access-Control-Request-Headers")?;
    }
    Ok(resp)
}

/// `Cache-Control: no-store` を付けたエラー応答を組み立てる。
///
/// 400 / 403 / 404 / 502 / 503 等、cache してはいけない応答全てに使う共通ヘルパ。
/// `with_cors` で ACAO + Vary を被せる前段の素レスポンスを返す。
fn no_store_error<T: AsRef<str>>(msg: T, status: u16) -> Result<Response> {
    let mut resp = Response::error(msg.as_ref().to_string(), status)?;
    resp.headers_mut().set("Cache-Control", "no-store")?;
    Ok(resp)
}

/// Response に `Cache-Control` ヘッダを上書きで設定する。
fn set_cache_control(resp: &mut Response, value: &str) -> Result<()> {
    resp.headers_mut().set("Cache-Control", value)?;
    Ok(())
}

/// `caches.default` から URL key で hit を取り出す。miss は `None`。
///
/// 取り出した Response は origin-neutral なので、呼び出し側で `with_cors` を
/// 被せて返却する契約。
///
/// `cache.get` が `Err` を返した場合 (Cache API 自体の障害) は `None` を返して
/// miss と同じくフォールバック (R2 から再 fetch) させる。ただし運用上の観測性が
/// 必要なため、`event` (`<root>_cache_get`) と `client_kind` 付きの logfmt を
/// `log_viewer_api_failed` で残す。サイレント抑制すると staging /
/// 本番で Cache API が一切機能していない事象に気付けない。
async fn cache_get_origin_neutral(
    cache_key: &str,
    event: &str,
    client_kind: &str,
) -> Option<Response> {
    let cache = worker::Cache::default();
    match cache.get(cache_key.to_string(), true).await {
        Ok(Some(resp)) => Some(resp),
        Ok(None) => None,
        Err(e) => {
            log_viewer_api_failed(event, client_kind, &e.to_string());
            None
        }
    }
}

/// origin-neutral な Response を `caches.default` に put する。
///
/// `Response::cloned()` で put 用と返却用に分け、put が失敗しても元 Response は
/// 返却できるようにする (best-effort、log のみ残す)。
async fn cache_put_origin_neutral(
    cache_key: &str,
    resp: &mut Response,
    event_root: &str,
    client_kind: &str,
) {
    let put_response = match resp.cloned() {
        Ok(c) => c,
        Err(e) => {
            log_viewer_api_failed(
                &format!("{event_root}_cache_clone"),
                client_kind,
                &e.to_string(),
            );
            return;
        }
    };
    let cache = worker::Cache::default();
    if let Err(e) = cache.put(cache_key.to_string(), put_response).await {
        // best-effort: cache.put に失敗しても元応答は返せる。log のみ残す。
        log_viewer_api_failed(&format!("{event_root}_cache_put"), client_kind, &e.to_string());
    }
}

/// 失敗ログを構造化 JSON で出す統一窓口。viewer API の経路別 event 名と、
/// 呼出側クライアントを特定する `client_kind` を持たせる
/// (https://github.com/SH11235/rshogi/issues/564 設計 v4 §3)。`client_kind` は [`normalize_client_kind`] により
/// `[a-z0-9-]{1,64}` に正規化済みの ASCII 文字列、または `unknown` / `invalid`。
fn log_viewer_api_failed(event: &str, client_kind: &str, detail: &str) {
    crate::structured_log!(
        event: event,
        component: "viewer_api",
        client_kind: client_kind,
        detail: detail,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_control_header_for_list_kind() {
        // edge は s-maxage で TTL 秒保存し、ブラウザ・private cache は
        // max-age=0 + must-revalidate で毎回 worker に再検証要求を投げる。
        assert_eq!(
            CacheableKind::List.cache_control_header(),
            "public, s-maxage=60, max-age=0, must-revalidate"
        );
    }

    #[test]
    fn cache_control_header_for_single_kind() {
        assert_eq!(
            CacheableKind::SingleGame.cache_control_header(),
            "public, s-maxage=600, max-age=0, must-revalidate"
        );
    }

    #[test]
    fn cache_control_headers_force_browser_revalidation() {
        // 全 cacheable 経路でブラウザ側の再検証 directive (`max-age=0` と
        // `must-revalidate`) が抜けていないことを確定させる。`s-maxage` だけが
        // 効いて `max-age` を落としても browser default の heuristic が走り
        // うるため、明示的に検査する。
        for kind in [CacheableKind::List, CacheableKind::SingleGame] {
            let header = kind.cache_control_header();
            assert!(header.contains("max-age=0"), "{kind:?} missing max-age=0: {header}");
            assert!(
                header.contains("must-revalidate"),
                "{kind:?} missing must-revalidate: {header}"
            );
            assert!(header.contains("s-maxage="), "{kind:?} missing s-maxage: {header}");
        }
    }

    #[test]
    fn is_viewer_api_path_accepts_root_paths() {
        assert!(is_viewer_api_path("/api/v1/games"));
        assert!(is_viewer_api_path("/api/v1/games/live"));
        assert!(is_viewer_api_path("/api/v1/games/abc-123"));
    }

    #[test]
    fn is_viewer_api_path_rejects_extra_segments_and_trailing_slash() {
        assert!(!is_viewer_api_path("/api/v1/games/"));
        assert!(!is_viewer_api_path("/api/v1/games/x/y"));
        assert!(!is_viewer_api_path("/api/v2/games"));
    }
}
