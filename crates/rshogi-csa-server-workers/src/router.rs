//! fetch イベントのルーティング。
//!
//! `#[event(fetch)]` から 1 本だけ呼ばれる薄いディスパッチャ。
//! - `GET /ws/:room_id` → Origin 検査後、`room_id` を `id_from_name` で
//!   決定論的に解決した Durable Object へ Upgrade 要求を転送する。
//! - `GET /` と `GET /health` → サーバ識別と deploy 元 commit sha を JSON で返す
//!   簡易ヘルスチェック。https://github.com/SH11235/rshogi/issues/639 の rollback drift detection が `deployed_sha`
//!   を main HEAD と突合する基準にするため、JSON schema の安定性を保つこと。
//! - 他は 404。

use worker::{Env, Method, Request, Response, Result};

use crate::config::{ConfigKeys, OriginAllowList, is_viewer_api_enabled};
use crate::origin::{OriginDecision, evaluate};
use crate::rate_limit::{
    RateLimitKind, build_missing_ip_response, build_ws_upgrade_rate_limited_response,
    check_and_consume_via_do, extract_client_ip, resolve_thresholds_from_env,
};
use crate::viewer_api;
use crate::ws_route::{WsRoute, parse_ws_route};

/// `/health` レスポンスで `DEPLOYED_SHA` 未設定時に返す既定値。local dev や
/// `wrangler deploy --var DEPLOYED_SHA:` 引数を付けない経路で観測される。
const HEALTH_UNKNOWN_SHA: &str = "unknown";

/// `#[event(fetch)]` から委譲されるディスパッチ。
pub async fn handle_fetch(req: Request, env: Env) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_owned();
    let method = req.method();

    if method == Method::Get && (path == "/" || path == "/health") {
        return health_response(&env);
    }

    // viewer 配信 API (`/api/v1/games[/...]`) は GameRoom DO を経由せず
    // R2 直 fetch のみで完結する。本ルートに該当しない場合のみ既存の
    // WebSocket ルーティングへ落ちる。
    if let Some(resp) = viewer_api::try_handle(&req, &env).await? {
        return Ok(resp);
    }

    if method == Method::Get && path == "/ws/lobby" {
        return forward_ws_to_lobby(req, env).await;
    }

    if method == Method::Get && path.starts_with("/ws/") {
        let Some(route) = parse_ws_route(&path) else {
            return Response::error("Invalid room_id", 400);
        };
        return forward_ws_to_room(req, env, &path, &route).await;
    }

    Response::error("Not Found", 404)
}

/// `/ws/lobby` を Origin 検査し、許可された場合のみ Lobby DO に転送する。
/// Lobby は 1 instance 固定 (`id_from_name("default")`) でアプリ全体のマッチング
/// 待機キューを保持する。
async fn forward_ws_to_lobby(req: Request, env: Env) -> Result<Response> {
    let allow_csv = env
        .var(ConfigKeys::WS_ALLOWED_ORIGINS)
        .ok()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let allow_list = OriginAllowList::from_csv(&allow_csv);
    let origin_header = req.headers().get("Origin")?;
    match evaluate(origin_header.as_deref(), allow_list.iter()) {
        OriginDecision::Allow => {}
        OriginDecision::NotAllowed => return Response::error("Forbidden Origin", 403),
    }

    let upgrade = req.headers().get("Upgrade")?.unwrap_or_default().to_ascii_lowercase();
    if upgrade != "websocket" {
        return Response::error("Upgrade required", 426);
    }

    let namespace = env.durable_object(ConfigKeys::LOBBY_BINDING)?;
    let stub = namespace.id_from_name("default")?.get_stub()?;

    let forward_url = "https://do.internal/ws/lobby";
    let mut fwd = Request::new(forward_url, Method::Get)?;
    let fwd_headers = fwd.headers_mut()?;
    for name in [
        "upgrade",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
        // Rate limit (issue #622 PR3a): LobbyDO 側で LOGIN_LOBBY / CHALLENGE_LOBBY
        // 受信時に per-IP 限流するため、CF-Connecting-IP を DO に渡す。
        // `accept_web_socket` は Worker fetch context を keep しないので、本ヘッダ
        // を attachment に保存して `websocket_message` から参照する設計。
        // 本リストに名前を載せていない他ヘッダは意図的に削ぎ落とすコントラクト
        // (DO 側で信頼できるのは `Upgrade` / `Sec-WebSocket-*` / `CF-Connecting-IP`
        // のみ) を維持する。
        "cf-connecting-ip",
    ] {
        if let Some(v) = req.headers().get(name)? {
            let _ = fwd_headers.set(name, &v);
        }
    }

    stub.fetch_with_request(fwd).await
}

