//! `GameRoom` Durable Object の対局ロジック実装。
//!
//! 1 部屋 = 1 DO インスタンス。以下のライフサイクルを駆動する:
//!
//! 1. **WebSocket Upgrade** (`fetch`): 対局者は [`WsAttachment::Pending`]、
//!    観戦者は [`WsAttachment::Spectator`] を付けて
//!    `state.accept_web_socket` で hibernation を有効化する。
//! 2. **LOGIN** (`websocket_message` / pending): `<handle>+<game_name>+<color>`
//!    形式を分解し、役割 (Role) 付きスロットとして [`state.storage().put`] に
//!    保存する。WS 側の attachment も `Player` に差し替える。パスワードの
//!    実検証は本クレートのスコープ外（入口で accept-all）で、認証ストレージ
//!    連携は別モジュールの責務。
//! 3. **マッチ成立**: 2 人目の LOGIN で役割が相補、同じ game_name なら
//!    [`CoreRoom`] を生成して Game_Summary を双方へ送出する。状態は
//!    `AgreeWaiting` として Core 側が握る。
//! 4. **対局中の行受信** (`websocket_message` / player): attachment から Color を
//!    取り出し、[`CoreRoom::handle_line`] に流して `HandleResult::broadcasts` を
//!    宛先色別に fanout する。着手は `moves` テーブルに append する。
//! 5. **切断** (`websocket_close`): 認証済みプレイヤの切断は
//!    [`CoreRoom::force_abnormal`] で敗北を確定する。
//! 6. **時間切れ駆動** (`alarm`): 手番開始ごとに `state.storage().set_alarm`
//!    で deadline を予約し、到着した時に `CoreRoom::force_time_up(current_turn)`
//!    で負け側を確定する。
//! 7. **再起動復元** (`ensure_core_loaded`): DO isolate が破棄された後の
//!    最初の操作で、`play_started_at_ms` が立っていれば AGREE を再送し、
//!    続けて `moves` テーブルを ply 順に `handle_line` で replay して
//!    CoreRoom を復元する。
//! 8. **棋譜エクスポート** (`export_kifu_to_r2`): 終局を観測した瞬間に
//!    CSA V2 形式で組み立て、R2 の `YYYY/MM/DD/<game_id>.csa` に書き出す。
//!    TCP 側 `FileKifuStorage` と同一キー体系で Ruby 系バッチとの互換性を保つ。

use std::cell::RefCell;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use worker::{
    Date, DurableObject, Env, Error, Request, Response, ResponseBuilder, Result, State, WebSocket,
    WebSocketIncomingMessage, WebSocketPair, console_log, durable_object, wasm_bindgen,
};

use rshogi_core::types::EnteringKingRule;
use rshogi_csa_server::ClockSpec;
use rshogi_csa_server::game::clock::TimeClock;
use rshogi_csa_server::game::room::{
    BroadcastEntry, BroadcastTarget, GameRoom as CoreRoom, GameRoomConfig, HandleOutcome,
    HandleResult,
};
use rshogi_csa_server::protocol::command::{ClientCommand, parse_command};
use rshogi_csa_server::protocol::summary::{
    GameSummaryBuilder, position_section_from_sfen, side_to_move_from_sfen,
    standard_initial_position_block,
};
use rshogi_csa_server::record::kifu::{fork_initial_sfen_from_kifu, initial_sfen_from_csa_moves};
use rshogi_csa_server::types::{Color, CsaLine, CsaMoveToken, GameId, GameName, PlayerName};

use crate::attachment::{Role, WsAttachment, parse_login_handle};
use crate::config::{ConfigKeys, parse_clock_spec};
use crate::datetime::{format_csa_datetime, format_date_path};
use crate::persistence::{
    FinishedState, MoveRow, PersistedConfig, ReplaySummary, replay_core_room,
};
use crate::session_state::{LoginReply, MatchResult, Slot, evaluate_match};
use crate::spectator_control::{MonitorDecision, resolve_monitor_target};
use crate::ws_route::{WsRoute, parse_ws_route};
use crate::x1_paths::{buoy_object_key, default_fork_buoy_name, kifu_by_id_object_key};

const DEFAULT_MAX_MOVES: u32 = 256;
const DEFAULT_TIME_MARGIN_MS: u64 = 1000;

/// Alarm 発火時刻に上乗せする安全側マージン（ミリ秒）。Cloudflare Alarm API
/// のジッタと `Date::now()` ↔ `handle_line` の now_ms 伝搬遅延を吸収する。
const ALARM_SAFETY_MS: u64 = 200;

/// Durable Object 初期化 SQL。
///
/// moves のみ SQL で持つ（append と ply 順 replay の効率を理由に）。
/// 他の構造化状態 (slots / config / finished) は `state.storage().put/get` で
/// JSON として置き、スキーママイグレーションを軽くする。
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS moves (
    ply INTEGER PRIMARY KEY,
    color TEXT NOT NULL,
    line TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);
"#;

const KEY_ROOM_ID: &str = "room_id";
const KEY_SLOTS: &str = "slots";
const KEY_CONFIG: &str = "config";
const KEY_FINISHED: &str = "finished";

/// R2 上の buoy 保存フォーマット。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedBuoy {
    moves: Vec<String>,
    remaining: u32,
    #[serde(default)]
    initial_sfen: Option<String>,
}

enum BuoyReservation {
    Missing,
    Reserved(Option<String>),
    Exhausted,
}

/// 1 対局分の Durable Object。
#[durable_object]
pub struct GameRoom {
    state: State,
    env: Env,
    core: RefCell<Option<CoreRoom>>,
    config: RefCell<Option<PersistedConfig>>,
}

