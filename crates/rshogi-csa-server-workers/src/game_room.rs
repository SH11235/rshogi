//! `GameRoom` Durable Object の対局ロジック実装 (Phase 2-9.4)。
//!
//! 1 部屋 = 1 DO インスタンス。以下のライフサイクルを駆動する:
//!
//! 1. **WebSocket Upgrade** (`fetch`): [`WsAttachment::Pending`] を付けて
//!    `state.accept_web_socket` で hibernation を有効化する。
//! 2. **LOGIN** (`websocket_message` / pending): `<handle>+<game_name>+<color>`
//!    形式を分解し、役割 (Role) 付きスロットとして [`state.storage().put`] に
//!    保存する。WS 側の attachment も `Player` に差し替える。Phase 2-9.4 MVP では
//!    認証は *accept-all* の stub（Phase 4 で `RateStorage` + PasswordHasher 互換に差し替え）。
//! 3. **マッチ成立**: 2 人目の LOGIN で役割が相補、同じ game_name なら
//!    [`CoreRoom`] を生成して Game_Summary を双方へ送出する。状態は
//!    `AgreeWaiting` として Core 側が握る。
//! 4. **対局中の行受信** (`websocket_message` / player): attachment から Color を
//!    取り出し、[`CoreRoom::handle_line`] に流して `HandleResult::broadcasts` を
//!    宛先色別に fanout する。着手は `moves` テーブルに append する。
//! 5. **切断** (`websocket_close`): 認証済みプレイヤの切断は
//!    [`CoreRoom::force_abnormal`] で敗北を確定する。
//!
//! 6. **時間切れ駆動** (`alarm`): 手番開始ごとに `state.storage().set_alarm`
//!    で deadline を予約し、到着した時に `CoreRoom::force_time_up(current_turn)`
//!    で負け側を確定する (§9.5)。
//! 7. **再起動復元** (`ensure_core_loaded`): DO isolate が破棄された後の
//!    最初の操作で、`moves` テーブルを ply 順に `handle_line` で replay し
//!    CoreRoom を再構築する (§9.4 の「再起動復元」要件)。
//!
//! # 未実装（後続タスク）
//!
//! - §9.6 R2 への CSA V2 棋譜エクスポート（GameEnded 時）。

use std::cell::RefCell;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use worker::{
    Date, DurableObject, Env, Error, Request, Response, ResponseBuilder, Result, State, WebSocket,
    WebSocketIncomingMessage, WebSocketPair, console_log, durable_object, wasm_bindgen,
};

use rshogi_core::types::EnteringKingRule;
use rshogi_csa_server::game::clock::SecondsCountdownClock;
use rshogi_csa_server::game::clock::TimeClock;
use rshogi_csa_server::game::room::{
    BroadcastEntry, BroadcastTarget, GameRoom as CoreRoom, GameRoomConfig, HandleOutcome,
    HandleResult,
};
use rshogi_csa_server::protocol::command::{ClientCommand, parse_command};
use rshogi_csa_server::protocol::summary::{GameSummaryBuilder, standard_initial_position_block};
use rshogi_csa_server::types::{Color, CsaLine, GameId, PlayerName};

use crate::attachment::{Role, WsAttachment, parse_login_handle};
use crate::phase_gate;
use crate::session_state::{LoginReply, MatchResult, Slot, evaluate_match};

/// Phase 2-9.4 MVP の時計既定値 (Floodgate 600-10 互換)。
const DEFAULT_MAIN_TIME_SEC: u32 = 600;
const DEFAULT_BYOYOMI_SEC: u32 = 10;
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
const SCHEMA_SQL: &str = "\nCREATE TABLE IF NOT EXISTS moves (\n    ply INTEGER PRIMARY KEY,\n    color TEXT NOT NULL,\n    line TEXT NOT NULL,\n    at_ms INTEGER NOT NULL\n);\n";

const KEY_SLOTS: &str = "slots";
const KEY_CONFIG: &str = "config";
const KEY_FINISHED: &str = "finished";

/// マッチ成立時に永続化する対局設定。CoreRoom の再構築に必要な最小情報。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedConfig {
    game_id: String,
    black_handle: String,
    white_handle: String,
    game_name: String,
    main_time_sec: u32,
    byoyomi_sec: u32,
    max_moves: u32,
    time_margin_ms: u64,
}

/// 終局フラグ。一度 `Some` になったらその DO は同じ対局を二度開始しない。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FinishedState {
    result_code: String,
    ended_at_ms: u64,
}

/// `moves` テーブル 1 行分。replay / alarm で使う。
#[derive(Debug, Clone, Deserialize)]
struct MoveRow {
    ply: i64,
    color: String,
    line: String,
    at_ms: i64,
}

