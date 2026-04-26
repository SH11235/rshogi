//! fetch イベントのルーティング。
//!
//! `#[event(fetch)]` から 1 本だけ呼ばれる薄いディスパッチャ。
//! - `GET /ws/:room_id` → Origin 検査後、`room_id` を `id_from_name` で
//!   決定論的に解決した Durable Object へ Upgrade 要求を転送する。
//! - `GET /` と `GET /health` → サーバ識別を返す簡易ヘルスチェック。
//! - 他は 404。

use worker::{Env, Method, Request, Response, Result};

use crate::config::{ConfigKeys, OriginAllowList};
use crate::origin::{OriginDecision, evaluate};
use crate::ws_route::parse_ws_route;

/// `#[event(fetch)]` から委譲されるディスパッチ。
pub async fn handle_fetch(req: Request, env: Env) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_owned();
    let method = req.method();

    if method == Method::Get && (path == "/" || path == "/health") {
        return Response::ok(format!("rshogi-csa-server-workers v{}", env!("CARGO_PKG_VERSION")));
    }

    if method == Method::Get && path.starts_with("/ws/") {
        let Some(route) = parse_ws_route(&path) else {
            return Response::error("Invalid room_id", 400);
        };
        return forward_ws_to_room(req, env, &path, route.room_id()).await;
    }

    Response::error("Not Found", 404)
}

/// `/ws/:room_id` を Origin 検査し、許可された場合のみ GameRoom DO に転送する。
async fn forward_ws_to_room(
    req: Request,
    env: Env,
    request_path: &str,
    room_id: &str,
) -> Result<Response> {
    // Origin 許可リストは `[vars] CORS_ORIGINS = "<csv>"` から取得する。
    // 値が空や未設定なら `OriginAllowList` は空 = 全拒否（安全側）。
    let allow_csv = env
        .var(ConfigKeys::CORS_ORIGINS)
        .ok()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let allow_list = OriginAllowList::from_csv(&allow_csv);

    let origin_header = req.headers().get("Origin")?;
    match evaluate(origin_header.as_deref(), allow_list.iter()) {
        OriginDecision::Allow => {}
        OriginDecision::Missing => return Response::error("Missing Origin", 403),
        OriginDecision::NotAllowed => return Response::error("Forbidden Origin", 403),
    }

    // WebSocket Upgrade であることを確認。Upgrade 以外の GET は 426 で弾く。
    let upgrade = req.headers().get("Upgrade")?.unwrap_or_default().to_ascii_lowercase();
    if upgrade != "websocket" {
        return Response::error("Upgrade required", 426);
    }

    // room_id から決定論的に DO インスタンスを解決する。`id_from_name` は
    // 文字列ハッシュを ID に写像するため、同じ room_id は常に同一 DO に到達する。
    let namespace = env.durable_object(ConfigKeys::GAME_ROOM_BINDING)?;
    let stub = namespace.id_from_name(room_id)?.get_stub()?;

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