/// `/ws/:room_id` を Origin 検査し、許可された場合のみ GameRoom DO に転送する。
///
/// Spectator 経路 (`/ws/<id>/spectate`) は viewer 配信 API と同列の access
/// control を適用する: `ALLOW_VIEWER_API` 無効 → 404、allowlist 未設定 → 403。
/// Player 経路 (`/ws/<room_id>`) は対局者ネイティブクライアントが Origin を
/// 送らない経路を温存する必要があるため、既存の Origin 検査 semantics を
/// 維持する（allowlist 未設定 + Origin 付きのみ 403）。
async fn forward_ws_to_room(
    req: Request,
    env: Env,
    request_path: &str,
    route: &WsRoute,
) -> Result<Response> {
    // Spectator 経路では viewer API gate を通す。無効化されている場合は
    // 404 を返し、`/api/v1/games*` と挙動を揃える。
    if route.is_spectator() && !is_viewer_api_enabled(&env) {
        return Response::error("Not Found", 404);
    }

    // Origin 許可リストは `[vars] WS_ALLOWED_ORIGINS = "<csv>"` から取得する。
    // Player 経路: 値が空や未設定なら `OriginAllowList` は空 = ブラウザ経由
    // （Origin 付き）は全拒否。ネイティブ CSA クライアント等 Origin ヘッダを
    // 送らない経路は素通し（[`evaluate`] の仕様）。
    // Spectator 経路: allowlist 未設定は fail-closed で 403（無認可公開を防ぐ）。
    let allow_csv = env
        .var(ConfigKeys::WS_ALLOWED_ORIGINS)
        .ok()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let allow_list = OriginAllowList::from_csv(&allow_csv);

    if route.is_spectator() && allow_list.is_empty() {
        return Response::error("Forbidden Origin", 403);
    }

    let origin_header = req.headers().get("Origin")?;
    match evaluate(origin_header.as_deref(), allow_list.iter()) {
        OriginDecision::Allow => {}
        OriginDecision::NotAllowed => return Response::error("Forbidden Origin", 403),
    }

    // WebSocket Upgrade であることを確認。Upgrade 以外の GET は 426 で弾く。
    let upgrade = req.headers().get("Upgrade")?.unwrap_or_default().to_ascii_lowercase();
    if upgrade != "websocket" {
        return Response::error("Upgrade required", 426);
    }

    // Rate limit (issue #622 PR3a): WS upgrade flood / GameRoom DO 起動 flood の
    // 抑制。`parse_ws_route` 通過後 + Origin / Upgrade 検査済み + DO `id_from_name`
    // 解決前にチェックする (`docs/csa-server/rate_limit_design.md` §4.4 hook 順序)。
    //
    // `parse_ws_route` で reject される `%%room_id` 不正値は本パスに到達しない
    // ため、bucket への counter 増加経路を踏まない (= claude review #1 の
    // 「不正値で counter 増加してはいけない」契約を満たす)。
    //
    // 二段階チェック:
    // 1. `WsRoomUpgradePerIp` (looser cap, 既定 60/分) — player + spectator 共有
    // 2. `RoomCreatePerIp` (tighter cap, 既定 20/分) — player route 固有 (新規
    //    GameRoom DO 起動の代理指標)
    //
    // CF-Connecting-IP 欠落時は **fail-closed** で 503 + Retry-After 短時間 (10 秒)。
    let Some(client_ip) = extract_client_ip(&req) else {
        crate::structured_log!(
            event: "rate_limit_missing_cf_ip",
            component: "router",
            path: request_path,
        );
        return build_missing_ip_response();
    };
    let thresholds = resolve_thresholds_from_env(&env);

    // Step 1: WS upgrade per-IP (player + spectator 共通)。
    let upgrade_decision = check_and_consume_via_do(
        &env,
        RateLimitKind::WsRoomUpgradePerIp,
        &client_ip,
        thresholds.ws_room_upgrade_per_ip,
    )
    .await?;
    if !upgrade_decision.allowed {
        crate::structured_log!(
            event: "rate_limit_denied",
            component: "router",
            kind: "ws_room_upgrade_per_ip",
            path: request_path,
            ip: client_ip,
            retry_after_sec: upgrade_decision.retry_after_sec,
        );
        return build_ws_upgrade_rate_limited_response(upgrade_decision.retry_after_sec);
    }

    // Step 2: player route のみ room create cap (tighter)。spectator route では
    // 既存の viewer API gate (上で `is_viewer_api_enabled` 通過済) が別軸で抑える。
    if !route.is_spectator() {
        let create_decision = check_and_consume_via_do(
            &env,
            RateLimitKind::RoomCreatePerIp,
            &client_ip,
            thresholds.room_create_per_ip,
        )
        .await?;
        if !create_decision.allowed {
            crate::structured_log!(
                event: "rate_limit_denied",
                component: "router",
                kind: "room_create_per_ip",
                path: request_path,
                ip: client_ip,
                retry_after_sec: create_decision.retry_after_sec,
            );
            return build_ws_upgrade_rate_limited_response(create_decision.retry_after_sec);
        }
    }

    // room_id から決定論的に DO インスタンスを解決する。`id_from_name` は
    // 文字列ハッシュを ID に写像するため、同じ room_id は常に同一 DO に到達する。
    let namespace = env.durable_object(ConfigKeys::GAME_ROOM_BINDING)?;
    let stub = namespace.id_from_name(route.room_id())?.get_stub()?;

    // DO 側 fetch は完全な URL を要求する仕様。転送用のダミー host を立て、
    // path をそのまま DO 側へ引き継ぐ（`/spectate` を含む route 判定に使う）。
    let forward_url = format!("https://do.internal{request_path}");
    let mut fwd = Request::new(&forward_url, Method::Get)?;
    let fwd_headers = fwd.headers_mut()?;

    // WebSocket ハンドシェイクに必要なヘッダのみを転送する。その他のヘッダは
    // 意図的に削ぎ落とし、DO 側で信頼できるのは `Upgrade` と `Sec-WebSocket-*`
    // に限るという静的コントラクトにする。
    for name in [
        "upgrade",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
    ] {
        if let Some(v) = req.headers().get(name)? {
            let _ = fwd_headers.set(name, &v);
        }
    }

    stub.fetch_with_request(fwd).await
}

