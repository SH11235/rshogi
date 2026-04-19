//! `GameRoom` Durable Object。
//!
//! 1 部屋（room_id）につき 1 DO インスタンスが対応し、その SQLite ストレージに
//! 対局状態を永続化する（tasks.md §9.1, Req 10.1/10.5）。本モジュールでは
//! Phase 2-9.1 のスコープとして:
//!
//! - WebSocket Upgrade 受理と Hibernation 化 (`state.accept_web_socket`) による
//!   アイドル時コード実行ゼロ化 (Req 10.2) の下地を整える。
//! - DO 再構築時の初期化として SQLite スキーマを用意する
//!   (§9.4 で拡張するため現段階では KV 互換テーブルのみ先置き)。
//! - 受信行は一旦パイプライン確認用の `##ACK` 応答にとどめる。
//!   `CoreRoom::handle_line` への結線は §9.4 の状態永続化と一緒に行う。
//!
//! wasm32-unknown-unknown でのみビルドされるモジュールなので、ホスト側の単体
//! テスト対象からは除外される。検証は `wrangler dev` (Miniflare) 以降で行う。

use worker::{
    DurableObject, Env, Error, Request, Response, ResponseBuilder, Result, State, WebSocket,
    WebSocketIncomingMessage, WebSocketPair, console_log, durable_object, wasm_bindgen,
};

use crate::phase_gate;

/// DO 初期化時に流す SQLite スキーマ。
///
/// §9.4 で CoreRoom 永続化テーブル（`game`, `moves` など）を追加する。ここでは
/// 初回 migration として最小の KV テーブルだけを先置きし、スキーマ更新時の
/// `CREATE TABLE IF NOT EXISTS` 冪等性を確認する土台とする。
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// 1 対局分の Durable Object。
#[durable_object]
pub struct GameRoom {
    state: State,
}

impl DurableObject for GameRoom {
    fn new(state: State, _: Env) -> Self {
        // スキーマ初期化は冪等。DO 再構築のたびに呼ばれても問題ない。
        let sql = state.storage().sql();
        sql.exec(SCHEMA_SQL, None).expect("failed to initialize DO schema");
        Self { state }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path();

        // `/ws/*` 以外の DO 直叩きは現状想定しない（router 側で弾く前提）。
        // 防御的に 426 を返し、仕様変更時に検知できるようにする。
        if !path.starts_with("/ws/") {
            return Response::error("Upgrade required", 426);
        }

        let pair = WebSocketPair::new()?;
        let server = pair.server;

        // Hibernation API に登録することで、アイドル区間は isolate が凍結されても
        // 接続が維持される (Req 10.2)。`server.accept()` を呼ばない点に注意:
        // accept_web_socket 経路は runtime 側が accept を代行する。
        self.state.accept_web_socket(&server);

        console_log!("[GameRoom] websocket upgrade accepted ({})", phase_gate::PhaseGate::label());

        Ok(ResponseBuilder::new().with_status(101).with_websocket(pair.client).empty())
    }

    async fn websocket_message(&self, ws: WebSocket, msg: WebSocketIncomingMessage) -> Result<()> {
        let line = match msg {
            WebSocketIncomingMessage::String(s) => s,
            // バイナリは CSA プロトコルに無いので silently drop（後続 Phase で
            // エラー応答にするかは仕様確認後に決める）。
            WebSocketIncomingMessage::Binary(_) => return Ok(()),
        };

        // §9.4 で `CoreRoom::handle_line` に結線する予定。現段階では配線確認用の
        // `##ACK` を折り返してパイプラインの疎通を E2E で観測可能にする。
        let reply = format!("##ACK {}", line.trim_end_matches(['\r', '\n']));
        ws.send_with_str(&reply)
            .map_err(|e| Error::RustError(format!("send_with_str: {e}")))
    }

    async fn websocket_close(
        &self,
        _ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        // §9.4 で対局中断ハンドリングを入れる。Phase 1 の `force_abnormal` 相当。
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }

    async fn alarm(&self) -> Result<Response> {
        // §9.5 で時間切れ判定を実装する。Phase 2-9.1 時点では未配線。
        Response::ok("noop")
    }
}