/// 1 対局分の Durable Object。
#[durable_object]
pub struct GameRoom {
    state: State,
    core: RefCell<Option<CoreRoom>>,
    config: RefCell<Option<PersistedConfig>>,
}

impl DurableObject for GameRoom {
    fn new(state: State, _: Env) -> Self {
        let sql = state.storage().sql();
        sql.exec(SCHEMA_SQL, None).expect("failed to initialize DO schema");
        Self {
            state,
            core: RefCell::new(None),
            config: RefCell::new(None),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        if !url.path().starts_with("/ws/") {
            return Response::error("Upgrade required", 426);
        }

        let pair = WebSocketPair::new()?;
        let server = pair.server;
        self.state.accept_web_socket(&server);

        let pending = WsAttachment::Pending;
        server
            .serialize_attachment(&pending)
            .map_err(|e| Error::RustError(format!("serialize_attachment: {e}")))?;

        console_log!("[GameRoom] websocket upgrade accepted ({})", phase_gate::PhaseGate::label());

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
                self.handle_game_line(role, &handle, &line).await
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
        let att: Option<WsAttachment> = ws.deserialize_attachment().ok().flatten();
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
            let loser = current_turn_color(core.moves_played());
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
            self.start_match(&black_handle, &white_handle, &game_name).await?;
        }

        Ok(())
    }

    /// マッチ成立時の処理: CoreRoom 作成 + Game_Summary 送出。
    async fn start_match(
        &self,
        black_handle: &str,
        white_handle: &str,
        game_name: &str,
    ) -> Result<()> {
        // Phase 2-9.4 MVP では game_id を DO 時刻ベースで生成する。
        let started = self.now_ms();
        let game_id = format!("{started}");

        let cfg = PersistedConfig {
            game_id: game_id.clone(),
            black_handle: black_handle.to_owned(),
            white_handle: white_handle.to_owned(),
            game_name: game_name.to_owned(),
            main_time_sec: DEFAULT_MAIN_TIME_SEC,
            byoyomi_sec: DEFAULT_BYOYOMI_SEC,
            max_moves: DEFAULT_MAX_MOVES,
            time_margin_ms: DEFAULT_TIME_MARGIN_MS,
        };
        self.state.storage().put(KEY_CONFIG, &cfg).await?;

        // CoreRoom を構築して in-memory に置く。
        let clock: Box<dyn TimeClock> =
            Box::new(SecondsCountdownClock::new(cfg.main_time_sec, cfg.byoyomi_sec));
        let time_section = clock.format_summary();
        let core = CoreRoom::new(
            GameRoomConfig {
                game_id: GameId::new(cfg.game_id.clone()),
                black: PlayerName::new(cfg.black_handle.clone()),
                white: PlayerName::new(cfg.white_handle.clone()),
                max_moves: cfg.max_moves,
                time_margin_ms: cfg.time_margin_ms,
                entering_king_rule: EnteringKingRule::Point24,
            },
            clock,
        );
        *self.core.borrow_mut() = Some(core);
        *self.config.borrow_mut() = Some(cfg.clone());

        // Game_Summary を双方に送出（Your_Turn だけ色で変える）。
        let builder = GameSummaryBuilder {
            game_id: GameId::new(cfg.game_id),
            black: PlayerName::new(cfg.black_handle),
            white: PlayerName::new(cfg.white_handle),
            time_section,
            position_section: standard_initial_position_block(),
            rematch_on_draw: false,
            to_move: Color::Black,
            declaration: String::new(),
        };
        let summary_black = builder.build_for(Color::Black);
        let summary_white = builder.build_for(Color::White);

        self.send_to_role(Role::Black, &summary_black).await?;
        self.send_to_role(Role::White, &summary_white).await?;

        Ok(())
    }

    /// 対局中のプレイヤからの行を CoreRoom に流す。
    async fn handle_game_line(&self, role: Role, handle: &str, line: &str) -> Result<()> {
        if self.load_finished().await?.is_some() {
            // 終局後に届いた行は無視する。
            return Ok(());
        }

        self.ensure_core_loaded().await?;
        let now = self.now_ms();
        let color = role.to_core();
        let csa = CsaLine::new(line);

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

        // 着手を永続化。MoveAccepted の場合のみ moves テーブルに append する。
        if let HandleOutcome::MoveAccepted { .. } = result.outcome {
            self.append_move(color, line, now).await?;
        }

        self.dispatch_broadcasts(&result.broadcasts).await?;
        self.reschedule_turn_alarm(&result.outcome).await?;
        self.finalize_if_ended(&result).await?;
        Ok(())
    }