impl DurableObject for GameRoom {
    fn new(state: State, env: Env) -> Self {
        let sql = state.storage().sql();
        sql.exec(SCHEMA_SQL, None).expect("failed to initialize DO schema");
        Self {
            state,
            env,
            core: RefCell::new(None),
            config: RefCell::new(None),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path();
        let Some(route) = parse_ws_route(&path) else {
            return Response::error("Upgrade required", 426);
        };
        let room_id = route.room_id();

        // 初回 fetch でのみ room_id を永続化する。`start_match` 側で game_id 生成に
        // 使うため、DO 再構築後でも同じ値を参照できるよう storage に置く。
        // room_id は `id_from_name` のキーと一致するので、同一 DO インスタンスでは
        // 常に同じ値が到着する前提。
        let existing: Option<String> = self.state.storage().get(KEY_ROOM_ID).await?;
        if existing.is_none() && !room_id.is_empty() {
            self.state.storage().put(KEY_ROOM_ID, room_id.to_owned()).await?;
        }

        let pair = WebSocketPair::new()?;
        let server = pair.server;
        self.state.accept_web_socket(&server);

        let pending = match route {
            WsRoute::Player { .. } => WsAttachment::Pending,
            WsRoute::Spectator { room_id } => WsAttachment::spectator(room_id),
        };
        server
            .serialize_attachment(&pending)
            .map_err(|e| Error::RustError(format!("serialize_attachment: {e}")))?;

        console_log!("[GameRoom] websocket upgrade accepted");

        Ok(ResponseBuilder::new().with_status(101).with_websocket(pair.client).empty())
    }

    async fn websocket_message(&self, ws: WebSocket, msg: WebSocketIncomingMessage) -> Result<()> {
        let raw = match msg {
            WebSocketIncomingMessage::String(s) => s,
            WebSocketIncomingMessage::Binary(_) => return Ok(()),
        };
        let line = raw.trim_end_matches(['\r', '\n']).to_owned();

        let attachment: WsAttachment = ws
            .deserialize_attachment()
            .map_err(|e| Error::RustError(format!("deserialize_attachment: {e}")))?
            .unwrap_or(WsAttachment::Pending);

        match attachment {
            WsAttachment::Pending => self.handle_login(&ws, &line).await,
            WsAttachment::Player { role, handle, .. } => {
                self.handle_game_line(&ws, role, &handle, &line).await
            }
            WsAttachment::Spectator { room_id } => {
                self.handle_spectator_line(&ws, &room_id, &line).await
            }
        }
    }

    async fn websocket_close(
        &self,
        ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        // attachment が corrupt (JSON が壊れた等) の場合は None と同じ扱いにせざるを
        // 得ないが、診断のためにエラー内容をログへ残す。現実装では Player 以外 (Pending /
        // corrupt) は slot 解放できないので何もせず return する。
        let att: Option<WsAttachment> = ws.deserialize_attachment().unwrap_or_else(|e| {
            console_log!("[GameRoom] websocket_close: deserialize_attachment failed: {e:?}");
            None
        });
        let Some(WsAttachment::Player { role, .. }) = att else {
            return Ok(());
        };

        // 終局後に届く close は CoreRoom を再構築して force_abnormal してしまうと
        // 永続化済みの正常終局結果を上書きしてしまうため、ここで即 return する。
        if self.load_finished().await?.is_some() {
            return Ok(());
        }

        // マッチ前の切断はコアを作らず、占有していたスロットだけを解放する。
        // これが漏れると同色枠が埋まったまま残り、以降の再 LOGIN が必ず conflict で弾かれる。
        let cfg_opt: Option<PersistedConfig> = self.state.storage().get(KEY_CONFIG).await?;
        if cfg_opt.is_none() {
            let mut slots = self.load_slots().await?;
            slots.retain(|s| s.role != role);
            self.state.storage().put(KEY_SLOTS, &slots).await?;
            return Ok(());
        }

        // 対局中の切断は force_abnormal で敗北を確定する。
        self.ensure_core_loaded().await?;
        let result_opt =
            self.core.borrow_mut().as_mut().map(|core| core.force_abnormal(role.to_core()));
        if let Some(result) = result_opt {
            self.dispatch_broadcasts(&result.broadcasts).await?;
            self.finalize_if_ended(&result).await?;
        }
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }

    async fn alarm(&self) -> Result<Response> {
        // 既に終局済みの DO でアラームが届いたら何もしない（念のためのガード）。
        if self.load_finished().await?.is_some() {
            return Response::ok("already finished");
        }

        self.ensure_core_loaded().await?;
        let outcome = {
            let mut borrow = self.core.borrow_mut();
            let Some(core) = borrow.as_mut() else {
                return Response::ok("no core");
            };
            // 時計切れ側は現在手番（SFEN `side_to_move` を起点に手数で交代した
            // 色）。buoy / `%%FORK` で白開始の局面でも正しく白を時間切れ扱いに
            // する。
            let loser = core.current_turn();
            Some(core.force_time_up(loser))
        };
        if let Some(result) = outcome {
            self.dispatch_broadcasts(&result.broadcasts).await?;
            self.finalize_if_ended(&result).await?;
        }
        Response::ok("time_up handled")
    }
}

impl GameRoom {
    /// 現在時刻（UNIX エポック ミリ秒）。`worker::Date::now()` を介して取得する。
    /// CoreRoom の `now_ms` は monotonic を想定するが、Workers では wall-clock しか
    /// ないため Date::now() を許容する。起点は DO インスタンス越しで一貫する
    /// （絶対時刻なので isolate 再構築でも進む）。
    fn now_ms(&self) -> u64 {
        Date::now().as_millis()
    }

    /// LOGIN 到着の pending ws に対する処理。
    async fn handle_login(&self, ws: &WebSocket, line: &str) -> Result<()> {
        // 既に終局済みの DO に新しい LOGIN は受け入れない。
        if self.load_finished().await?.is_some() {
            send_line(ws, &LoginReply::Incorrect.to_line())?;
            let _ = ws.close(Some(1000), Some("room finished".to_owned()));
            return Ok(());
        }

        let csa = CsaLine::new(line);
        let cmd = match parse_command(&csa) {
            Ok(c) => c,
            Err(_) => {
                send_line(ws, &LoginReply::Incorrect.to_line())?;
                return Ok(());
            }
        };
        let ClientCommand::Login { name, .. } = cmd else {
            // pending 状態で LOGIN 以外が来たら拒否して切断。
            send_line(ws, &LoginReply::Incorrect.to_line())?;
            let _ = ws.close(Some(1000), Some("expected LOGIN".to_owned()));
            return Ok(());
        };

        let Some((handle, game_name, role)) = parse_login_handle(name.as_str()) else {
            send_line(ws, &LoginReply::Incorrect.to_line())?;
            return Ok(());
        };

        // 新スロットを**仮に**加えて衝突判定する。`evaluate_match` が Conflict を返す
        // 場合は永続化も attachment 差し替えも行わず、部屋を破壊しないよう拒否する
        // （game_name 不一致・重複 role・スロット超過の全てを一元的に弾く）。
        let mut next_slots = self.load_slots().await?;
        next_slots.push(Slot {
            role,
            handle: handle.clone(),
            game_name: game_name.clone(),
        });
        if let MatchResult::Conflict { reason } = evaluate_match(&next_slots) {
            console_log!("[GameRoom] LOGIN rejected (conflict: {reason})");
            send_line(ws, &LoginReply::Incorrect.to_line())?;
            return Ok(());
        }

        // 検証を通ったので slots を書き戻し、attachment を Player に差し替える。
        self.state.storage().put(KEY_SLOTS, &next_slots).await?;
        let att = WsAttachment::player(role, handle.clone(), game_name.clone());
        ws.serialize_attachment(&att)
            .map_err(|e| Error::RustError(format!("attach player: {e}")))?;

        let ok_reply = LoginReply::Ok {
            name: name.to_string(),
        };
        send_line(ws, &ok_reply.to_line())?;

        if let MatchResult::Match {
            black_handle,
            white_handle,
            game_name,
        } = evaluate_match(&next_slots)
        {
            let _ = self.start_match(&black_handle, &white_handle, &game_name).await?;
        }

        Ok(())
    }

