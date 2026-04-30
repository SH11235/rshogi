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
//!
//! # 私的対局 (Issue #582) — 本 PR スコープ
//!
//! 本 PR では以下のみ実装する。両者揃った後の対局起動経路 (consume → GameRoom
//! DO 起動 + clock_spec / initial_sfen バトンパス) は Issue #582 follow-up
//! integration の後半スコープに分割する。
//!
//! - `CHALLENGE_LOBBY <inviter> <opponent> <color> <clock_preset> [<sfen>]` 受理
//!   と token 発行 (`CHALLENGE_LOBBY:OK <token> <ttl_sec>` 応答)
//! - `LOGIN_LOBBY <handle>+private-<token>+free <password>` の認識と attachment
//!   登録 (`LOGIN_LOBBY:<handle> OK pending_match_dispatch_pending` 応答)
//! - [`rshogi_csa_server::matching::challenge::ChallengeRegistry`] の
//!   DO storage 永続化 (cold start 復元 + `purge_expired` 後再保存)
//! - DO Alarm による TTL purge と保留中 WS への切断信号送出

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use worker::{
    Date, DurableObject, Env, Error, Request, Response, ResponseBuilder, Result, State, WebSocket,
    WebSocketIncomingMessage, WebSocketPair, console_log, durable_object, wasm_bindgen,
};

use crate::config::{ConfigKeys, parse_challenge_ttl_duration, parse_clock_presets};
use crate::lobby_protocol::{
    ChallengeLobbyError, LobbyQueue, LoginLobbyError, LoginLobbyPrivateError,
    LoginLobbyPrivateRequest, MatchedEntries, QueueEntry, build_challenge_incorrect_line,
    build_challenge_ok_line, build_login_incorrect_line, build_login_ok_line, build_matched_line,
    build_room_id, is_private_login_handle, parse_challenge_lobby, parse_login_lobby,
    parse_login_lobby_with_free,
};
use rshogi_csa_server::ClockSpec;
use rshogi_csa_server::matching::challenge::{
    ChallengeEntry, ChallengeRegistry, ChallengeToken, IssueError,
};
use rshogi_csa_server::types::{Color, PlayerName, ReconnectToken};

/// LobbyDO 内 in-memory queue 上限の既定値 (`LOBBY_QUEUE_SIZE_LIMIT` 未設定時)。
const DEFAULT_LOBBY_QUEUE_SIZE_LIMIT: usize = 100;

/// `ChallengeRegistry` を DO storage に書き出すキー。
///
/// 同 LobbyDO 内で他に永続化対象がない (queue は volatile) ため、key は単一で
/// 衝突の心配はない。Cold start 時に必ず `state.storage().get` でこのキーを
/// 引き、既存値を `purge_expired` してから保持する契約。
const KEY_CHALLENGE_REGISTRY: &str = "challenge_registry";

/// 私的対局 attachment ごとに割り振る一意 id を採番するためのキー。
/// `state.storage()` の単純なカウンタとして単調増加させ、`pending_ws_attachment_ids`
/// に積む際に handle 単位の race ([`ChallengeRegistry::unmark_ws_logged_in`]
/// と対称) を防ぐ。Cloudflare DO は同 instance に対する fetch / alarm /
/// websocket_* を単一スレッドで逐次処理するため、本カウンタは追加 lock 不要。
const KEY_NEXT_ATTACHMENT_ID: &str = "challenge_next_attachment_id";