    /// 直前の `HandleOutcome` に応じて Alarm を張り替える (§9.5)。
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
                    let next_turn = current_turn_color(core.moves_played());
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
                // Phase 2 MVP では観戦者を持たないので Spectators は送る宛先なし。
                BroadcastTarget::Spectators => {}
            }
        }
        Ok(())
    }

    /// 終局したなら finished フラグを立て、両 ws を close して後片付けする。
    async fn finalize_if_ended(&self, result: &HandleResult) -> Result<()> {
        let HandleOutcome::GameEnded(ref game_result) = result.outcome else {
            return Ok(());
        };
        use rshogi_csa_server::record::kifu::primary_result_code;
        let code = primary_result_code(game_result).to_owned();
        let finished = FinishedState {
            result_code: code,
            ended_at_ms: self.now_ms(),
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

    /// CoreRoom が in-memory に無ければ永続化から復元する（`moves` replay 付き）。
    ///
    /// 復元ステップ:
    /// 1. `KEY_CONFIG` から `GameRoomConfig` を再構築し、新しい `CoreRoom` を作る。
    /// 2. 既に終局済みなら config が残っていても core を作らずに早期 return。
    /// 3. `moves` テーブルに着手が 1 手以上あれば、両プレイヤは必ず AGREE 済みとみなし、
    ///    記録されている最初の着手時刻をもって AGREE を二度流し、Playing 状態へ
    ///    遷移させる。その後、ply 順に `handle_line` で差し手を再送する。
    ///
    /// # 既知の制約
    /// - AGREE 完了だが 1 手目未指の状態で isolate が破棄された場合、復元後の
    ///   CoreRoom は `AgreeWaiting` に戻る。両者が再 AGREE を送れば再開できる。
    /// - 復元中に `handle_line` が `Err` を返したら core は生成せず、以降の着手
    ///   受理を拒絶する（結果整合性を優先）。
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
        let clock: Box<dyn TimeClock> =
            Box::new(SecondsCountdownClock::new(cfg.main_time_sec, cfg.byoyomi_sec));
        let mut core = CoreRoom::new(
            GameRoomConfig {
                game_id: GameId::new(cfg.game_id.clone()),
                black: PlayerName::new(cfg.black_handle.clone()),
                white: PlayerName::new(cfg.white_handle.clone()),
                max_moves: cfg.max_moves,
                time_margin_ms: cfg.time_margin_ms,
                entering_king_rule: EnteringKingRule::Point24,
            },
            clock,
        );

        // moves 再送。AGREE は手として永続化しないため、moves が存在するなら
        // 両者 AGREE 済みと確定できる（そうでないと MoveAccepted に至らない）。
        let moves = self.load_moves().await?;
        if !moves.is_empty() {
            // SQLite 側は i64 で保存するが CoreRoom の API は u64 ミリ秒。
            // 過去のタイムスタンプなので非負前提で cast する（負値は防御的に 0）。
            let first_ts = moves[0].at_ms.max(0) as u64;
            for color in [Color::Black, Color::White] {
                if let Err(e) = core.handle_line(color, &CsaLine::new("AGREE"), first_ts) {
                    console_log!("[GameRoom] replay AGREE failed: {e:?}");
                    return Ok(());
                }
            }
            for m in &moves {
                let color = match m.color.as_str() {
                    "black" => Color::Black,
                    "white" => Color::White,
                    _ => {
                        console_log!("[GameRoom] replay: unknown color '{}'", m.color);
                        return Ok(());
                    }
                };
                let ts = m.at_ms.max(0) as u64;
                if let Err(e) = core.handle_line(color, &CsaLine::new(&m.line), ts) {
                    console_log!(
                        "[GameRoom] replay move ply={} line='{}' failed: {e:?}",
                        m.ply,
                        m.line
                    );
                    return Ok(());
                }
            }
        }

        *self.core.borrow_mut() = Some(core);
        *self.config.borrow_mut() = Some(cfg);
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
        let count_cursor = sql.exec("SELECT COUNT(*) AS n FROM moves", None)?;
        #[derive(Deserialize)]
        struct CountRow {
            n: i64,
        }
        let rows: Vec<CountRow> = count_cursor.to_array()?;
        let next_ply = rows.first().map(|r| r.n).unwrap_or(0) + 1;
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

/// 既消費手数から次の手番色を導出する。平手開始は先手 (Black) なので偶数手目の
/// 次手番は Black、奇数手目の次手番は White。replay 後や Alarm 起動時に呼ぶ。
fn current_turn_color(moves_played: u32) -> Color {
    if moves_played % 2 == 0 {
        Color::Black
    } else {
        Color::White
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