    /// マッチ成立時の処理: CoreRoom 作成 + Game_Summary 送出。
    async fn start_match(
        &self,
        black_handle: &str,
        white_handle: &str,
        game_name: &str,
    ) -> Result<bool> {
        // `room_id` は fetch 時に永続化している（DO インスタンス = room_id なので
        // 他 DO と衝突しない。game_id は `<room_id>-<epoch_ms>` 形式で、
        // 別 DO が同一ミリ秒にマッチしても R2 キー `YYYY/MM/DD/<game_id>.csa` が
        // 一意になるように room_id を混ぜる）。
        let started = self.now_ms();
        let room_id: String = self
            .state
            .storage()
            .get(KEY_ROOM_ID)
            .await?
            .unwrap_or_else(|| "unknown".to_owned());
        let game_id = format!("{room_id}-{started}");
        let clock_spec = load_clock_spec_from_env(&self.env)?;
        // 双方の LOGIN は既に OK を返しているので、予約で失敗したまま早期
        // return するとスロットが永久に詰まる。Exhausted に加え、CAS リトライ
        // 上限到達などの Err も pending match abort 経路に落として部屋を
        // 再利用可能にする (codex レビュー PR #474 2nd round P2)。
        let reservation = match self.reserve_initial_sfen_from_buoy(&GameName::new(game_name)).await
        {
            Ok(r) => r,
            Err(e) => {
                console_log!(
                    "[GameRoom] buoy '{game_name}' reservation failed: {e:?}; rejecting pending match"
                );
                self.abort_pending_match_with_error(&format!(
                    "##[ERROR] buoy '{game_name}' reservation failed"
                ))
                .await?;
                return Ok(false);
            }
        };
        let initial_sfen = match reservation {
            BuoyReservation::Missing => None,
            BuoyReservation::Reserved(initial_sfen) => initial_sfen,
            BuoyReservation::Exhausted => {
                console_log!("[GameRoom] buoy '{game_name}' exhausted; rejecting pending match");
                self.abort_pending_match_with_error(&format!(
                    "##[ERROR] buoy '{game_name}' exhausted"
                ))
                .await?;
                return Ok(false);
            }
        };

        let cfg = PersistedConfig {
            game_id: game_id.clone(),
            black_handle: black_handle.to_owned(),
            white_handle: white_handle.to_owned(),
            game_name: game_name.to_owned(),
            clock: clock_spec.clone(),
            max_moves: DEFAULT_MAX_MOVES,
            time_margin_ms: DEFAULT_TIME_MARGIN_MS,
            matched_at_ms: started,
            play_started_at_ms: None,
            initial_sfen,
        };
        self.state.storage().put(KEY_CONFIG, &cfg).await?;

        // CoreRoom を構築して in-memory に置く。
        let clock: Box<dyn TimeClock> = clock_spec.build_clock();
        let time_section = clock_spec.format_time_section();
        // initial_sfen 指定時は Game_Summary `position_section` / `To_Move` を
        // 同じ SFEN から派生させる。未指定時は平手相当のブロックと `Color::Black`。
        let (position_section, to_move) = match cfg.initial_sfen.as_deref() {
            Some(sfen) => {
                let section = position_section_from_sfen(sfen).map_err(Error::RustError)?;
                let side = side_to_move_from_sfen(sfen).map_err(Error::RustError)?;
                (section, side)
            }
            None => (standard_initial_position_block(), Color::Black),
        };
        // `CoreRoom::new` は initial_sfen が不正な場合に Err を返す。Workers DO は
        // 永続化済み config から cold start 復元することもあるため、Err を panic で
        // 落とさず Error::RustError で Runtime に伝搬する (Codex review PR #470
        // 4th round P2)。
        let core = CoreRoom::new(
            GameRoomConfig {
                game_id: GameId::new(cfg.game_id.clone()),
                black: PlayerName::new(cfg.black_handle.clone()),
                white: PlayerName::new(cfg.white_handle.clone()),
                max_moves: cfg.max_moves,
                time_margin_ms: cfg.time_margin_ms,
                entering_king_rule: EnteringKingRule::Point24,
                initial_sfen: cfg.initial_sfen.clone(),
            },
            clock,
        )
        .map_err(|e| Error::RustError(format!("CoreRoom::new: {e:?}")))?;
        *self.core.borrow_mut() = Some(core);
        *self.config.borrow_mut() = Some(cfg.clone());

        // Game_Summary を双方に送出（Your_Turn だけ色で変える）。
        let builder = GameSummaryBuilder {
            game_id: GameId::new(cfg.game_id),
            black: PlayerName::new(cfg.black_handle),
            white: PlayerName::new(cfg.white_handle),
            time_section,
            position_section,
            rematch_on_draw: false,
            to_move,
            declaration: String::new(),
        };
        let summary_black = builder.build_for(Color::Black);
        let summary_white = builder.build_for(Color::White);

        self.send_to_role(Role::Black, &summary_black).await?;
        self.send_to_role(Role::White, &summary_white).await?;

        Ok(true)
    }

