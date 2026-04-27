//! `Lobby` Durable Object — マッチング待機キューと room_id 発番。
//!
//! 1 LobbyDO instance (固定 id `"default"`) が以下を駆動する:
//!
//! 1. **WebSocket Upgrade** (`fetch`): Origin 検査済み・`/ws/lobby` 経由で
//!    渡ってきた upgrade 要求を accept_web_socket して Hibernation 対応にする。
//! 2. **LOGIN_LOBBY** (`websocket_message` / pending): `<handle>+<game_name>+<color>`
//!    形式を [`crate::lobby_protocol::parse_login_lobby`] で分解、queue に追加して
//!    [`crate::lobby_protocol::LobbyQueue::try_pair`] を回す。
//! 3. **マッチ成立**: 対象 2 client に `MATCHED <room_id> <color>` を送出して
//!    各 WS を close する。client は新規 `/ws/<room_id>` に LOGIN し直す。
//! 4. **LOGOUT_LOBBY / WS close**: queue から該当 handle を削除する。
//!
//! queue は **DO 永続 storage を使わず in-memory** で保持する (Hibernation 復帰で
//! 消える)。client は再 LOGIN_LOBBY する想定。
//!
//! 認証は self-claim (`<password>` 値検証なし)、本家 Floodgate と同じ扱い。

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use worker::{
    DurableObject, Env, Error, Request, Response, ResponseBuilder, Result, State, WebSocket,
    WebSocketIncomingMessage, WebSocketPair, console_log, durable_object, wasm_bindgen,
};

use crate::config::ConfigKeys;
use crate::lobby_protocol::{
    LobbyQueue, LoginLobbyError, MatchedEntries, QueueEntry, build_login_incorrect_line,
    build_login_ok_line, build_matched_line, build_room_id, parse_login_lobby,
};
use rshogi_csa_server::types::{Color, ReconnectToken};

/// LobbyDO 内 in-memory queue 上限の既定値 (`LOBBY_QUEUE_SIZE_LIMIT` 未設定時)。
const DEFAULT_LOBBY_QUEUE_SIZE_LIMIT: usize = 100;

/// WebSocket attachment。LobbyDO は対局 DO と異なり 1 種類の player のみ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum LobbyAttachment {
    /// LOGIN_LOBBY 到着前の匿名接続。`websocket_message` で初手は LOGIN_LOBBY を期待する。
    Pending,
    /// queue 登録済みの待機者。
    Queued {
        handle: String,
        game_name: String,
        color: ColorTag,
    },
}

/// `serde::Serialize` を持たない `rshogi_csa_server::types::Color` を attachment 用に
/// JSON 互換形式へ橋渡しする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ColorTag {
    Black,
    White,
}

impl ColorTag {
    fn from_core(c: Color) -> Self {
        match c {
            Color::Black => Self::Black,
            Color::White => Self::White,
        }
    }

    fn to_core(self) -> Color {
        match self {
            Self::Black => Color::Black,
            Self::White => Color::White,
        }
    }
}

/// マッチングロビーの Durable Object。
#[durable_object]
pub struct Lobby {
    state: State,
    env: Env,
    queue: RefCell<LobbyQueue>,
}

impl DurableObject for Lobby {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            queue: RefCell::new(LobbyQueue::new()),
        }
    }

    async fn fetch(&self, _req: Request) -> Result<Response> {
        let pair = WebSocketPair::new()?;
        let server = pair.server;
        self.state.accept_web_socket(&server);

        server
            .serialize_attachment(&LobbyAttachment::Pending)
            .map_err(|e| Error::RustError(format!("serialize_attachment: {e}")))?;
        console_log!("[Lobby] websocket upgrade accepted");

        Ok(ResponseBuilder::new().with_status(101).with_websocket(pair.client).empty())
    }

    async fn websocket_message(&self, ws: WebSocket, msg: WebSocketIncomingMessage) -> Result<()> {
        let raw = match msg {
            WebSocketIncomingMessage::String(s) => s,
            WebSocketIncomingMessage::Binary(_) => return Ok(()),
        };
        let line = raw.trim_end_matches(['\r', '\n']).to_owned();

        let attachment: LobbyAttachment = ws
            .deserialize_attachment()
            .map_err(|e| Error::RustError(format!("deserialize_attachment: {e}")))?
            .unwrap_or(LobbyAttachment::Pending);

        match attachment {
            LobbyAttachment::Pending => self.handle_login_lobby(&ws, &line).await,
            LobbyAttachment::Queued {
                ref handle,
                ref game_name,
                color,
            } => self.handle_queued_line(&ws, handle, game_name, color, &line).await,
        }
    }

    async fn websocket_close(
        &self,
        ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        if let Ok(Some(LobbyAttachment::Queued { handle, .. })) =
            ws.deserialize_attachment::<LobbyAttachment>()
        {
            self.queue.borrow_mut().remove(&handle);
            console_log!(
                "[Lobby] queued client closed: handle={handle} queue_size={}",
                self.queue.borrow().len()
            );
        }
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        // 切断は `websocket_close` 経路で必ず呼ばれるのでここでは何もしない。
        Ok(())
    }
}

impl Lobby {
    fn queue_size_limit(&self) -> usize {
        self.env
            .var(ConfigKeys::LOBBY_QUEUE_SIZE_LIMIT)
            .ok()
            .and_then(|v| v.to_string().parse::<usize>().ok())
            .unwrap_or(DEFAULT_LOBBY_QUEUE_SIZE_LIMIT)
    }