/// Alarm 発火時刻に上乗せする安全側マージン (ms)。Cloudflare Alarm のジッタを
/// 吸収し、`Date::now()` 取得 → `set_alarm` 反映までの遅延中に earliest entry
/// が直前の `now_ms` を割り込むことを防ぐ。`game_room.rs::ALARM_SAFETY_MS` と
/// 同名同値で揃えてある。
const CHALLENGE_ALARM_SAFETY_MS: u64 = 200;

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
    /// 私的対局 (`LOGIN_LOBBY <handle>+private-<token>+free`) で先行 LOGIN
    /// 済の待機者。両者揃った時点での GameRoom DO 起動経路は次 PR に分割する
    /// ため、本 attachment は `pending_ws_attachment_ids` への登録を保持する
    /// だけの暫定状態にとどまる。WS close 時に attachment id 単位で
    /// `unmark_ws_logged_in` を呼んで stale handle race を回避する。
    PrivatePending {
        /// challenge token の hex 文字列 (24 文字)。`ChallengeToken::from_raw`
        /// でラップして `ChallengeRegistry` 操作に使う。
        token: String,
        /// LOGIN 申告された handle (= `ChallengeEntry::inviter` または
        /// `opponent` のいずれかと一致済)。
        handle: String,
        /// 採番済の attachment id。`ChallengeEntry::pending_ws_attachment_ids`
        /// の値と一致する場合のみ unmark する契約 ([`ChallengeRegistry`]
        /// 仕様)。
        attachment_id: String,
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
    /// `CLOCK_PRESETS` 環境変数を 1 度だけパースして保持するキャッシュ。
    /// 起動中に env vars は不変のため、初回 LOGIN_LOBBY のたびに JSON パースを
    /// 走らせる無駄を避ける。空 HashMap = preset 未宣言 = strict mode 無効。
    clock_presets: OnceCell<HashMap<String, ClockSpec>>,
}