    /// 対局中のプレイヤからの行を CoreRoom に流す。
    async fn handle_game_line(
        &self,
        ws: &WebSocket,
        role: Role,
        handle: &str,
        line: &str,
    ) -> Result<()> {
        if self.load_finished().await?.is_some() {
            // 終局後に届いた行は無視する。
            return Ok(());
        }

        let csa = CsaLine::new(line);
        if let Ok(cmd) = parse_command(&csa) {
            if let Some(replies) = self.handle_player_control_command(handle, cmd).await? {
                for out in replies {
                    send_line(ws, &out)?;
                }
                return Ok(());
            }
        }

        if self.active_game_id().await?.is_none() && !self.try_start_pending_match().await? {
            return Ok(());
        }

        self.ensure_core_loaded().await?;
        let now = self.now_ms();
        let color = role.to_core();

        let result = {
            let mut borrow = self.core.borrow_mut();
            let Some(core) = borrow.as_mut() else {
                console_log!("[GameRoom] handle_game_line: core missing (handle={handle})");
                return Ok(());
            };
            match core.handle_line(color, &csa, now) {
                Ok(r) => r,
                Err(e) => {
                    console_log!("[GameRoom] handle_line error: {e:?}");
                    return Ok(());
                }
            }
        };

        // Playing 開始を確定できた瞬間だけ cfg を更新（冪等）。
        if let HandleOutcome::GameStarted = result.outcome {
            self.mark_play_started(now).await?;
        }

        // 着手を永続化。MoveAccepted の場合のみ moves テーブルに append する。
        if let HandleOutcome::MoveAccepted { .. } = result.outcome {
            self.append_move(color, line, now).await?;
        }

        self.dispatch_broadcasts(&result.broadcasts).await?;
        self.reschedule_turn_alarm(&result.outcome).await?;
        self.finalize_if_ended(&result).await?;
        Ok(())
    }