    /// LOGIN_LOBBY 受信時の処理。
    async fn handle_login_lobby(&self, ws: &WebSocket, line: &str) -> Result<()> {
        let req = match parse_login_lobby(line) {
            Ok(r) => r,
            Err(e) => return self.send_login_error(ws, e).await,
        };

        let entry = QueueEntry {
            handle: req.handle.clone(),
            game_name: req.game_name.clone(),
            color: req.color,
        };
        let limit = self.queue_size_limit();
        if !self.queue.borrow_mut().enqueue(entry, limit) {
            send_line(ws, &build_login_incorrect_line("queue_full"))?;
            return Ok(());
        }

        // attachment を Queued に差し替えて待機状態に遷移。
        ws.serialize_attachment(&LobbyAttachment::Queued {
            handle: req.handle.clone(),
            game_name: req.game_name.clone(),
            color: ColorTag::from_core(req.color),
        })
        .map_err(|e| Error::RustError(format!("serialize_attachment: {e}")))?;

        send_line(ws, &build_login_ok_line(&req.handle))?;
        console_log!(
            "[Lobby] LOGIN_LOBBY: handle={} game_name={} color={:?} queue_size={}",
            req.handle,
            req.game_name,
            req.color,
            self.queue.borrow().len()
        );

        // ペアリング判定をその場で実行。成立したら両 WS に MATCHED を送って close。
        if let Some(matched) = self.queue.borrow_mut().try_pair() {
            self.dispatch_match(matched).await?;
        }
        Ok(())
    }

    /// queue 登録後の追加 line (LOGOUT_LOBBY / LOBBY_PONG)。
    async fn handle_queued_line(
        &self,
        ws: &WebSocket,
        handle: &str,
        _game_name: &str,
        _color: ColorTag,
        line: &str,
    ) -> Result<()> {
        match line {
            "LOGOUT_LOBBY" => {
                self.queue.borrow_mut().remove(handle);
                console_log!("[Lobby] LOGOUT_LOBBY: handle={handle}");
                let _ = ws.close(Some(1000), Some("logout"));
                Ok(())
            }
            "LOBBY_PONG" => {
                // keep-alive 応答。現状は受信のみ (PING 送出は未実装)。
                Ok(())
            }
            _ => {
                console_log!("[Lobby] queued client sent unexpected line: {line}");
                Ok(())
            }
        }
    }

    /// マッチ成立時の通知。両 WS 接続を attachment から探し、`MATCHED` を送って close。
    async fn dispatch_match(&self, matched: MatchedEntries) -> Result<()> {
        let room_id = build_room_id(&matched.game_name, &random_128bit_hex());
        let mut sent_black = false;
        let mut sent_white = false;
        for ws in self.state.get_websockets() {
            let att = match ws.deserialize_attachment::<LobbyAttachment>() {
                Ok(Some(a)) => a,
                _ => continue,
            };
            let LobbyAttachment::Queued { handle, color, .. } = att else {
                continue;
            };
            let target_color = if handle == matched.black.handle {
                Color::Black
            } else if handle == matched.white.handle {
                Color::White
            } else {
                continue;
            };
            if target_color != color.to_core() {
                continue;
            }
            send_line(&ws, &build_matched_line(&room_id, target_color))?;
            // close 後に websocket_close ハンドラが queue から remove するが、
            // 既に try_pair で removed なので no-op で安全。
            let _ = ws.close(Some(1000), Some("matched"));
            match target_color {
                Color::Black => sent_black = true,
                Color::White => sent_white = true,
            }
        }
        console_log!(
            "[Lobby] MATCHED dispatched: room_id={} black={} white={} (sent_black={} sent_white={})",
            room_id,
            matched.black.handle,
            matched.white.handle,
            sent_black,
            sent_white,
        );
        Ok(())
    }

    async fn send_login_error(&self, ws: &WebSocket, err: LoginLobbyError) -> Result<()> {
        send_line(ws, &build_login_incorrect_line(err.reason()))?;
        // フォーマット違反は接続維持しても回復経路がないので close する。
        let _ = ws.close(Some(1003), Some("bad_login_lobby"));
        Ok(())
    }
}

/// 末尾改行を付けて 1 行送出する。CSA 行は改行終端が契約なので、`game_room.rs` の
/// `send_line` と挙動を合わせる (本モジュール固有のヘルパとして再定義)。
fn send_line(ws: &WebSocket, line: &str) -> Result<()> {
    let mut out = String::with_capacity(line.len() + 1);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
    ws.send_with_str(&out)
        .map_err(|e| Error::RustError(format!("send_with_str: {e}")))
}

/// 128 bit の hex 文字列 (32 文字) を生成する。
///
/// `rshogi-csa-server::types::ReconnectToken::generate()` の実装を流用する。
/// 内部は `rand::random::<[u8; 16]>()` で、wasm32 (Workers) では `getrandom` の
/// `wasm_js` feature 経由で Web Crypto API (`Crypto.getRandomValues`) から
/// 128 bit エントロピーを得る。`Math.random` の偏りに依存しない経路。
fn random_128bit_hex() -> String {
    ReconnectToken::generate().as_str().to_owned()
}