/// `/health` および `/` で返す JSON ペイロード。
///
/// `deployed_sha` は CI deploy 時に `wrangler deploy --var DEPLOYED_SHA:<sha>` で
/// 注入された commit sha (= `.github/workflows/deploy-workers.yml` の `push.paths`
/// にマッチする main 上の最新 commit)。https://github.com/SH11235/rshogi/issues/639 の drift detection workflow が
/// 本フィールドを `git log -1 --format=%H -- <paths>` の結果と突合して、Cloudflare
/// 側に残った rollback 後の旧 version を検出する。
///
/// 未設定時 (`HEALTH_UNKNOWN_SHA = "unknown"`) は drift workflow 側で「schema
/// 不正 / 古い deploy」として警告に倒す（`null` ではなく文字列を返すことで JSON
/// schema を不変に保つ）。
#[derive(serde::Serialize)]
struct HealthPayload<'a> {
    name: &'a str,
    version: &'a str,
    deployed_sha: &'a str,
}

/// `/health` `GET` レスポンスを生成する。`DEPLOYED_SHA` 未設定や空文字なら
/// [`HEALTH_UNKNOWN_SHA`] を返す。
fn health_response(env: &Env) -> Result<Response> {
    let deployed_sha_owned = env.var(ConfigKeys::DEPLOYED_SHA).ok().map(|v| v.to_string());
    let deployed_sha = deployed_sha_owned
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(HEALTH_UNKNOWN_SHA);
    let payload = HealthPayload {
        name: "rshogi-csa-server-workers",
        version: env!("CARGO_PKG_VERSION"),
        deployed_sha,
    };
    Response::from_json(&payload)
}