    /// 観戦者からの制御行。`%%CHAT` を同一 room の全参加者へ relay し、
    /// `%%MONITOR2OFF` は確認応答後に socket を閉じる。
    async fn handle_spectator_line(&self, ws: &WebSocket, room_id: &str, line: &str) -> Result<()> {
        let csa = CsaLine::new(line);
        let Ok(cmd) = parse_command(&csa) else {
            return Ok(());
        };
        let active_game_id = self.active_game_id().await?;
        let monitor_id = active_game_id.as_deref().unwrap_or(room_id);
        match cmd {
            ClientCommand::KeepAlive => Ok(()),
            ClientCommand::Chat { message } => {
                self.relay_chat("spectator", &message).await?;
                send_line(ws, &format!("##[CHAT] OK {monitor_id}"))?;
                send_line(ws, "##[CHAT] END")?;
                Ok(())
            }
            ClientCommand::Monitor2Off { game_id } => {
                match resolve_monitor_target(room_id, active_game_id.as_deref(), game_id.as_str()) {
                    MonitorDecision::Accept { monitor_id } => {
                        send_line(ws, &format!("##[MONITOR2OFF] {monitor_id}"))?;
                        send_line(ws, "##[MONITOR2OFF] END")?;
                        let _ = ws.close(Some(1000), Some("spectator off".to_owned()));
                    }
                    MonitorDecision::NotFound { requested } => {
                        send_line(ws, &format!("##[MONITOR2OFF] NOT_FOUND {requested}"))?;
                        send_line(ws, "##[MONITOR2OFF] END")?;
                    }
                }
                Ok(())
            }
            ClientCommand::Monitor2On { game_id } => {
                match resolve_monitor_target(room_id, active_game_id.as_deref(), game_id.as_str()) {
                    MonitorDecision::Accept { monitor_id } => {
                        send_line(ws, &format!("##[MONITOR2] BEGIN {monitor_id}"))?;
                    }
                    MonitorDecision::NotFound { requested } => {
                        send_line(ws, &format!("##[MONITOR2] NOT_FOUND {requested}"))?;
                    }
                }
                send_line(ws, "##[MONITOR2] END")?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// プレイヤー接続から受け付ける制御系コマンドを処理する。
    ///
    /// `Some(replies)` を返した場合は、呼び出し側が返信行を送って通常の
    /// `CoreRoom::handle_line` 経路をスキップする。
    async fn handle_player_control_command(
        &self,
        handle: &str,
        cmd: ClientCommand,
    ) -> Result<Option<Vec<String>>> {
        match cmd {
            ClientCommand::Chat { message } => {
                self.relay_chat(handle, &message).await?;
                let monitor_id = self.current_monitor_id().await?;
                Ok(Some(vec![
                    format!("##[CHAT] OK {monitor_id}"),
                    "##[CHAT] END".to_owned(),
                ]))
            }
            ClientCommand::SetBuoy {
                game_name,
                moves,
                count,
            } => {
                if !self.is_admin_handle(handle) {
                    return Ok(Some(vec![
                        format!("##[SETBUOY] PERMISSION_DENIED {game_name}"),
                        "##[SETBUOY] END".to_owned(),
                    ]));
                }
                let derived = match initial_sfen_from_csa_moves(&moves) {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(Some(vec![
                            format!("##[SETBUOY] ERROR {game_name} {e}"),
                            "##[SETBUOY] END".to_owned(),
                        ]));
                    }
                };
                let doc = PersistedBuoy {
                    moves: moves.into_iter().map(|m| m.as_str().to_owned()).collect(),
                    remaining: count,
                    initial_sfen: Some(derived),
                };
                if let Err(e) = self.store_buoy(&game_name, &doc).await {
                    return Ok(Some(vec![
                        format!("##[SETBUOY] ERROR {game_name} {e}"),
                        "##[SETBUOY] END".to_owned(),
                    ]));
                }
                Ok(Some(vec![
                    format!("##[SETBUOY] OK {game_name} {count}"),
                    "##[SETBUOY] END".to_owned(),
                ]))
            }
            ClientCommand::DeleteBuoy { game_name } => {
                if !self.is_admin_handle(handle) {
                    return Ok(Some(vec![
                        format!("##[DELETEBUOY] PERMISSION_DENIED {game_name}"),
                        "##[DELETEBUOY] END".to_owned(),
                    ]));
                }
                if let Err(e) = self.delete_buoy(&game_name).await {
                    return Ok(Some(vec![
                        format!("##[DELETEBUOY] ERROR {game_name} {e}"),
                        "##[DELETEBUOY] END".to_owned(),
                    ]));
                }
                Ok(Some(vec![
                    format!("##[DELETEBUOY] OK {game_name}"),
                    "##[DELETEBUOY] END".to_owned(),
                ]))
            }
            ClientCommand::GetBuoyCount { game_name } => match self.load_buoy(&game_name).await {
                Ok(Some(doc)) => Ok(Some(vec![
                    format!("##[GETBUOYCOUNT] {game_name} {}", doc.remaining),
                    "##[GETBUOYCOUNT] END".to_owned(),
                ])),
                Ok(None) => Ok(Some(vec![
                    format!("##[GETBUOYCOUNT] NOT_FOUND {game_name}"),
                    "##[GETBUOYCOUNT] END".to_owned(),
                ])),
                Err(e) => Ok(Some(vec![
                    format!("##[GETBUOYCOUNT] ERROR {game_name} {e}"),
                    "##[GETBUOYCOUNT] END".to_owned(),
                ])),
            },
            ClientCommand::Fork {
                source_game,
                new_buoy,
                nth_move,
            } => {
                let buoy_name = new_buoy.unwrap_or_else(|| {
                    GameName::new(default_fork_buoy_name(source_game.as_str(), nth_move))
                });
                let csa_v2 = match self.load_kifu_by_game_id(&source_game).await {
                    Ok(Some(csa_v2)) => csa_v2,
                    Ok(None) => {
                        return Ok(Some(vec![
                            format!("##[FORK] NOT_FOUND {source_game}"),
                            "##[FORK] END".to_owned(),
                        ]));
                    }
                    Err(e) => {
                        return Ok(Some(vec![
                            format!("##[FORK] ERROR {buoy_name} {e}"),
                            "##[FORK] END".to_owned(),
                        ]));
                    }
                };
                let (initial_sfen, applied_moves) =
                    match fork_initial_sfen_from_kifu(&csa_v2, nth_move) {
                        Ok(v) => v,
                        Err(e) => {
                            return Ok(Some(vec![
                                format!("##[FORK] ERROR {buoy_name} {e}"),
                                "##[FORK] END".to_owned(),
                            ]));
                        }
                    };
                let doc = PersistedBuoy {
                    moves: Vec::new(),
                    remaining: 1,
                    initial_sfen: Some(initial_sfen),
                };
                if let Err(e) = self.store_buoy(&buoy_name, &doc).await {
                    return Ok(Some(vec![
                        format!("##[FORK] ERROR {buoy_name} {e}"),
                        "##[FORK] END".to_owned(),
                    ]));
                }
                Ok(Some(vec![
                    format!("##[FORK] OK {buoy_name} {applied_moves}"),
                    "##[FORK] END".to_owned(),
                ]))
            }
            _ => Ok(None),
        }
    }

    fn is_admin_handle(&self, handle: &str) -> bool {
        let configured = self.env.var(ConfigKeys::ADMIN_HANDLE).ok().map(|v| v.to_string());
        configured
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some_and(|admin| admin == handle)
    }

    async fn reserve_initial_sfen_from_buoy(
        &self,
        game_name: &GameName,
    ) -> Result<BuoyReservation> {
        // R2 には CAS プリミティブが無い代わりに conditional PUT（etag 一致時
        // のみ書き込み）が使える。`load → decrement → put(onlyIf=etag)` を
        // リトライループで回し、別 DO が同時に同じ buoy を予約してきた場合は
        // etag 不一致で put が Ok(None) に落ちるので再読み込みする。
        //
        // リトライ上限は 5 回。実運用では同一 buoy への同時アクセスは稀と
        // 見込むが、同一 game_name の room が連続して LOGIN を受けると再試行
        // が必要になり得る。上限に達したら Exhausted 相当にフォールバックせず
        // 明示的なエラーを返し、`abort_pending_match_with_error` 経由で部屋を
        // 閉じる（静かな誤受理より fail-fast の方が運用上安全）。
        const MAX_ATTEMPTS: u32 = 5;
        let bucket = self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING)?;
        let key = buoy_object_key(game_name.as_str());
        for attempt in 0..MAX_ATTEMPTS {
            let Some(obj) = bucket.get(&key).execute().await? else {
                return Ok(BuoyReservation::Missing);
            };
            let etag = obj.etag();
            let Some(body) = obj.body() else {
                return Ok(BuoyReservation::Missing);
            };
            let text = body.text().await?;
            let mut buoy: PersistedBuoy = serde_json::from_str(&text)
                .map_err(|e| Error::RustError(format!("parse buoy json: {e}")))?;
            if buoy.remaining == 0 {
                return Ok(BuoyReservation::Exhausted);
            }
            let reserved_initial_sfen = match buoy.initial_sfen.as_ref() {
                Some(sfen) => Some(sfen.clone()),
                None => {
                    let moves: Vec<CsaMoveToken> =
                        buoy.moves.iter().map(|mv| CsaMoveToken::new(mv.as_str())).collect();
                    Some(initial_sfen_from_csa_moves(&moves).map_err(Error::RustError)?)
                }
            };
            buoy.remaining -= 1;
            let payload = serde_json::to_vec(&buoy)
                .map_err(|e| Error::RustError(format!("serialize buoy json: {e}")))?;
            let put_result = bucket
                .put(&key, payload)
                .only_if(worker::Conditional {
                    etag_matches: Some(etag),
                    ..Default::default()
                })
                .execute()
                .await?;
            if put_result.is_some() {
                return Ok(BuoyReservation::Reserved(reserved_initial_sfen));
            }
            console_log!(
                "[GameRoom] buoy '{}' reservation etag mismatch, retry {}/{MAX_ATTEMPTS}",
                game_name.as_str(),
                attempt + 1,
            );
        }
        Err(Error::RustError(format!(
            "buoy '{}' reservation retry exhausted after {MAX_ATTEMPTS} attempts",
            game_name.as_str(),
        )))
    }

    async fn try_start_pending_match(&self) -> Result<bool> {
        let slots = self.load_slots().await?;
        let MatchResult::Match {
            black_handle,
            white_handle,
            game_name,
        } = evaluate_match(&slots)
        else {
            return Ok(false);
        };
        self.start_match(&black_handle, &white_handle, &game_name).await
    }

    async fn load_kifu_by_game_id(&self, game_id: &GameId) -> Result<Option<String>> {
        let bucket = self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING)?;
        let key = kifu_by_id_object_key(game_id.as_str());
        let Some(obj) = bucket.get(&key).execute().await? else {
            return Ok(None);
        };
        let Some(body) = obj.body() else {
            return Ok(None);
        };
        Ok(Some(body.text().await?))
    }