impl DurableObject for Lobby {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            queue: RefCell::new(LobbyQueue::new()),
            clock_presets: OnceCell::new(),
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
            LobbyAttachment::Pending => self.dispatch_pending_line(&ws, &line).await,
            LobbyAttachment::Queued {
                ref handle,
                ref game_name,
                color,
            } => self.handle_queued_line(&ws, handle, game_name, color, &line).await,
            LobbyAttachment::PrivatePending {
                ref token,
                ref handle,
                ref attachment_id,
            } => self.handle_private_pending_line(&ws, token, handle, attachment_id, &line).await,
        }
    }

    async fn websocket_close(
        &self,
        ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        match ws.deserialize_attachment::<LobbyAttachment>() {
            Ok(Some(LobbyAttachment::Queued { handle, .. })) => {
                self.queue.borrow_mut().remove(&handle);
                console_log!(
                    "[Lobby] queued client closed: handle={handle} queue_size={}",
                    self.queue.borrow().len()
                );
            }
            Ok(Some(LobbyAttachment::PrivatePending {
                token,
                handle,
                attachment_id,
            })) => {
                self.handle_private_pending_close(&token, &handle, &attachment_id).await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        // 切断は `websocket_close` 経路で必ず呼ばれるのでここでは何もしない。
        Ok(())
    }

    async fn alarm(&self) -> Result<Response> {
        self.handle_challenge_alarm().await?;
        Response::ok("challenge alarm handled")
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

    /// `CLOCK_PRESETS` 環境変数をパースして得たマップを返す（OnceCell キャッシュ）。
    /// パース失敗時は空 HashMap として扱い、`console_log!` で警告を残して strict
    /// mode を無効化する（preset 設定不正で全 LOGIN がブロックされる事態を避ける
    /// 安全側挙動。`CLOCK_PRESETS` 値そのものは `parse_clock_presets` の起動時
    /// テストでカバーする）。
    fn clock_presets(&self) -> &HashMap<String, ClockSpec> {
        self.clock_presets.get_or_init(|| {
            let raw = self.env.var(ConfigKeys::CLOCK_PRESETS).ok().map(|v| v.to_string());
            match parse_clock_presets(raw.as_deref()) {
                Ok(map) => map,
                Err(e) => {
                    console_log!("[Lobby] event=invalid_clock_presets err={e}");
                    HashMap::new()
                }
            }
        })
    }

    /// `CHALLENGE_TTL_SEC` env を `Duration` に解決する (未設定時は 3600 秒の
    /// 既定値、不正値もフォールバック)。`config::parse_challenge_ttl_duration`
    /// の薄いラッパで、Workers ランタイム側の `env.var` 読み取りを集約する。
    fn challenge_ttl(&self) -> Duration {
        let raw = self.env.var(ConfigKeys::CHALLENGE_TTL_SEC).ok().map(|v| v.to_string());
        parse_challenge_ttl_duration(raw.as_deref())
    }

    /// 現在時刻 (UNIX エポック ms)。`worker::Date::now()` 経由。`game_room.rs::now_ms`
    /// と挙動を揃える (絶対時刻で isolate 再構築でも進む)。
    fn now_ms(&self) -> u64 {
        Date::now().as_millis()
    }

    /// `ChallengeRegistry` を DO storage から読み出す (未保存なら空で初期化)。
    /// `purge_expired` 等で書き戻す前提の **抽出** API で、書き戻しは
    /// [`Self::save_challenge_registry`] が担う。
    async fn load_challenge_registry(&self) -> Result<ChallengeRegistry> {
        // storage error は `?` で上位に伝播させる (空 registry に潰すと、cold
        // start restore で transient な storage error が起きた場合に entry を
        // 失って `challenge_expired` 相当に転倒する。Codex review 指摘の通り、
        // 後続 save で正しい registry を上書きする危険を伴う)。
        let v: Option<ChallengeRegistry> = self.state.storage().get(KEY_CHALLENGE_REGISTRY).await?;
        Ok(v.unwrap_or_default())
    }

    /// `ChallengeRegistry` を DO storage に書き戻す。`issue` / `mark` / `unmark`
    /// / `purge_expired` 後に都度呼ぶ契約。
    async fn save_challenge_registry(&self, reg: &ChallengeRegistry) -> Result<()> {
        self.state.storage().put(KEY_CHALLENGE_REGISTRY, reg).await
    }

    /// `pending_ws_attachment_ids` 用の attachment id を採番する。`state.storage()`
    /// 内のカウンタを単調増加させ、文字列化して返す。Cloudflare DO は同 instance
    /// に対する fetch / alarm / websocket_* を単一スレッドで逐次処理するため、
    /// `get → put` の間で他ハンドラが割り込む race は起きない (`game_room.rs::
    /// enter_grace_window` の grace_registry / pending_alarm_kind の 2 連続 put
    /// が同様の前提で動いているのと同じ理由)。
    async fn next_attachment_id(&self) -> Result<String> {
        let current: Option<u64> =
            self.state.storage().get(KEY_NEXT_ATTACHMENT_ID).await.ok().flatten();
        let next = current.unwrap_or(0).saturating_add(1);
        self.state.storage().put(KEY_NEXT_ATTACHMENT_ID, &next).await?;
        Ok(format!("ws-{next}"))
    }

    /// 次回の Alarm を `ChallengeRegistry::earliest_expiry_ms` に基づいて
    /// 設定する。空 registry なら delete_alarm で予約を解除する。既存の Alarm
    /// が earliest より早いケースは現実には発生しない (本 LobbyDO の Alarm は
    /// challenge purge 専用で、他用途で予約されない) ため、単純に上書きする。
    async fn reschedule_challenge_alarm(&self, reg: &ChallengeRegistry) -> Result<()> {
        match reg.earliest_expiry_ms() {
            Some(epoch_ms) => {
                let now_ms = self.now_ms();
                let delay_ms =
                    epoch_ms.saturating_sub(now_ms).saturating_add(CHALLENGE_ALARM_SAFETY_MS);
                self.state.storage().set_alarm(Duration::from_millis(delay_ms)).await?;
            }
            None => {
                let _ = self.state.storage().delete_alarm().await;
            }
        }
        Ok(())
    }

    /// LOGIN_LOBBY 入口で「公開マッチング経路 (`<handle>+<game_name>+<color>`)」
    /// と「私的対局経路 (`<handle>+private-<token>+free`)」を peek 分岐する。
    /// `is_private_login_handle` で `true` を返した場合は私的経路に分岐し、
    /// CHALLENGE_LOBBY 行は専用パス (`handle_challenge_lobby`) に分岐させる。
    async fn dispatch_pending_line(&self, ws: &WebSocket, line: &str) -> Result<()> {
        if line.starts_with("CHALLENGE_LOBBY ") {
            // 中身は `parse_challenge_lobby` 側で再度 strip + 構造化するので、ここでは
            // prefix の有無のみを判定して dispatch する。
            return self.handle_challenge_lobby(ws, line).await;
        }
        if let Some(rest) = line.strip_prefix("LOGIN_LOBBY ") {
            // `<id>` 部分だけを peek し、私的対局フォーマットなら専用 handler へ。
            if let Some(id) = rest.split_whitespace().next()
                && is_private_login_handle(id)
            {
                return self.handle_login_lobby_private(ws, line).await;
            }
        }
        // 既存経路 (公開マッチング) に委譲。`LOGIN_LOBBY` 以外のコマンドは
        // 既存 `handle_login_lobby` 内のパース失敗経路で `not_login_command`
        // として処理される。
        self.handle_login_lobby(ws, line).await
    }

    /// LOGIN_LOBBY 受信時の処理。
    async fn handle_login_lobby(&self, ws: &WebSocket, line: &str) -> Result<()> {
        let req = match parse_login_lobby(line) {
            Ok(r) => r,
            Err(e) => return self.send_login_error(ws, e).await,
        };

        // strict mode: `CLOCK_PRESETS` が宣言済みかつ未登録 `game_name` は拒否。
        // 空 (= preset 未宣言) のときは strict mode 自体を無効化し全 game_name を
        // 通す後方互換動作にとどまる。
        let presets = self.clock_presets();
        if !presets.is_empty() && !presets.contains_key(&req.game_name) {
            return self.send_login_error(ws, LoginLobbyError::UnknownGameName).await;
        }

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
        // 同 handle で既存の `Queued` 状態の WS が残っている場合は close する。
        // queue 側は `enqueue` の `retain(|e| e.handle != entry.handle)` で旧 entry を
        // 抜いてあるが、attachment が `Queued` のまま残ると `dispatch_match` の
        // `state.get_websockets()` 走査で旧 WS にも MATCHED が誤送される。close を
        // 先に行うことで websocket_close 経路に乗せ、attachment を消す。
        evict_old_websockets_with_handle(&self.state, &ws, &req.handle);

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
        if !sent_black || !sent_white {
            // 片方の WS が close 済みなどで MATCHED が届かなかった場合、queue から
            // 既に該当 entry を抜いてあるため client は orphan になる。client 側は
            // recv timeout (60 秒) で接続エラー検出 → retry_delay 経由で再 LOGIN_LOBBY
            // するため最終的に復帰できるが、サーバ側でも警告ログを残して
            // observability を確保する。GameRoom DO 側の login deadline (将来 PR で
            // 実装予定) でも片側不在は救済される。
            console_log!(
                "[Lobby] WARN: MATCHED dispatch incomplete. Black/white client may be orphaned, \
                 client retry expected after recv timeout: room_id={room_id}"
            );
        }
        Ok(())
    }

    async fn send_login_error(&self, ws: &WebSocket, err: LoginLobbyError) -> Result<()> {
        send_line(ws, &build_login_incorrect_line(err.reason()))?;
        // フォーマット違反は接続維持しても回復経路がないので close する。
        let _ = ws.close(Some(1003), Some("bad_login_lobby"));
        Ok(())
    }

    /// `CHALLENGE_LOBBY` 受信時の処理。3 段検証 (`unknown_clock_preset` →
    /// `bad_sfen` → `self_challenge`) を順に行い、通過したら
    /// `ChallengeRegistry::issue` で token を発行して
    /// `CHALLENGE_LOBBY:OK <token> <ttl_sec>` を返す。
    ///
    /// **本 PR スコープ補足**: `unknown_opponent_handle` の検証は Workers では
    /// 実装しない (self-claim モデル、`PasswordStore` 等の認証層がないため)。
    /// 両者揃った時点での GameRoom DO 起動経路は次 PR に分割するため、本関数
    /// は token を登録するだけで対局起動の trigger を発火させない。
    async fn handle_challenge_lobby(&self, ws: &WebSocket, line: &str) -> Result<()> {
        let req = match parse_challenge_lobby(line) {
            Ok(r) => r,
            Err(ChallengeLobbyError::NotChallengeCommand) => {
                // この経路には dispatch 側で `CHALLENGE_LOBBY ` prefix が一致した
                // 場合のみ入る。defense in depth として `bad_format` を返す。
                send_line(ws, &build_challenge_incorrect_line("bad_format"))?;
                return Ok(());
            }
            Err(e) => {
                send_line(ws, &build_challenge_incorrect_line(e.reason()))?;
                return Ok(());
            }
        };

        // 0. issue 直前に期限切れ entry を掃く。`handle_login_lobby_private` 入口
        //    でも同等の即時 purge を行うため、両入口で対称化することで
        //    `earliest_expiry_ms` が古い entry に引きずられて Alarm が空 fire
        //    し続けるのを避ける。purge 戻り値の WS 切断責務は `disconnect_pending_websockets`。
        //    purge / issue を 1 回の load 結果に対して連続適用し、再 load
        //    による purge 結果の取りこぼしを避ける (Codex review 指摘)。
        let now_ms = self.now_ms();
        let ttl = self.challenge_ttl();
        let mut reg = self.load_challenge_registry().await?;
        let expired = reg.purge_expired(now_ms);
        let purged = !expired.is_empty();
        if purged {
            self.disconnect_pending_websockets(&expired).await;
        }

        // 1. clock_preset の存在確認。`CLOCK_PRESETS` 未宣言 (空 map) の構成では
        //    Workers 経路に preset 名解決の正がないため、本コマンド自体を
        //    `unknown_clock_preset` で拒否する (TCP 側は `state.config.clock_presets`
        //    が同じく必須で、未登録は拒否される)。
        let presets = self.clock_presets();
        let Some(clock_spec) = presets.get(&req.clock_preset).cloned() else {
            // 早期 return でも、purge した reg は永続化する (取りこぼし回避)。
            if purged {
                self.save_challenge_registry(&reg).await?;
                self.reschedule_challenge_alarm(&reg).await?;
            }
            send_line(ws, &build_challenge_incorrect_line("unknown_clock_preset"))?;
            return Ok(());
        };

        // 2. initial_sfen 検証は core ヘルパで行う。`position_section_from_sfen`
        //    と `side_to_move_from_sfen` の双方が `Ok` でなければ `bad_sfen`。
        if let Some(sfen) = &req.initial_sfen
            && !is_valid_sfen(sfen)
        {
            if purged {
                self.save_challenge_registry(&reg).await?;
                self.reschedule_challenge_alarm(&reg).await?;
            }
            send_line(ws, &build_challenge_incorrect_line("bad_sfen"))?;
            return Ok(());
        }

        // 3. registry に発行 (self_challenge は内部 enum で帰る)。purge 後の
        //    `reg` をそのまま使い、再 load しない (Codex 指摘の取りこぼし回避)。
        let issue_result = reg.issue(
            PlayerName::new(&req.inviter),
            PlayerName::new(&req.opponent),
            req.inviter_color,
            clock_spec,
            req.initial_sfen.clone(),
            ttl,
            now_ms,
        );
        match issue_result {
            Ok(token) => {
                self.save_challenge_registry(&reg).await?;
                self.reschedule_challenge_alarm(&reg).await?;
                let ttl_sec = ttl.as_secs();
                send_line(ws, &build_challenge_ok_line(token.as_str(), ttl_sec))?;
                console_log!(
                    "[Lobby] CHALLENGE_LOBBY: issued token inviter={} opponent={} preset={} ttl_sec={}",
                    req.inviter,
                    req.opponent,
                    req.clock_preset,
                    ttl_sec,
                );
            }
            Err(IssueError::SelfChallenge) => {
                // self_challenge でも purge があった場合は永続化する。
                if purged {
                    self.save_challenge_registry(&reg).await?;
                    self.reschedule_challenge_alarm(&reg).await?;
                }
                send_line(ws, &build_challenge_incorrect_line("self_challenge"))?;
            }
        }
        Ok(())
    }

    /// 私的対局 LOGIN_LOBBY (`<handle>+private-<token>+free`) の処理。
    ///
    /// 検証順序 (Issue #582 仕様):
    /// 1. パース失敗 (`+free` 以外 / hex 不正 / 引数不足) → `LOGIN_LOBBY:incorrect`
    ///    + 適切な reason
    /// 2. token 期限切れ / 未登録 → `LOGIN_LOBBY:incorrect challenge_expired`
    /// 3. handle が `inviter` / `opponent` のどちらにも一致しない →
    ///    `LOGIN_LOBBY:incorrect not_invited`
    /// 4. 同 handle が同 token に既登録 → `LOGIN_LOBBY:incorrect already_logged_in`
    /// 5. 通過 → `mark_ws_logged_in` で attachment id を登録し、
    ///    `LOGIN_LOBBY:<handle> OK pending_match_dispatch_pending` 暫定応答を返す。
    ///
    /// **本 PR スコープ補足**: 両者揃った時点で `consume(token)` → GameRoom DO
    /// 起動 + clock_spec / initial_sfen バトンパスする経路は Issue #582
    /// follow-up integration の後半スコープに分割する。本 PR では LOGIN_LOBBY
    /// を受理して attachment を `PrivatePending` で登録するだけで、対局起動
    /// trigger を発火させない (WS は接続維持され、次 PR の dispatch 経路で
    /// 起動する)。
    async fn handle_login_lobby_private(&self, ws: &WebSocket, line: &str) -> Result<()> {
        let req = match parse_login_lobby_with_free(line) {
            Ok(r) => r,
            Err(e) => return self.send_private_login_error(ws, e).await,
        };
        let LoginLobbyPrivateRequest { handle, token } = req;

        // 認証直後に TTL purge を 1 回走らせて、対局相手の到着前に expire した
        // token を即時掃除する (Alarm の最終ガードに加えた即時パス)。
        let now_ms = self.now_ms();
        let mut reg = self.load_challenge_registry().await?;
        let expired = reg.purge_expired(now_ms);
        if !expired.is_empty() {
            self.disconnect_pending_websockets(&expired).await;
        }

        // 期限切れ / 未登録 → `challenge_expired`
        let entry = match reg.lookup(&token, now_ms) {
            Some(e) => e.clone(),
            None => {
                if !expired.is_empty() {
                    // expire 後に登録簿が変わったので、(空でなければ) save し直す。
                    self.save_challenge_registry(&reg).await?;
                    self.reschedule_challenge_alarm(&reg).await?;
                }
                send_line(ws, &build_login_incorrect_line("challenge_expired"))?;
                let _ = ws.close(Some(1000), Some("challenge_expired"));
                return Ok(());
            }
        };

        // handle 一致確認 (case-sensitive)
        if handle != entry.inviter && handle != entry.opponent {
            if !expired.is_empty() {
                self.save_challenge_registry(&reg).await?;
                self.reschedule_challenge_alarm(&reg).await?;
            }
            send_line(ws, &build_login_incorrect_line("not_invited"))?;
            let _ = ws.close(Some(1000), Some("not_invited"));
            return Ok(());
        }

        // 同 handle が既登録なら `already_logged_in`
        if entry.pending_ws_attachment_ids.contains_key(&handle) {
            if !expired.is_empty() {
                self.save_challenge_registry(&reg).await?;
                self.reschedule_challenge_alarm(&reg).await?;
            }
            send_line(ws, &build_login_incorrect_line("already_logged_in"))?;
            let _ = ws.close(Some(1000), Some("already_logged_in"));
            return Ok(());
        }

        // 通過: attachment id を採番し registry に mark、attachment を更新
        let attachment_id = self.next_attachment_id().await?;
        reg.mark_ws_logged_in(
            &token,
            PlayerName::new(handle.as_str()),
            attachment_id.clone(),
            now_ms,
        );
        self.save_challenge_registry(&reg).await?;
        self.reschedule_challenge_alarm(&reg).await?;

        ws.serialize_attachment(&LobbyAttachment::PrivatePending {
            token: token.as_str().to_owned(),
            handle: handle.clone(),
            attachment_id: attachment_id.clone(),
        })
        .map_err(|e| Error::RustError(format!("serialize_attachment: {e}")))?;

        send_line(ws, &format!("LOGIN_LOBBY:{handle} OK pending_match_dispatch_pending"))?;
        console_log!(
            "[Lobby] LOGIN_LOBBY private: handle={handle} token={} attachment_id={}",
            token.as_str(),
            attachment_id,
        );
        Ok(())
    }

    /// 私的対局 attachment の close 経路。stale handle race を避けるため、
    /// `unmark_ws_logged_in` は attachment id 単位で照合させる。
    async fn handle_private_pending_close(
        &self,
        token: &str,
        handle: &str,
        attachment_id: &str,
    ) -> Result<()> {
        let mut reg = self.load_challenge_registry().await?;
        let token_obj = ChallengeToken::from_raw(token);
        reg.unmark_ws_logged_in(&token_obj, &PlayerName::new(handle), attachment_id);
        self.save_challenge_registry(&reg).await?;
        self.reschedule_challenge_alarm(&reg).await?;
        console_log!(
            "[Lobby] private LOGIN ws closed: handle={handle} token={token} attachment_id={attachment_id}"
        );
        Ok(())
    }

    /// 私的対局 attachment 状態で受信した line。本 PR スコープでは対局起動が
    /// 動かないため、`LOGOUT_LOBBY` を受理して切断する以外は ignore する。
    /// `LOBBY_PONG` は keep-alive として silent に受理する。
    async fn handle_private_pending_line(
        &self,
        ws: &WebSocket,
        token: &str,
        handle: &str,
        attachment_id: &str,
        line: &str,
    ) -> Result<()> {
        match line {
            "LOGOUT_LOBBY" => {
                self.handle_private_pending_close(token, handle, attachment_id).await?;
                let _ = ws.close(Some(1000), Some("logout"));
                Ok(())
            }
            "LOBBY_PONG" => Ok(()),
            _ => {
                console_log!(
                    "[Lobby] private pending client sent unexpected line: handle={handle} line={line}"
                );
                Ok(())
            }
        }
    }

    async fn send_private_login_error(
        &self,
        ws: &WebSocket,
        err: LoginLobbyPrivateError,
    ) -> Result<()> {
        send_line(ws, &build_login_incorrect_line(err.reason()))?;
        let _ = ws.close(Some(1003), Some("bad_login_lobby_private"));
        Ok(())
    }

    /// DO Alarm ハンドラから呼ぶ TTL purge。期限切れ entry を一括削除し、
    /// 戻り値の `pending_ws_attachment_ids` を走査して該当 WS にエラー送信
    /// + close する。残 entry があれば `earliest_expiry_ms` で Alarm を再設定。
    async fn handle_challenge_alarm(&self) -> Result<()> {
        let now_ms = self.now_ms();
        let mut reg = self.load_challenge_registry().await?;
        let expired = reg.purge_expired(now_ms);
        if expired.is_empty() {
            // 期限切れが無いまま Alarm が早めに発火した場合 (clock skew) は
            // 残 registry の earliest で再予約して終了。
            self.reschedule_challenge_alarm(&reg).await?;
            return Ok(());
        }
        self.disconnect_pending_websockets(&expired).await;
        self.save_challenge_registry(&reg).await?;
        self.reschedule_challenge_alarm(&reg).await?;
        console_log!("[Lobby] challenge purge_expired: removed={}", expired.len());
        Ok(())
    }

    /// 期限切れ `(token, entry)` の組から `pending_ws_attachment_ids` を集めて、
    /// 該当する `PrivatePending` attachment を持つ WS にエラー送信 + close。
    /// `state.get_websockets()` を 1 回だけ走査するために攻撃面の attachment id
    /// 集合を pre-compute する (登録 entry の数 × 2 attachment 上限なので O(n))。
    async fn disconnect_pending_websockets(&self, expired: &[(ChallengeToken, ChallengeEntry)]) {
        if expired.is_empty() {
            return;
        }
        // 期限切れの (token, attachment_id) 集合を平坦化する。
        let mut targets: Vec<(String, String)> = Vec::new();
        for (token, entry) in expired {
            for attachment_id in entry.pending_ws_attachment_ids.values() {
                targets.push((token.as_str().to_owned(), attachment_id.clone()));
            }
        }
        if targets.is_empty() {
            return;
        }
        for ws in self.state.get_websockets() {
            let att = match ws.deserialize_attachment::<LobbyAttachment>() {
                Ok(Some(a)) => a,
                _ => continue,
            };
            if let LobbyAttachment::PrivatePending {
                token: ws_token,
                attachment_id: ws_id,
                ..
            } = att
                && targets.iter().any(|(t, a)| t == &ws_token && a == &ws_id)
            {
                // 期限切れであることをクライアントに通知してから close。
                let _ = send_line(&ws, &build_login_incorrect_line("challenge_expired"));
                let _ = ws.close(Some(1000), Some("challenge_expired"));
            }
        }
    }
}

/// 私的対局 (`CHALLENGE_LOBBY`) の `<sfen>` 妥当性を検証する。core の
/// `position_section_from_sfen` と `side_to_move_from_sfen` の双方が `Ok` を
/// 返すときのみ `true`。Game_Summary 構築経路 (`GameRoom`) と同じ 2 関数で
/// 揃えることで、CHALLENGE 時点で受理した SFEN が以降の対局駆動でも再利用
/// 可能であることを保証する (TCP `process_challenge` と同じ流儀)。
fn is_valid_sfen(sfen: &str) -> bool {
    use rshogi_csa_server::protocol::summary::{
        position_section_from_sfen, side_to_move_from_sfen,
    };
    position_section_from_sfen(sfen).is_ok() && side_to_move_from_sfen(sfen).is_ok()
}

/// 同 handle で `Queued` attachment を持つ旧 WS を close して、新 WS のみが
/// マッチング対象になるよう揃える。`evict_old` 挙動 (本家 Floodgate 互換) を
/// attachment レイヤでも反映する。`current_ws` は除外する (新しい接続そのものを
/// close しないため)。
fn evict_old_websockets_with_handle(state: &State, current_ws: &WebSocket, handle: &str) {
    for ws in state.get_websockets() {
        // `WebSocket` には eq 比較がないので serialize_attachment 側の同一性で
        // 判定する代替が無く、ws を都度 attachment 経由で見て handle 一致を見る。
        // current_ws と他 WS の弁別: current_ws は LOGIN_LOBBY 直後で attachment を
        // まだ `Queued` に切り替えていない (`handle_login_lobby` 内で本関数の後に
        // serialize_attachment するため、`current_ws` の attachment は Pending か
        // 古い handle のままで、新 handle と一致しない経路が支配的)。同 handle で
        // 別 WS のみが loop 対象になる安全側設計。
        let _ = current_ws; // 比較には使わないが、契約を明示するために引数に残す。
        let attachment = match ws.deserialize_attachment::<LobbyAttachment>() {
            Ok(Some(a)) => a,
            _ => continue,
        };
        if let LobbyAttachment::Queued {
            handle: existing, ..
        } = attachment
        {
            if existing == handle {
                console_log!(
                    "[Lobby] evict old queued WS with duplicate handle: handle={existing}"
                );
                let _ = ws.close(Some(1000), Some("evicted_by_new_login"));
            }
        }
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