    async fn load_buoy(&self, game_name: &GameName) -> Result<Option<PersistedBuoy>> {
        let bucket = self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING)?;
        let key = buoy_object_key(game_name.as_str());
        let Some(obj) = bucket.get(&key).execute().await? else {
            return Ok(None);
        };
        let Some(body) = obj.body() else {
            return Ok(None);
        };
        let text = body.text().await?;
        let doc = serde_json::from_str::<PersistedBuoy>(&text)
            .map_err(|e| Error::RustError(format!("parse buoy json: {e}")))?;
        Ok(Some(doc))
    }

    async fn store_buoy(&self, game_name: &GameName, doc: &PersistedBuoy) -> Result<()> {
        let bucket = self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING)?;
        let key = buoy_object_key(game_name.as_str());
        let payload = serde_json::to_vec(doc)
            .map_err(|e| Error::RustError(format!("serialize buoy json: {e}")))?;
        bucket.put(&key, payload).execute().await?;
        Ok(())
    }

    async fn delete_buoy(&self, game_name: &GameName) -> Result<()> {
        let bucket = self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING)?;
        let key = buoy_object_key(game_name.as_str());
        bucket.delete(&key).await
    }

    /// 直前の `HandleOutcome` に応じて Alarm を張り替える。
    ///
    /// - `GameStarted` / `MoveAccepted`: 次手番側が使える残時間 (main + byoyomi) を
    ///   `Duration` として set_alarm に渡す。通信マージン分の安全側余裕も追加する。
    /// - `GameEnded`: 明示的に delete_alarm で解除する (set_alarm で上書きされないケースへの保険)。
    /// - `Continue`: 手番は変わらないので何もしない。
    async fn reschedule_turn_alarm(&self, outcome: &HandleOutcome) -> Result<()> {
        match outcome {
            HandleOutcome::GameStarted | HandleOutcome::MoveAccepted { .. } => {
                let budget_ms = {
                    let borrow = self.core.borrow();
                    let Some(core) = borrow.as_ref() else {
                        return Ok(());
                    };
                    let next_turn = core.current_turn();
                    core.clock_turn_budget_ms(next_turn)
                };
                let margin_ms = self
                    .config
                    .borrow()
                    .as_ref()
                    .map(|c| c.time_margin_ms)
                    .unwrap_or(DEFAULT_TIME_MARGIN_MS);
                // budget が負になるのは契約違反だが、set_alarm に負時間は渡せないので
                // 防御的に 0 へ丸める。`u64 + margin` に小さな安全側ゲタ (ALARM_SAFETY_MS)
                // を加えて、CoreRoom が deadline 未到達として直前に弾くのを防ぐ。
                let budget = budget_ms.max(0) as u64;
                let total = budget.saturating_add(margin_ms).saturating_add(ALARM_SAFETY_MS);
                self.state.storage().set_alarm(Duration::from_millis(total)).await?;
            }
            HandleOutcome::GameEnded(_) => {
                let _ = self.state.storage().delete_alarm().await;
            }
            HandleOutcome::Continue => {}
        }
        Ok(())
    }

    /// HandleResult の broadcasts を宛先色に応じて ws に送出する。
    async fn dispatch_broadcasts(&self, entries: &[BroadcastEntry]) -> Result<()> {
        for entry in entries {
            match entry.target {
                BroadcastTarget::Black => {
                    self.send_to_role(Role::Black, entry.line.as_str()).await?;
                }
                BroadcastTarget::White => {
                    self.send_to_role(Role::White, entry.line.as_str()).await?;
                }
                BroadcastTarget::Players | BroadcastTarget::All => {
                    self.send_to_role(Role::Black, entry.line.as_str()).await?;
                    self.send_to_role(Role::White, entry.line.as_str()).await?;
                }
                BroadcastTarget::Spectators => {
                    self.send_to_spectators(entry.line.as_str()).await?;
                }
            }
            if matches!(entry.target, BroadcastTarget::All) {
                self.send_to_spectators(entry.line.as_str()).await?;
            }
        }
        Ok(())
    }

    /// 同一 room の対局者 + 観戦者全員へ chat を relay する。
    async fn relay_chat(&self, sender: &str, message: &str) -> Result<()> {
        let line = format!("##[CHAT] {sender}: {message}");
        self.send_to_role(Role::Black, &line).await?;
        self.send_to_role(Role::White, &line).await?;
        self.send_to_spectators(&line).await
    }

    /// 終局したなら R2 に棋譜を書き出し、finished フラグを立てて両 ws を close する。
    async fn finalize_if_ended(&self, result: &HandleResult) -> Result<()> {
        let HandleOutcome::GameEnded(ref game_result) = result.outcome else {
            return Ok(());
        };
        use rshogi_csa_server::record::kifu::primary_result_code;
        let code = primary_result_code(game_result).to_owned();
        let ended_at_ms = self.now_ms();

        // 棋譜エクスポートは best-effort：R2 バインディングが設定されていない開発
        // 環境や一時的な put 失敗で終局処理自体を止めないよう、ログだけ残して続行する。
        if let Err(e) = self.export_kifu_to_r2(game_result, ended_at_ms).await {
            console_log!("[GameRoom] kifu export failed: {e:?}");
        }

        let finished = FinishedState {
            result_code: code,
            ended_at_ms,
        };
        self.state.storage().put(KEY_FINISHED, &finished).await?;

        // CoreRoom を落とす。再度 ensure_core_loaded しても finished ガードで戻る。
        self.core.borrow_mut().take();

        // 両 ws を穏やかに閉じる。
        for ws in self.state.get_websockets() {
            let _ = ws.close(Some(1000), Some("game finished".to_owned()));
        }
        Ok(())
    }

    /// R2 バケットに CSA V2 形式の棋譜を書き出す。
    ///
    /// キー体系: `YYYY/MM/DD/<game_id>.csa`。TCP 版 `FileKifuStorage` と同一
    /// 構造なので、外部のレート集計や HTML レンダリングなどの後段処理は R2 を
    /// mount するだけで TCP 版と同じパスで読める。
    async fn export_kifu_to_r2(
        &self,
        game_result: &rshogi_csa_server::game::result::GameResult,
        ended_at_ms: u64,
    ) -> Result<()> {
        use rshogi_csa_server::record::kifu::{KifuMove, KifuRecord};

        let cfg = match self.config.borrow().as_ref() {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        let moves_rows = self.load_moves().await?;
        // MoveRow は raw CSA 行（例: `+7776FU,T3`）を保持しているので、トークン部のみを
        // 抽出して `KifuMove` に変換する。消費時間は at_ms 差分から秒に丸める。
        let mut kifu_moves: Vec<KifuMove> = Vec::with_capacity(moves_rows.len());
        let mut prev_ts: u64 = cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms);
        for m in &moves_rows {
            let token_str = m.line.split(',').next().unwrap_or(&m.line);
            let at_ms = m.at_ms.max(0) as u64;
            let elapsed_ms = at_ms.saturating_sub(prev_ts);
            prev_ts = at_ms;
            kifu_moves.push(KifuMove {
                token: rshogi_csa_server::types::CsaMoveToken::new(token_str),
                elapsed_sec: (elapsed_ms / 1000) as u32,
                comment: None,
            });
        }

        // `time_section` は clock の初期設定値に依存し、持ち時間の残量には
        // 左右されないので cfg から再構築しても同じ出力になる。
        let time_section = cfg.clock.format_time_section();

        let start_str = format_csa_datetime(cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms));
        let end_str = format_csa_datetime(ended_at_ms);

        let record = KifuRecord {
            game_id: GameId::new(cfg.game_id.clone()),
            black: PlayerName::new(cfg.black_handle.clone()),
            white: PlayerName::new(cfg.white_handle.clone()),
            start_time: start_str,
            end_time: end_str,
            event: String::new(),
            time_section,
            // Game_Summary の position_section と同じ SFEN 由来のブロックを使う。
            // 三点一致契約 (CoreRoom / Summary / 棋譜 initial_position) の R2 側。
            initial_position: match cfg.initial_sfen.as_deref() {
                Some(sfen) => position_section_from_sfen(sfen).map_err(Error::RustError)?,
                None => standard_initial_position_block(),
            },
            moves: kifu_moves,
            result: game_result.clone(),
        };
        let text = record.build_v2();

        let date_path = format_date_path(cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms));
        let key = format!("{date_path}/{}.csa", cfg.game_id);
        let by_id_key = kifu_by_id_object_key(&cfg.game_id);

        let bucket = self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING)?;
        bucket.put(&key, text.as_bytes().to_vec()).execute().await?;
        bucket.put(&by_id_key, text.as_bytes().to_vec()).execute().await?;
        console_log!("[GameRoom] kifu exported to R2 key='{key}'");
        Ok(())
    }

    /// マッチ開始直前の致命的条件（buoy 枯渇等）で対局を開始できない場合に、
    /// 既に LOGIN OK を受けている Player ロールの WS 全員にエラー行を送出し、
    /// 接続を閉じてスロットを空にする。
    ///
    /// ここでクリアしないと、スロットは Match 状態のまま残ってしまい 2 人目に
    /// Game_Summary もエラーも届かないため、部屋が永久に詰まる (codex review
    /// PR #474 P2)。
    async fn abort_pending_match_with_error(&self, error_line: &str) -> Result<()> {
        for ws in self.state.get_websockets() {
            let att: Option<WsAttachment> = ws.deserialize_attachment().ok().flatten();
            if matches!(att, Some(WsAttachment::Player { .. })) {
                let _ = send_line(&ws, error_line);
                let _ = ws.close(Some(1011), Some("match aborted".to_owned()));
            }
        }
        self.state.storage().put(KEY_SLOTS, &Vec::<Slot>::new()).await?;
        Ok(())
    }

    /// 指定 Role の WebSocket に 1 行送出する。該当 ws が無ければ何もしない。
    async fn send_to_role(&self, role: Role, line: &str) -> Result<()> {
        for ws in self.state.get_websockets() {
            let att: Option<WsAttachment> = ws.deserialize_attachment().ok().flatten();
            if let Some(WsAttachment::Player { role: r, .. }) = att {
                if r == role {
                    send_line(&ws, line)?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// 全観戦者へ 1 行送出する。
    ///
    /// 観戦者は best-effort 配信。特定の WS への書き込みが失敗しても他の
    /// 観戦者や対局進行を止めず、エラーは log に落として継続する (Copilot
    /// レビュー指摘)。観戦者 1 人の切断が DO を不安定化させないようにする。
    async fn send_to_spectators(&self, line: &str) -> Result<()> {
        for ws in self.state.get_websockets() {
            let att: Option<WsAttachment> = ws.deserialize_attachment().ok().flatten();
            if let Some(WsAttachment::Spectator { .. }) = att
                && let Err(e) = send_line(&ws, line)
            {
                console_log!("[GameRoom] spectator send failed (ignored): {e:?}");
            }
        }
        Ok(())
    }

    /// 現在アクティブな `game_id` を返す。マッチ成立前は `None`。
    async fn active_game_id(&self) -> Result<Option<String>> {
        if let Some(cfg) = self.config.borrow().as_ref() {
            return Ok(Some(cfg.game_id.clone()));
        }
        let cfg_opt: Option<PersistedConfig> = self.state.storage().get(KEY_CONFIG).await?;
        Ok(cfg_opt.map(|cfg| cfg.game_id))
    }

    /// 応答に載せる現在の観戦対象 ID。対局中は `game_id`、それ以前は `room_id`。
    async fn current_monitor_id(&self) -> Result<String> {
        if let Some(game_id) = self.active_game_id().await? {
            return Ok(game_id);
        }
        let room_id: Option<String> = self.state.storage().get(KEY_ROOM_ID).await?;
        Ok(room_id.unwrap_or_else(|| "unknown".to_owned()))
    }

    /// CoreRoom が in-memory に無ければ永続化から復元する。
    ///
    /// 復元ステップ:
    /// 1. 既に in-memory にコアがあれば即 return。終局済みフラグが立っていても
    ///    新しいコアを作らずに return（同 DO で同対局が再開しないことの保証）。
    /// 2. `KEY_CONFIG` (`PersistedConfig`) を読み、無ければ何もしない。
    /// 3. `play_started_at_ms` が立っているときだけ `moves` テーブルを読み込む。
    /// 4. `crate::persistence::replay_core_room` に委譲して新しい `CoreRoom` を
    ///    組み立てる。成功時は in-memory にセット、失敗 variant は console_log で
    ///    記録するだけでコアを生成しない（結果整合性を優先）。
    ///
    /// # 既知の制約
    /// - AGREE 完了だが 1 手目未指の状態で isolate が破棄された場合は、
    ///   `play_started_at_ms` が `Some(t)` であれば AGREE を再送して `Playing`
    ///   に復帰する（cold start 復元時に alarm による time-up が発火できる経路
    ///   を維持する）。`play_started_at_ms` が `None` なら `AgreeWaiting` のまま。
    /// - 復元中の `handle_line` 失敗（`AgreeReplayFailed` / `MoveReplayFailed` 等）
    ///   ではコアを生成せず、以降の着手受理を拒絶する。
    async fn ensure_core_loaded(&self) -> Result<()> {
        if self.core.borrow().is_some() {
            return Ok(());
        }
        if self.load_finished().await?.is_some() {
            return Ok(());
        }
        let cfg_opt: Option<PersistedConfig> = self.state.storage().get(KEY_CONFIG).await?;
        let Some(cfg) = cfg_opt else {
            return Ok(());
        };
        // moves replay は I/O 非依存に分離した `replay_core_room` に委譲する。
        // 永続化レイヤとの境界は `load_moves()` の戻り値だけで、replay 中の状態
        // 復元は I/O を持たない純粋関数として `crate::persistence` 側でホスト
        // target から網羅テストされている (cold start シナリオの状態完全一致 +
        // 失敗系の分岐被覆)。
        let moves = if cfg.play_started_at_ms.is_some() {
            self.load_moves().await?
        } else {
            Vec::new()
        };
        match replay_core_room(&cfg, &moves) {
            ReplaySummary::Restored { core } => {
                // `core` は `Box<CoreRoom>` で返るためここで unbox する
                // (`ReplaySummary` の variant 間サイズ差対策、persistence.rs 参照)。
                *self.core.borrow_mut() = Some(*core);
                *self.config.borrow_mut() = Some(cfg);
            }
            ReplaySummary::InvalidSfen { reason } => {
                console_log!("[GameRoom] replay CoreRoom::new failed: {reason}");
            }
            ReplaySummary::UnknownColor { ply, color } => {
                console_log!("[GameRoom] replay: unknown color '{color}' at ply={ply}");
            }
            ReplaySummary::MoveReplayFailed { ply, line, reason } => {
                console_log!("[GameRoom] replay move ply={ply} line='{line}' failed: {reason}");
            }
        }
        Ok(())
    }

    /// 初めて `HandleOutcome::GameStarted` を観測した時刻を cfg に書き込む。
    /// 2 手目以降は冪等に no-op として扱い、storage への再書き込みを避ける。
    async fn mark_play_started(&self, ts: u64) -> Result<()> {
        let new_cfg = {
            let mut borrow = self.config.borrow_mut();
            match borrow.as_mut() {
                Some(c) if c.play_started_at_ms.is_none() => {
                    c.play_started_at_ms = Some(ts);
                    Some(c.clone())
                }
                _ => None,
            }
        };
        if let Some(c) = new_cfg {
            self.state.storage().put(KEY_CONFIG, &c).await?;
        }
        Ok(())
    }

    /// `moves` テーブルを ply 昇順で読み出す。
    async fn load_moves(&self) -> Result<Vec<MoveRow>> {
        let sql = self.state.storage().sql();
        let cursor =
            sql.exec("SELECT ply, color, line, at_ms FROM moves ORDER BY ply ASC", None)?;
        let rows: Vec<MoveRow> = cursor.to_array()?;
        Ok(rows)
    }

    async fn load_slots(&self) -> Result<Vec<Slot>> {
        let v: Option<Vec<Slot>> = self.state.storage().get(KEY_SLOTS).await?;
        Ok(v.unwrap_or_default())
    }

    async fn load_finished(&self) -> Result<Option<FinishedState>> {
        self.state.storage().get(KEY_FINISHED).await
    }

    async fn append_move(&self, color: Color, line: &str, now_ms: u64) -> Result<()> {
        let sql = self.state.storage().sql();
        // `COALESCE(MAX(ply), 0) + 1` を採用: 仮に未来のメンテナンス等で moves を
        // 一部削除しても PRIMARY KEY 衝突を避けられる。`COUNT(*) + 1` は削除後の
        // ply とぶつかる危険があるため選ばない。
        let cursor = sql.exec("SELECT COALESCE(MAX(ply), 0) + 1 AS n FROM moves", None)?;
        #[derive(Deserialize)]
        struct CountRow {
            n: i64,
        }
        let rows: Vec<CountRow> = cursor.to_array()?;
        let next_ply = rows.first().map(|r| r.n).unwrap_or(1);
        let color_str = match color {
            Color::Black => "black",
            Color::White => "white",
        };
        sql.exec(
            "INSERT INTO moves(ply, color, line, at_ms) VALUES (?, ?, ?, ?)",
            vec![
                next_ply.into(),
                color_str.into(),
                line.into(),
                (now_ms as i64).into(),
            ],
        )?;
        Ok(())
    }
}

/// 末尾改行を付けて 1 行送出する。CSA 行は改行終端が契約なので、
/// アダプタレイヤ（この関数）で 1 箇所に集約する。
fn send_line(ws: &WebSocket, line: &str) -> Result<()> {
    let mut out = String::with_capacity(line.len() + 1);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
    ws.send_with_str(&out)
        .map_err(|e| Error::RustError(format!("send_with_str: {e}")))
}

fn load_clock_spec_from_env(env: &Env) -> Result<ClockSpec> {
    let clock_kind = env.var(ConfigKeys::CLOCK_KIND).ok().map(|v| v.to_string());
    let total_time_sec = env.var(ConfigKeys::TOTAL_TIME_SEC).ok().map(|v| v.to_string());
    let byoyomi_sec = env.var(ConfigKeys::BYOYOMI_SEC).ok().map(|v| v.to_string());
    let total_time_min = env.var(ConfigKeys::TOTAL_TIME_MIN).ok().map(|v| v.to_string());
    let byoyomi_min = env.var(ConfigKeys::BYOYOMI_MIN).ok().map(|v| v.to_string());
    parse_clock_spec(
        clock_kind.as_deref(),
        total_time_sec.as_deref(),
        byoyomi_sec.as_deref(),
        total_time_min.as_deref(),
        byoyomi_min.as_deref(),
    )
    .map_err(Error::RustError)
}
