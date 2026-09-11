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

use std::cell::{Cell, RefCell};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use worker::{
    Date, Delay, DurableObject, Env, Error, Request, Response, ResponseBuilder, Result, State,
    WebSocket, WebSocketIncomingMessage, WebSocketPair, durable_object, wasm_bindgen,
};

use rshogi_csa_server::ClockSpec;
use rshogi_csa_server::config::{
    FloodgateFeatureIntent, parse_allow_floodgate_features, validate_floodgate_feature_gate,
};
use rshogi_csa_server::game::clock::TimeClock;
use rshogi_csa_server::game::room::{
    BroadcastEntry, BroadcastTarget, GameRoom as CoreRoom, GameRoomConfig, HandleOutcome,
    HandleResult,
};
use rshogi_csa_server::protocol::command::{ClientCommand, ReconnectRequest, parse_command};
use rshogi_csa_server::protocol::summary::position_section_from_position;
use rshogi_csa_server::protocol::summary::{
    GameSummaryBuilder, position_section_from_sfen, side_to_move_from_sfen,
    standard_initial_position_block,
};
use rshogi_csa_server::record::kifu::{
    fork_initial_sfen_from_kifu, initial_sfen_from_csa_moves, winner_of,
};
use rshogi_csa_server::types::{
    Color, CsaLine, CsaMoveToken, GameId, GameName, PlayerName, ReconnectToken,
};
use rshogi_csa_server::{FloodgateHistoryEntry, FloodgateHistoryStorage, HistoryColor};

use crate::attachment::{
    MAX_SPECTATOR_QUEUE_BYTES, MAX_SPECTATOR_QUEUE_ITEMS, MAX_WS_LINE_BYTES, Role, WsAttachment,
    parse_login_handle,
};
use crate::config::{
    ConfigKeys, DEFAULT_ENTERING_KING_RULE, parse_agree_timeout_duration, parse_clock_presets,
    parse_clock_spec, resolve_entering_king_rule_from_string, resolve_reconnect_grace_from_strings,
};
use crate::datetime::{format_csa_datetime, format_date_path, format_rfc3339_utc};
use crate::export_retry::{is_exhausted, next_retry_delay_ms};
use crate::floodgate_history::R2FloodgateHistoryStorage;
use crate::games_index::{
    ClockSpec as IndexClockSpec, GamesIndexEntry, classify_result, games_index_key,
    resolve_index_source,
};
use crate::handle_auth::{HandleAuthError, load_handle_auth_registry};
use crate::live_games_index::{LiveGamesIndexEntry, live_games_index_key};
use crate::persistence::{
    ExportBodyKind, ExportPendingState, FailedExportObject, FinalizingState, FinishedState,
    MoveRow, PersistedConfig, ReplaySummary, pending_finalization, replay_core_room,
};
use crate::player_identity::{derive_player_identity, legacy_player_id};
use crate::reconnect::{
    PendingAlarmKind, PendingReconnect, ReconnectMatchOutcome, ReconnectSnapshot, StartMatchGuard,
    build_resume_message, classify_alarm_after_enter_grace, classify_start_match_guard,
    color_from_str, color_to_str, issue_tokens_if_enabled,
};
use crate::session_state::{LoginReply, MatchResult, Slot, evaluate_match};
use crate::spectator_control::{
    MonitorDecision, resolve_monitor_target, resolve_monitor_target_with_finished,
};
use crate::spectator_snapshot::{
    SpectatorClocks, SpectatorSnapshotInput, build_spectator_clock_update,
    build_spectator_snapshot, initial_spectator_clocks, is_move_broadcast, move_elapsed_secs,
    move_rows_from_exported_csa, parse_move_row_line, spectator_clock_insert_after,
};
use crate::ws_route::{WsRoute, parse_ws_route};
use crate::x1_paths::{
    BUOY_KEY_PREFIX, buoy_object_key, default_fork_buoy_name, kifu_by_id_meta_key,
    kifu_by_id_object_key,
};

const DEFAULT_MAX_MOVES: u32 = 256;
const DEFAULT_TIME_MARGIN_MS: u64 = 1000;

/// 1 部屋あたりの観戦者同時接続上限。`fetch` で `/ws/<id>/spectate` の upgrade
/// 時にカウントし、上限に達していたら accept 後に 1013 で閉じる。MVP の DDoS
/// 防御として最低限の gating であり、ベンチで上限を見直す際は環境変数化を検討する
/// (現状は const)。
const MAX_SPECTATORS_PER_ROOM: usize = 50;

/// Alarm 発火時刻に上乗せする安全側マージン（ミリ秒）。Cloudflare Alarm API
/// のジッタと `Date::now()` ↔ `handle_line` の now_ms 伝搬遅延を吸収する。
const ALARM_SAFETY_MS: u64 = 200;

/// `try_delete_live_games_index` の delete 試行上限 (https://github.com/SH11235/rshogi/issues/629)。R2 delete は
/// idempotent なので transient error は積極的に retry する。
const LIVE_INDEX_DELETE_MAX_ATTEMPTS: u32 = 3;

/// `try_delete_live_games_index` の attempt 間 backoff (ミリ秒、https://github.com/SH11235/rshogi/issues/629)。
/// 配列長 = `LIVE_INDEX_DELETE_MAX_ATTEMPTS - 1`。最終 attempt の後は backoff
/// せず giveup ログに抜けるため、最後の値は使われない。`100, 200` で合計 wall
/// 300ms 以内に収める (Workers cron の 30s 制限を圧迫しない)。
///
/// 配列長の不変条件は下記 `const _: () = assert!(...)` で **コンパイル時** に gate
/// する (https://github.com/SH11235/rshogi/issues/654)。将来 `MAX_ATTEMPTS` を
/// 変更する際にこの配列も同時に更新しないとビルドが失敗するため、runtime
/// out-of-bounds を防ぐ。
const LIVE_INDEX_DELETE_BACKOFF_MS: [u64; 2] = [100, 200];

const _: () = assert!(
    LIVE_INDEX_DELETE_BACKOFF_MS.len() as u32 == LIVE_INDEX_DELETE_MAX_ATTEMPTS - 1,
    "LIVE_INDEX_DELETE_BACKOFF_MS の長さは LIVE_INDEX_DELETE_MAX_ATTEMPTS - 1 と一致させてください \
     (最終 attempt の後は backoff せず giveup するため、配列要素は MAX_ATTEMPTS - 1 個)"
);

/// Durable Object 初期化 SQL。
///
/// moves と ply ごとの時計補償を SQL で持つ（append と ply 順 replay の効率を理由に）。
/// 他の構造化状態 (slots / config / finished) は `state.storage().put/get` で
/// JSON として置き、スキーママイグレーションを軽くする。
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS moves (
    ply INTEGER PRIMARY KEY,
    color TEXT NOT NULL,
    line TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS reconnect_clock_credits (
    ply INTEGER PRIMARY KEY CHECK (ply >= 1),
    credited_ms INTEGER NOT NULL CHECK (credited_ms >= 0)
);
"#;

const KEY_ROOM_ID: &str = "room_id";
const KEY_SLOTS: &str = "slots";
const KEY_CONFIG: &str = "config";
const KEY_FINISHED: &str = "finished";
const KEY_FINALIZING: &str = "finalizing";
const FINALIZE_RETRY_DELAYS_SEC: [u64; 5] = [5, 30, 120, 600, 3600];
/// 切断 → 再接続待ちエントリの DO storage key (1 対局 = 0..=1 件)。
const KEY_GRACE_REGISTRY: &str = "grace_registry";
/// 次に発火する `state.alarm()` の種別タグ。`None` は alarm 未予約 / 既存 alarm
/// が時間切れ駆動 (TimeUp) であることを示す。grace 経路に入ったときだけ
/// `GraceExpired` を書き込む。
const KEY_PENDING_ALARM_KIND: &str = "pending_alarm_kind";
/// 終局時に R2 export PUT が一部または全部失敗したときに、再 PUT に必要な
/// CSA 本文 / meta JSON / 失敗 key 一覧を保持する DO storage key (R2 export retry)。
/// `KEY_PENDING_ALARM_KIND = ExportRetry` とペアでセットされ、`alarm()` 経路で
/// `handle_export_retry_alarm` が読み取る。retry 全消費後は alarm のみ停止し
/// この key は残置する (運用観測用)。
const KEY_EXPORT_PENDING: &str = "export_pending";

/// 終局時 R2 export 試行の結果分類 (R2 export retry)。
///
/// `export_kifu_to_r2` はこの enum を返し、呼び出し側 (`finalize_if_ended`) が
/// `Complete` / `Pending` / `Skipped` に応じて pending 永続化 + retry alarm
/// 予約 / 何もしない を分岐する。
#[derive(Debug)]
enum ExportAttempt {
    /// 4 オブジェクト (csa 本文 / by-id / meta / games-index) すべて PUT 成功
    /// (`exported_at_ms = Some` を埋められる)。
    Complete,
    /// 1 つ以上 PUT 失敗で retry 必要。この variant が持つ
    /// [`ExportPendingState`] を `KEY_EXPORT_PENDING` に永続化する。
    Pending(Box<ExportPendingState>),
    /// retry しても解決しない致命的失敗 (bucket binding 不在 / load_moves 失敗 /
    /// SFEN 不正 / serialize 失敗 / games_index_key 生成失敗等)。
    /// `exported_at_ms = None` のままで観測上は欠損として残るが、それ以外の処理
    /// は通常通り進める。retry できるはずの CSA PUT 失敗が混在している場合は
    /// `Pending` に倒し、CSA 部分だけ retry する。
    Skipped,
}

impl ExportAttempt {
    /// 4 オブジェクト全てが PUT 試行済の経路から呼ぶコンストラクタ。
    /// `failed_keys` が空 = 全成功 → `Complete`、非空 → `Pending`。
    fn from_full_attempt(
        game_id: String,
        ended_at_ms: u64,
        csa_text: String,
        meta_body: Vec<u8>,
        failed_keys: Vec<FailedExportObject>,
    ) -> Self {
        if failed_keys.is_empty() {
            ExportAttempt::Complete
        } else {
            ExportAttempt::Pending(Box::new(ExportPendingState {
                game_id,
                ended_at_ms,
                csa_text,
                meta_body,
                failed_keys,
                attempt: 0,
            }))
        }
    }

    /// 4 オブジェクト中いずれかを **PUT 試行できなかった** 経路から呼ぶ
    /// コンストラクタ。serialize 失敗 / games_index_key 生成失敗で
    /// meta or index PUT が抜けたケースなど。
    ///
    /// retry 可能な CSA PUT 失敗が `failed_keys` にあれば `Pending` に倒し、
    /// CSA 部分だけ再試行する (`exported_at_ms` は引き続き `None`)。
    /// `failed_keys` が空 (= 4 中 N 個 PUT 成功 / 残りは retry 不能で skip) なら
    /// `Skipped` を返し、`Complete` を**絶対に返さない** (= `exported_at_ms` を
    /// 埋めない)。
    fn from_partial_attempt(
        game_id: String,
        ended_at_ms: u64,
        csa_text: String,
        meta_body: Vec<u8>,
        failed_keys: Vec<FailedExportObject>,
    ) -> Self {
        if failed_keys.is_empty() {
            ExportAttempt::Skipped
        } else {
            ExportAttempt::Pending(Box::new(ExportPendingState {
                game_id,
                ended_at_ms,
                csa_text,
                meta_body,
                failed_keys,
                attempt: 0,
            }))
        }
    }

    /// 全 PUT 成功か (`exported_at_ms` を埋めて良いか)。
    fn is_complete(&self) -> bool {
        matches!(self, ExportAttempt::Complete)
    }

    /// pending 永続化対象を取り出す。`Complete` / `Skipped` は `None`。
    fn into_pending(self) -> Option<ExportPendingState> {
        match self {
            ExportAttempt::Pending(state) => Some(*state),
            _ => None,
        }
    }
}

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
    /// この isolate 寿命中に `live-games-index/<inv>-<id>.json` の put が成功
    /// 済みかどうか (https://github.com/SH11235/rshogi/issues/549)。hibernation で isolate が破棄されると
    /// `false` に戻るため、cold start 後の最初の `ensure_core_loaded` で 1 回
    /// だけ retry が走る。`mark_play_started` 経路で put 成功した直後にも
    /// `true` を立て、同 isolate 内の後続 `ensure_core_loaded` で冗長 put が
    /// 走らないようにする。
    ///
    /// put 失敗時はあえて `false` のまま残す (= 次回 `ensure_core_loaded` で
    /// retry させる)。R2 put は同一キーで上書きしても idempotent なので
    /// 重複 put は安全。
    live_index_put_done: Cell<bool>,
    /// 終局確定中。R2 PUT の await 中は他イベントが割り込めるため、
    /// `KEY_FINISHED` 確定前の再入で export や履歴追記が二重に走るのを防ぐ。
    finalizing: Cell<bool>,
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
            live_index_put_done: Cell::new(false),
            finalizing: Cell::new(false),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path();
        let Some(route) = parse_ws_route(&path) else {
            return Response::error("Upgrade required", 426);
        };
        let room_id = route.room_id().to_owned();

        // 初回 fetch でのみ room_id を永続化する。`start_match` 側で game_id 生成に
        // 使うため、DO 再構築後でも同じ値を参照できるよう storage に置く。
        // room_id は `id_from_name` のキーと一致するので、同一 DO インスタンスでは
        // 常に同じ値が到着する前提。
        let existing: Option<String> = self.state.storage().get(KEY_ROOM_ID).await?;
        if existing.is_none() && !room_id.is_empty() {
            self.state.storage().put(KEY_ROOM_ID, room_id.to_owned()).await?;
        }

        // 観戦者の上限チェック (MVP の DDoS 防御)。ブラウザでは WS handshake の
        // HTTP status/body が隠れるため、上限到達時も一度 accept して 1013 で閉じる。
        // 対局者経路 (`WsRoute::Player`) には影響しない。
        let is_spectator = route.is_spectator();
        let spectator_room_full =
            is_spectator && self.count_spectator_sockets() >= MAX_SPECTATORS_PER_ROOM;

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

        if is_spectator {
            if spectator_room_full {
                self.release_spectator_socket(&server, Some(1013), "room full");
            } else if let Err(e) =
                self.send_finished_spectator_snapshot_if_needed(&server, &room_id).await
            {
                crate::structured_log!(
                    event: "finished_spectator_initial_push_failed",
                    component: "game_room",
                    room_id: room_id,
                    err: format!("{e:?}"),
                );
            }
        }

        crate::structured_log!(
            event: "websocket_upgrade_accepted",
            component: "game_room",
        );

        Ok(ResponseBuilder::new().with_status(101).with_websocket(pair.client).empty())
    }

    async fn websocket_message(&self, ws: WebSocket, msg: WebSocketIncomingMessage) -> Result<()> {
        // https://github.com/SH11235/rshogi/issues/627: 受信フレームの byte 数を **String/Binary 共通で** 上限判定する。
        // CSA protocol は text-only なので Binary は最終的に discard するが、サイズ
        // 上限を効かせるためには discard する前に len() を見る必要がある (Cloudflare
        // ランタイムは frame 受信時点で 32 MiB まで取り込めてしまうため、binary 経路を
        // 素通しにすると DoS 緩和が片肺になる)。`trim_end_matches` で改行を削った後だと
        // 判定対象が縮むため必ず元の長さで判定。超過時は `1009 Message Too Big` で即 close。
        let raw_len = match &msg {
            WebSocketIncomingMessage::String(s) => s.len(),
            WebSocketIncomingMessage::Binary(b) => b.len(),
        };
        if raw_len > MAX_WS_LINE_BYTES {
            crate::structured_log!(
                event: "ws_message_too_big",
                component: "game_room",
                bytes: raw_len,
                limit: MAX_WS_LINE_BYTES,
            );
            let _ = ws.close(Some(1009), Some("message too big".to_owned()));
            return Ok(());
        }
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
            WsAttachment::Player {
                role,
                handle,
                is_admin,
                ..
            } => self.handle_game_line(&ws, role, &handle, is_admin, &line).await,
            WsAttachment::Spectator { room_id, .. } => {
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
        // 得ないが、診断のためにエラー内容をログへ残す。Player 以外は対局状態を
        // 動かさず、Spectator だけは上限カウントから確実に外れるよう attachment を
        // Pending に戻す。
        let att: Option<WsAttachment> = ws.deserialize_attachment().unwrap_or_else(|e| {
            crate::structured_log!(
                event: "websocket_close_deserialize_failed",
                component: "game_room",
                err: format!("{e:?}"),
            );
            None
        });
        let role = match att {
            Some(WsAttachment::Player { role, .. }) => role,
            Some(WsAttachment::Spectator { .. }) => {
                self.release_spectator_socket(&ws, None, "spectator closed");
                return Ok(());
            }
            _ => return Ok(()),
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

        // 再接続プロトコルは start_match 時点の設定で対局単位に固定する。
        // close 時に env を読み直すと、対局中の deploy / 設定変更で
        // `Reconnect_Token` 配布有無と grace 判定がずれるため、永続化済み
        // `PersistedConfig` の値だけを参照する。
        let Some(cfg) = cfg_opt else {
            return Ok(());
        };
        let grace_duration = Duration::from_millis(cfg.reconnect_grace_ms.unwrap_or_default());
        if !grace_duration.is_zero() {
            if let Err(e) = self.enter_grace_window(role, grace_duration).await {
                // grace 経路のセットアップに失敗したら旧経路 (即時 force_abnormal)
                // にフォールバックして部屋が宙ぶらりんにならないようにする。
                crate::structured_log!(
                    event: "enter_grace_window_failed",
                    component: "game_room",
                    err: format!("{e:?}"),
                );
            } else {
                return Ok(());
            }
        }

        // 対局中の切断は force_abnormal で敗北を確定する。
        self.ensure_core_loaded().await?;
        let result_opt =
            self.core.borrow_mut().as_mut().map(|core| core.force_abnormal(role.to_core()));
        if let Some(result) = result_opt {
            self.record_verdict(&result).await?;
            self.dispatch_broadcasts(&result.broadcasts).await?;
            self.finalize_if_ended(&result).await?;
        }
        Ok(())
    }

    async fn websocket_error(&self, ws: WebSocket, error: Error) -> Result<()> {
        if matches!(
            ws.deserialize_attachment::<WsAttachment>().ok().flatten(),
            Some(WsAttachment::Spectator { .. })
        ) {
            crate::structured_log!(
                event: "spectator_websocket_error",
                component: "game_room",
                err: format!("{error:?}"),
            );
            self.release_spectator_socket(&ws, Some(1011), "spectator websocket error");
        }
        Ok(())
    }

    async fn alarm(&self) -> Result<Response> {
        // alarm 種別タグを読み、各経路を分岐する。
        // 既定値 (タグ未設定 / `TimeUp`) は時間切れ駆動とみなす。
        //
        // ⚠ `ExportRetry` は `KEY_FINISHED` 確定**後**に発火する
        // 特殊な alarm なので、`load_finished` ガードよりも**前**に分岐する必要
        // がある。それ以外 (`GraceExpired` / `AgreeTimeout` / `TimeUp`) は終局前
        // 経路なので従来どおり `load_finished` ガードを先に通す。
        let kind = self.load_pending_alarm_kind().await?;
        // 確定直後に落ちると種別タグを書く前に止まり得るため、確定後に pending が
        // 残っていれば種別に関わらず export を再試行する。
        let orphaned_export = !matches!(kind, Some(PendingAlarmKind::ExportRetry))
            && self.load_finished().await?.is_some()
            && self
                .state
                .storage()
                .get::<ExportPendingState>(KEY_EXPORT_PENDING)
                .await?
                .is_some();
        if matches!(kind, Some(PendingAlarmKind::ExportRetry)) || orphaned_export {
            if let Err(error) = self.handle_export_retry_alarm().await {
                if self.state.storage().get_alarm().await?.is_none() {
                    return Err(error);
                }
                crate::structured_log!(event: "export_retry_scheduled_after_error",
                    component: "game_room", err: format!("{error:?}"));
            }
            return Response::ok("export_retry handled");
        }

        if self.load_finalizing().await?.is_some() && self.load_finished().await?.is_none() {
            if let Err(error) = self.resume_pending_finalization(true).await {
                // 張り直した alarm があれば runtime の自動再試行に回さない。
                // 自動再試行は数秒間隔で試行回数を消費し、再試行の猶予が尽きるため。
                if self.state.storage().get_alarm().await.ok().flatten().is_some() {
                    crate::structured_log!(
                        event: "finalization_retry_scheduled",
                        component: "game_room",
                        err: format!("{error:?}"),
                    );
                    return Response::ok("finalization retry scheduled");
                }
                return Err(error);
            }
            return Response::ok("finalization resumed");
        }

        // 既に終局済みの DO でアラームが届いたら何もしない（念のためのガード）。
        if self.load_finished().await?.is_some() {
            return Response::ok("already finished");
        }

        if matches!(kind, Some(PendingAlarmKind::GraceExpired)) {
            if self
                .state
                .storage()
                .get::<PendingReconnect>(KEY_GRACE_REGISTRY)
                .await?
                .is_some()
            {
                self.handle_grace_expired_alarm().await?;
                return Response::ok("grace_expired handled");
            }
            // 種別保存と registry 保存の間で中断した場合、残っていた turn alarm を
            // 消費するだけで終わらせず、通常の時計判定へ戻す。
            self.delete_grace_alarm_state().await?;
        }
        if matches!(kind, Some(PendingAlarmKind::AgreeTimeout)) {
            self.handle_agree_timeout_alarm().await?;
            return Response::ok("agree_timeout handled");
        }

        self.ensure_core_loaded().await?;

        // 計時起点は「直前の指し手の at_ms（初手前は play_started_at_ms）」で統一され
        // (https://github.com/SH11235/rshogi/issues/852)、cold-start 復元でも張り直さ
        // ない。ここでは `now` 時点の真の残時間を再計算し、まだ残っていれば
        // (turn alarm を貼り直す前の古い予約が早発火した等) 残時間ぶんで貼り直し、
        // `0` なら実際の時間切れとして force_time_up する。休眠を跨いだ長考の time_up
        // は、実ギャップで残時間が `0` になるためこの経路で正しく発火する。
        let now_ms = self.now_ms();
        let remaining_ms = {
            let borrow = self.core.borrow();
            let Some(core) = borrow.as_ref() else {
                return Response::ok("no core");
            };
            core.current_turn_remaining_ms(now_ms)
        };
        if let Some(remaining) = remaining_ms.filter(|&r| r > 0) {
            let margin_ms = self
                .config
                .borrow()
                .as_ref()
                .map(|c| c.time_margin_ms)
                .unwrap_or(DEFAULT_TIME_MARGIN_MS);
            let total = remaining.saturating_add(margin_ms).saturating_add(ALARM_SAFETY_MS);
            self.state.storage().set_alarm(Duration::from_millis(total)).await?;
            return Response::ok("turn_alarm rescheduled after cold restart");
        }

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
            self.record_verdict(&result).await?;
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
        let ClientCommand::Login {
            name,
            password,
            reconnect,
            ..
        } = cmd
        else {
            // pending 状態で LOGIN 以外が来たら拒否して切断。
            send_line(ws, &LoginReply::Incorrect.to_line())?;
            let _ = ws.close(Some(1000), Some("expected LOGIN".to_owned()));
            return Ok(());
        };

        let Some((handle, game_name, role)) = parse_login_handle(name.as_str()) else {
            send_line(ws, &LoginReply::Incorrect.to_line())?;
            return Ok(());
        };

        // `WORKERS_HANDLE_AUTH` whitelist: registry に登録された
        // handle に限り SHA256(password) を要求する。reconnect 経路の前に挟む
        // (= reconnect token を持っていても、whitelist 対象 handle で password
        // 検証に落ちる session は reconnect 経路にも入らない)。fail-closed:
        // env JSON 不正 / 内部不整合は全 LOGIN reject し、`handle_auth_failed`
        // で uniform に拒否することで「whitelist が設定されているか」を leak
        // しない。
        if enforce_handle_auth(ws, &self.env, &handle, password.expose())? {
            let _ = ws.close(Some(1003), Some("handle_auth_failed".to_owned()));
            return Ok(());
        }

        // 再接続要求の経路分岐。LOGIN 行 3 つ目トークンが
        // `reconnect:<game_id>+<token>` の場合は新規対局参加 (slot 確保 + マッチ
        // 成立) ではなく、grace 中の該当対局へ「同一対局者として再参加」する経路へ。
        // 失敗時は LOGIN OK を送らずに `LOGIN:incorrect reconnect_rejected` で
        // 拒否する (拒否は元の対局者による再試行を妨げない)。
        if let Some(req) = reconnect {
            return self.handle_reconnect_request(ws, &handle, role, req).await;
        }

        // 新スロットを**仮に**加えて衝突判定する。`evaluate_match` が Conflict を返す
        // 場合は永続化も attachment 差し替えも行わず、部屋を破壊しないよう拒否する
        // （game_name 不一致・重複 role・スロット超過の全てを一元的に弾く）。
        let mut next_slots = self.load_slots().await?;
        let player_id_config =
            self.env.var(ConfigKeys::PLAYER_ID_SECRET).ok().map(|value| value.to_string());
        let identity =
            match derive_player_identity(&handle, password.expose(), player_id_config.as_deref()) {
                Ok(identity) => identity,
                Err(error) => {
                    crate::structured_log!(
                        event: "player_id_secret_invalid",
                        component: "game_room",
                        reason: error.reason(),
                        fallback: "legacy_handle_id",
                    );
                    crate::player_identity::PlayerIdentity {
                        player_id: legacy_player_id(&handle),
                        aliases: Vec::new(),
                    }
                }
            };
        next_slots.push(Slot {
            role,
            handle: handle.clone(),
            player_id: identity.player_id.clone(),
            game_name: game_name.clone(),
        });
        if let MatchResult::Conflict { reason } = evaluate_match(&next_slots) {
            crate::structured_log!(
                event: "login_rejected",
                component: "game_room",
                handle: handle,
                role: format!("{role:?}"),
                reason: reason,
            );
            send_line(ws, &LoginReply::Incorrect.to_line())?;
            return Ok(());
        }

        if let Err(error) = crate::games_search_index::register_player_aliases(
            &self.env,
            &identity.player_id,
            &identity.aliases,
        )
        .await
        {
            crate::structured_log!(
                event: "player_id_alias_registration_failed",
                component: "game_room",
                err: format!("{error:?}"),
            );
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
            black_player_id,
            white_handle,
            white_player_id,
            game_name,
        } = evaluate_match(&next_slots)
        {
            let _ = self
                .start_match(
                    &black_handle,
                    &black_player_id,
                    &white_handle,
                    &white_player_id,
                    &game_name,
                )
                .await?;
        }

        Ok(())
    }

    /// マッチ成立時の処理: CoreRoom 作成 + Game_Summary 送出。
    async fn start_match(
        &self,
        black_handle: &str,
        black_player_id: &str,
        white_handle: &str,
        white_player_id: &str,
        game_name: &str,
    ) -> Result<bool> {
        // https://github.com/SH11235/rshogi/issues/626: `start_match` の呼び出し前に存在する重複起動防止ガード
        // (`handle_login` 側は `evaluate_match` の `MatchResult::Match` 判定、
        // `try_start_pending_match` 側は `handle_game_line` 入口の
        // `active_game_id().is_none()` 判定) が、cold start race (`KEY_CONFIG`
        // read transient err 等) で誤通過した場合、末尾の
        // `set_alarm(agree_timeout)` で `enter_grace_window` 経由の `GraceExpired`
        // alarm を上書きし、合意済み対局を `agree_timeout` で abort する race が
        // 理論上残る。副作用 (buoy reservation / `KEY_CONFIG` put / `set_alarm`)
        // の前に三段ガード (FINISHED / KEY_CONFIG / KEY_PENDING_ALARM_KIND) を
        // 逐次 early return で配置し、いずれか set 済なら
        // `abort_pending_match_with_error` で pending slot を片付けて
        // `Ok(false)` を返す。判定ロジックは `reconnect::classify_start_match_guard`
        // で純粋関数化し unit test で固定する。
        //
        // 各 read を逐次にしているのは、`finished` を読み終えた直後に後続 read が
        // err になっても、判定済みの強い状態 (FINISHED) を優先して abort cleanup
        // まで到達できるようにするため (Codex review round 3 の指摘)。
        // `KEY_FINISHED` チェック: 通常は `handle_login` / `handle_game_line`
        // 入口の `load_finished` ガードで弾かれる経路。defensive な fallback と
        // して残し、Player attachment の WS は `abort_pending_match_with_error`
        // で server-initiated close する (`handle_login` で `LOGIN OK` 送出 +
        // attachment が Player に差し替わっているため、無言で `Ok(false)` だと
        // WS が宙ぶらりんになる)。
        let finished_present = self.load_finished().await?.is_some();
        if let StartMatchGuard::AlreadyFinished =
            classify_start_match_guard(finished_present, false, None)
        {
            crate::structured_log!(
                event: "start_match_aborted",
                component: "game_room",
                reason: "already_finished",
            );
            self.abort_pending_match_with_error("##[ERROR] room already finished").await?;
            return Ok(false);
        }
        // `KEY_CONFIG` チェック: race 想定では既存対局の Player WS は (a) 切断済
        // (= attachment は Player のままだが close 済の状態を
        // `abort_pending_match_with_error` が無視する) か (b) grace 中 (= 同様に
        // 既に close 済) のいずれか。万一既存対局の生 WS が同 DO に残っている
        // 場合は close されるが、KEY_CONFIG present + 新規 LOGIN 経路自体が
        // cold start race 想定で、巻き込みは race 経路の defensive 副作用として
        // 許容する (Codex round 2)。
        let cfg_present = self.state.storage().get::<PersistedConfig>(KEY_CONFIG).await?.is_some();
        if let StartMatchGuard::AlreadyMatched =
            classify_start_match_guard(false, cfg_present, None)
        {
            crate::structured_log!(
                event: "start_match_aborted",
                component: "game_room",
                reason: "key_config_already_present",
            );
            self.abort_pending_match_with_error("##[ERROR] match already in progress")
                .await?;
            return Ok(false);
        }
        // `KEY_PENDING_ALARM_KIND` チェック: `GraceExpired` / `TimeUp` /
        // `AgreeTimeout` のどれが残っていても reject する。`AgreeTimeout` の前回
        // start_match 残留を続行扱いにすると、古い alarm 本体が新 match を巻き
        // 込む可能性があるため (Codex round 2 → round 3 の合意点)。
        let existing_kind: Option<PendingAlarmKind> =
            self.state.storage().get(KEY_PENDING_ALARM_KIND).await?;
        if let StartMatchGuard::AlarmPending(kind) =
            classify_start_match_guard(false, false, existing_kind)
        {
            crate::structured_log!(
                event: "start_match_aborted",
                component: "game_room",
                reason: "pending_alarm_kind_present",
                pending_alarm_kind: format!("{kind:?}"),
            );
            self.abort_pending_match_with_error("##[ERROR] match already has pending alarm")
                .await?;
            return Ok(false);
        }

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
        // 双方の LOGIN は既に OK を返しているため、ここで `?` で Err を伝播させると
        // 両クライアントには Game_Summary も `##[ERROR]` 通知も届かず部屋が永久に
        // 詰まる。`CLOCK_PRESETS` の不正設定 (`parse_clock_presets` Err) や、env
        // 設定の不整合で `game_name` 未登録に落ちるケースも、buoy reservation 失敗
        // と同じ pending match abort 経路に揃えて部屋を解放する (https://github.com/SH11235/rshogi/issues/641 で
        // Lobby 側の OnceCell キャッシュ廃止後は両 DO ともに env を毎回読み直す
        // ため deploy race による乖離は解消されたが、`CLOCK_PRESETS` 不正設定時の
        // fail-fast 経路は残す)。
        let preset = match resolve_game_preset_for_game(&self.env, game_name) {
            Ok(preset) => preset,
            Err(e) => {
                crate::structured_log!(
                    event: "clock_spec_resolution_failed",
                    component: "game_room",
                    game_name: game_name,
                    err: format!("{e:?}"),
                );
                self.abort_pending_match_with_error(&format!(
                    "##[ERROR] clock spec for '{game_name}' could not be resolved"
                ))
                .await?;
                return Ok(false);
            }
        };
        // LOGIN OK 後 Game_Summary 送出前に `##[ERROR]` を送って match を不成立に
        // する経路は CSA 標準には無いが、本コードベースは既に clock_spec / buoy
        // 解決失敗で同経路を使っている (`abort_pending_match_with_error`)。
        // Floodgate 互換 client は `Game_Summary` を期待するので `##[ERROR]` を
        // 受け取って parse error / session disconnect する。Workers DO 側は両 player
        // の WS が close されることで部屋を解放できる。本経路は misconfig fail-fast
        // の defensive measure であり、production の保守的既定 (grace=0 +
        // allow=false) では `Ok(Duration::ZERO)` を返すため到達しない。
        //
        // 本検証を **buoy reservation の前** に配置するのは
        // `reserve_initial_sfen_from_buoy` が R2 に `remaining -= 1` を conditional
        // PUT する side-effect を持つため。grace 設定 misconfig で abort する場合に
        // buoy だけ消費されると、次 LOGIN で `Exhausted` が誤発火する。
        let grace = match resolve_reconnect_grace(&self.env) {
            Ok(d) => d,
            Err(e) => {
                crate::structured_log!(
                    event: "reconnect_grace_config_error",
                    component: "game_room",
                    err: format!("{e}"),
                );
                self.abort_pending_match_with_error("##[ERROR] reconnect grace config error")
                    .await?;
                return Ok(false);
            }
        };
        // 双方の LOGIN は既に OK を返しているので、予約で失敗したまま早期
        // return するとスロットが永久に詰まる。Exhausted に加え、CAS リトライ
        // 上限到達などの Err も pending match abort 経路に落として部屋を
        // 再利用可能にする。
        let reservation = match self.reserve_initial_sfen_from_buoy(&GameName::new(game_name)).await
        {
            Ok(r) => r,
            Err(e) => {
                crate::structured_log!(
                    event: "buoy_reservation_failed",
                    component: "game_room",
                    game_name: game_name,
                    err: format!("{e:?}"),
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
                crate::structured_log!(
                    event: "buoy_exhausted",
                    component: "game_room",
                    game_name: game_name,
                );
                self.abort_pending_match_with_error(&format!(
                    "##[ERROR] buoy '{game_name}' exhausted"
                ))
                .await?;
                return Ok(false);
            }
        };

        // 対局開始時に対局者ごとに一意な再接続トークンを発行する。再接続 grace が
        // 有効な構成 (`grace > 0`) でのみ発行し、`Game_Summary` 末尾拡張行で配布した
        // 値を、後の grace 経路 (websocket_close → 再接続要求の `expected_token` 照合)
        // で参照するために `PersistedConfig` に保存する。`grace == 0` の構成では
        // `(None, None)` を返し token 配布も `PersistedConfig` への保存もスキップする。
        let (black_reconnect_token, white_reconnect_token) = issue_tokens_if_enabled(grace);
        // 入玉ルールを env から解決してマッチ単位で固定する。対局中に env を
        // 変えても進行中の局は開始時ルールで判定する ([`PersistedConfig::entering_king_rule`])。
        let entering_king_rule = resolve_entering_king_rule(&self.env);
        let clock_spec = preset.clock;
        let cfg = PersistedConfig {
            game_id: game_id.clone(),
            black_handle: black_handle.to_owned(),
            black_player_id: black_player_id.to_owned(),
            white_handle: white_handle.to_owned(),
            white_player_id: white_player_id.to_owned(),
            game_name: game_name.to_owned(),
            clock: clock_spec.clone(),
            max_moves: preset.max_moves.unwrap_or(DEFAULT_MAX_MOVES),
            time_margin_ms: DEFAULT_TIME_MARGIN_MS,
            matched_at_ms: started,
            play_started_at_ms: None,
            initial_sfen,
            reconnect_grace_ms: Some(grace.as_millis().try_into().map_err(|_| {
                Error::RustError("reconnect grace duration exceeds u64 milliseconds".into())
            })?),
            black_reconnect_token: black_reconnect_token.as_ref().map(|t| t.as_str().to_owned()),
            white_reconnect_token: white_reconnect_token.as_ref().map(|t| t.as_str().to_owned()),
            entering_king_rule: Some(entering_king_rule.to_usi().to_owned()),
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
        // 落とさず Error::RustError で Runtime に伝搬する。
        let core = CoreRoom::new(
            GameRoomConfig {
                game_id: GameId::new(cfg.game_id.clone()),
                black: PlayerName::new(cfg.black_handle.clone()),
                white: PlayerName::new(cfg.white_handle.clone()),
                max_moves: cfg.max_moves,
                time_margin_ms: cfg.time_margin_ms,
                entering_king_rule,
                initial_sfen: cfg.initial_sfen.clone(),
            },
            clock,
        )
        .map_err(|e| Error::RustError(format!("CoreRoom::new: {e:?}")))?;
        *self.core.borrow_mut() = Some(core);
        *self.config.borrow_mut() = Some(cfg.clone());

        // 上で `PersistedConfig` に保存したトークンを Game_Summary の末尾拡張行で
        // 配布する。クライアントは本トークンを保持しておき、デプロイ／DO 再起動
        // による切断時に LOGIN reconnect 引数として提示して同一対局・同一対局者
        // として再参加する。再接続 grace が無効な構成 (`grace == 0`) では
        // `black_reconnect_token` / `white_reconnect_token` ともに `None` で、
        // `Game_Summary` 末尾拡張行に `Reconnect_Token:` 行は付かない。
        let builder = GameSummaryBuilder {
            game_id: GameId::new(cfg.game_id),
            black: PlayerName::new(cfg.black_handle),
            white: PlayerName::new(cfg.white_handle),
            time_section,
            position_section,
            rematch_on_draw: false,
            to_move,
            entering_king_rule,
            black_reconnect_token,
            white_reconnect_token,
        };
        let summary_black = builder.build_for(Color::Black);
        let summary_white = builder.build_for(Color::White);

        self.send_to_role(Role::Black, &summary_black).await?;
        self.send_to_role(Role::White, &summary_white).await?;

        // AGREE 待ち TTL を予約する (https://github.com/SH11235/rshogi/issues/600)。LOGIN OK 後に AGREE が届かない
        // まま片方が刺さると、`/api/v1/games/live` に出ない (mark_play_started が
        // 走らないので live-games-index に put しない) のに DO 側では memory /
        // room 枠を専有し続ける edge case を解消する。
        //
        // - 両者 AGREE で `HandleOutcome::GameStarted` が観測されたら、
        //   `reschedule_turn_alarm` が turn budget で alarm 本体を上書きした **後**
        //   に `clear_agree_timeout_tag` で kind タグを削除する (タグ削除を後置
        //   することで「kind=None かつ alarm=AgreeTimeout 当時の発火時刻」の
        //   中間状態をコード上に作らない)。
        //   タグが無くなった後は alarm が時間切れ駆動 (TimeUp) として扱われる。
        // - alarm が `AgreeTimeout` のまま発火したら `handle_agree_timeout_alarm` で
        //   部屋を解放する (`abort_pending_match_with_error` 相当 + `KEY_FINISHED`
        //   セット)。AGREE 前なので live-games-index は put 前 (cleanup 不要)、
        //   `play_started_at_ms` も None のまま (R2 棋譜エクスポート skip)。
        let agree_timeout = resolve_agree_timeout(&self.env);
        self.state
            .storage()
            .put(KEY_PENDING_ALARM_KIND, &PendingAlarmKind::AgreeTimeout)
            .await?;
        self.state.storage().set_alarm(agree_timeout).await?;

        Ok(true)
    }

    /// 対局中のプレイヤからの行を CoreRoom に流す。
    async fn handle_game_line(
        &self,
        ws: &WebSocket,
        role: Role,
        handle: &str,
        is_admin: bool,
        line: &str,
    ) -> Result<()> {
        if self.load_finished().await?.is_some() {
            // 終局後に届いた行は無視する。
            return Ok(());
        }

        // Workers 固有プロトコル拡張: `%%ADMIN <token>` で admin 権限を session
        // 内に昇格する (https://github.com/SH11235/rshogi/issues/621)。共有
        // `parse_command` には乗せず Workers 内で完結させる。判定後は通常の
        // CoreRoom 経路には流さない (admin elevation は副作用持ちの独立操作)。
        if let Some(token) = crate::admin_auth::parse_admin_line(line) {
            for out in self.handle_admin_elevation(ws, token).await? {
                send_line(ws, &out)?;
            }
            return Ok(());
        }

        let csa = CsaLine::new(line);
        if let Ok(cmd) = parse_command(&csa) {
            if let Some(replies) = self.handle_player_control_command(handle, is_admin, cmd).await?
            {
                for out in replies {
                    send_line(ws, &out)?;
                }
                return Ok(());
            }
        }

        if self.active_game_id().await?.is_none() && !self.try_start_pending_match().await? {
            return Ok(());
        }

        // `now` は ensure_core_loaded の**前**(= メッセージ到着時点)で採る。計時起点は
        // 直前の指し手の at_ms なので、着手側に課金されるのは「到着時刻 - 起点」=
        // 思考 + ネットワーク時間のみとなり、cold-start の復元 I/O(R2 リトライ +
        // O(手数) の replay)は課金されない。後で採ると msec 級の短い時計では
        // 復元 I/O だけで TimeUp し得る (https://github.com/SH11235/rshogi/issues/852)。
        let now = self.now_ms();
        self.ensure_core_loaded().await?;
        let color = role.to_core();

        let result = {
            let mut borrow = self.core.borrow_mut();
            let Some(core) = borrow.as_mut() else {
                // core 復元不能で安全網 (`force_finalize_unrecoverable`) が既に
                // #ABNORMAL 強制終局済み (or 対局未成立)。無言破棄せず WS を閉じて
                // 終局として応答する (https://github.com/SH11235/rshogi/issues/852)。
                crate::structured_log!(
                    event: "handle_game_line_core_missing",
                    component: "game_room",
                    handle: handle,
                );
                let _ =
                    ws.close(Some(1011), Some("game finished (unrecoverable state)".to_owned()));
                return Ok(());
            };
            match core.handle_line(color, &csa, now) {
                Ok(r) => r,
                Err(e) => {
                    crate::structured_log!(
                        event: "handle_line_error",
                        component: "game_room",
                        handle: handle,
                        err: format!("{e:?}"),
                    );
                    return Ok(());
                }
            }
        };

        // outcome 別の永続化 + alarm + broadcast 順序 (AgreeTimeout タグ管理)。
        //
        // - `GameStarted` / `MoveAccepted`: 新 turn alarm の予約と
        //   `clear_agree_timeout_tag` を **broadcast より前** に行う。broadcast 失敗で
        //   `?` 伝播した場合でも alarm が turn budget に再予約済となり、既存の
        //   `AgreeTimeout` alarm がそのまま発火して `handle_agree_timeout_alarm` の
        //   `play_started_at_ms.is_some()` ガードで「タグ削除のみ」して return する
        //   (turn alarm が二度と貼られない) 経路を回避する。
        // - `GameEnded`: 終局保存が失敗しても再試行できるよう、alarm は保存後に解除する。
        // - `Continue`: alarm 変更は不要 (旧順序維持)。
        match &result.outcome {
            HandleOutcome::GameStarted => {
                self.mark_play_started(now).await?;
                self.reschedule_turn_alarm(&result.outcome).await?;
                // AGREE 待ち TTL タグの片付けは新 turn alarm 設定後に行う。先に
                // タグを消すと「kind=None かつ alarm=AgreeTimeout 当時の発火時刻」の
                // 中間状態をコード順序として作ることになる。
                self.clear_agree_timeout_tag().await;
                self.dispatch_broadcasts(&result.broadcasts).await?;
            }
            HandleOutcome::MoveAccepted { .. } => {
                self.append_move(color, line, now).await?;
                self.reschedule_turn_alarm(&result.outcome).await?;
                self.dispatch_broadcasts(&result.broadcasts).await?;
            }
            HandleOutcome::GameEnded(_) => {
                // 盤面を進めた終局手も保存し、棋譜と復元後の裁定を一致させる。
                if result.broadcasts.iter().any(is_move_broadcast) {
                    self.append_move(color, line, now).await?;
                }
                self.record_verdict(&result).await?;
                self.dispatch_broadcasts(&result.broadcasts).await?;
            }
            HandleOutcome::Continue => {
                self.dispatch_broadcasts(&result.broadcasts).await?;
                self.reschedule_turn_alarm(&result.outcome).await?;
            }
        }
        self.finalize_if_ended(&result).await?;
        Ok(())
    }

    fn count_spectator_sockets(&self) -> usize {
        let mut count = 0;
        for ws in self.state.get_websockets() {
            let att = ws.deserialize_attachment::<WsAttachment>().ok().flatten();
            let Some(att @ WsAttachment::Spectator { .. }) = att else {
                continue;
            };
            if ws.serialize_attachment(&att).is_ok() {
                count += 1;
            } else {
                let _ = ws.close(Some(1001), Some("stale spectator".to_owned()));
            }
        }
        count
    }

    fn release_spectator_socket(&self, ws: &WebSocket, code: Option<u16>, reason: &str) {
        let _ = ws.serialize_attachment(&WsAttachment::Pending);
        if let Some(code) = code {
            let _ = ws.close(Some(code), Some(reason.to_owned()));
        }
    }

    async fn send_finished_spectator_snapshot_if_needed(
        &self,
        ws: &WebSocket,
        room_id: &str,
    ) -> Result<()> {
        let Some(finished) = self.load_finished().await? else {
            return Ok(());
        };
        let cfg_opt: Option<PersistedConfig> = self.state.storage().get(KEY_CONFIG).await?;
        let monitor_id = cfg_opt.as_ref().map(|c| c.game_id.as_str()).unwrap_or(room_id);
        let finished_opt = Some(finished);

        send_line(ws, &format!("##[MONITOR2] BEGIN {monitor_id}"))?;
        self.send_spectator_snapshot(ws, &finished_opt, cfg_opt.as_ref()).await?;
        send_line(ws, "##[MONITOR2] END")?;
        self.release_spectator_socket(ws, Some(1000), "spectate finished");
        Ok(())
    }

    /// 観戦者からの制御行。`%%CHAT` を同一 room の全参加者へ relay し、
    /// `%%MONITOR2OFF` は確認応答後に socket を閉じる。`%%MONITOR2ON` は
    /// snapshot (= Game_Summary + 既存指し手 + 終局結果) を 1 回送出する。
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
                let finished = self.load_finished().await?;
                let cfg_opt: Option<PersistedConfig> = self.state.storage().get(KEY_CONFIG).await?;
                let finished_game_id =
                    finished.as_ref().and(cfg_opt.as_ref().map(|c| c.game_id.as_str()));
                let decision = resolve_monitor_target_with_finished(
                    room_id,
                    active_game_id.as_deref(),
                    finished_game_id,
                    game_id.as_str(),
                );
                match decision {
                    MonitorDecision::Accept { monitor_id } => {
                        send_line(ws, &format!("##[MONITOR2] BEGIN {monitor_id}"))?;
                        self.send_spectator_snapshot(ws, &finished, cfg_opt.as_ref()).await?;
                        send_line(ws, "##[MONITOR2] END")?;
                        // 終局済 DO は snapshot を流したあとで close する。client 側は
                        // `onEnd` 発火後の reconnect 経路を停止するため、normal close
                        // (code 1000) で終了通知するだけで十分。
                        if finished.is_some() {
                            let _ = ws.close(Some(1000), Some("spectate finished".to_owned()));
                        } else {
                            // snapshot 用の復元で終局済みと分かった場合、結果通知を
                            // snapshot の後に送るためここで終局確定を再開する。
                            self.ensure_core_loaded().await?;
                        }
                    }
                    MonitorDecision::NotFound { requested } => {
                        send_line(ws, &format!("##[MONITOR2] NOT_FOUND {requested}"))?;
                        send_line(ws, "##[MONITOR2] END")?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `Monitor2On Accept` 経路で snapshot を送る本体。
    ///
    /// 流れ:
    /// 1. attachment の `snapshot_in_progress = true` をセット (= 以降この ws 宛
    ///    の broadcast は `send_to_spectators` で per-ws pending queue に積まれる)
    /// 2. `ensure_core_loaded()` 後に `core` があれば現在残時間、無ければ config
    ///    由来の初期残時間で `SpectatorClocks` を組み、`load_moves()` で指し手列を確定
    /// 3. `build_spectator_snapshot` の wire 行を順次 send
    /// 4. attachment の queue を flush (`ply > last_ply_in_snapshot` のみ送る) し、
    ///    `snapshot_in_progress = false` に戻して通常 broadcast 経路へ復帰
    ///
    /// 例外経路: `cfg` が無いケース (= LOGIN 前の DO に観戦者だけが入ってきた
    /// case)。snapshot は組まずに END を返してそのまま通常 broadcast 経路に
    /// 載せる (player から手が指されるまで配信は無いので queue 不要)。
    ///
    /// エラー経路: flag を `true` にセットした後の失敗 (`ensure_core_loaded` /
    /// `load_moves` / `send_line` / attachment 更新) は必ずリセットして復旧する。
    /// flag が `true` のまま抜けると `send_to_spectators` が以降の broadcast を
    /// per-ws pending queue に積み続け、観戦者が queue overflow まで無音フリーズ
    /// する。中断時は attachment を [`WsAttachment::reset_spectator_snapshot`] で
    /// best-effort リセットし、client の reconnect ロジックで綺麗に復旧できるよう
    /// ws を close (1011) してからエラーを伝播する。
    async fn send_spectator_snapshot(
        &self,
        ws: &WebSocket,
        finished: &Option<FinishedState>,
        cfg_opt: Option<&PersistedConfig>,
    ) -> Result<()> {
        let Some(cfg) = cfg_opt else {
            // 対局未開始 (LOGIN 前) の DO に観戦者が入ったケース。snapshot は
            // 出さずそのまま終了する (Game_Summary に必要な game_id すら無いため)。
            // 未開始の空 snapshot も完了として通常配信へ移す。
            self.set_spectator_snapshot_state(ws, false, 0, Vec::new())?;
            return Ok(());
        };

        // attachment の snapshot 状態を「送信中」に更新する。queue / last_ply は
        // ここで初期化し、過去 invocation の残骸が混ざらないようにする。
        self.set_spectator_snapshot_state(ws, true, 0, Vec::new())?;

        // ここから先の失敗は flag=true を残さない。中断したら reset + close する。
        if let Err(e) = self.send_spectator_snapshot_body(ws, cfg, finished).await {
            if let Ok(Some(att)) = ws.deserialize_attachment::<WsAttachment>() {
                let mut reset = att.reset_spectator_snapshot();
                let _ = reset.abort_terminal(
                    &mut |att| ws.serialize_attachment(att),
                    &mut |code, reason| {
                        let _ = ws.close(Some(code), Some(reason.to_owned()));
                    },
                );
            }
            let _ = ws.close(Some(1011), Some("snapshot failed".to_owned()));
            crate::structured_log!(
                event: "spectator_snapshot_failed",
                component: "game_room",
                game_id: cfg.game_id,
                err: format!("{e:?}"),
            );
            return Err(e);
        }
        Ok(())
    }

    /// `send_spectator_snapshot` の本体。`snapshot_in_progress = true` 前提で
    /// snapshot 行を組んで送信し、queue を flush して flag を `false` に戻す。
    /// いずれかの `?` で早期 return したときのリセット責務は呼び出し側が担う。
    async fn send_spectator_snapshot_body(
        &self,
        ws: &WebSocket,
        cfg: &PersistedConfig,
        finished: &Option<FinishedState>,
    ) -> Result<()> {
        // CoreRoom を確保し、clock / current_turn から `SpectatorClocks` を組む。
        // borrow scope は最小化し、await を伴う `load_moves` は borrow 外で呼ぶ。
        // 終局確定は snapshot 完了後に再開する。ここで確定すると結果通知と
        // close が snapshot 本体より先に届く。
        let _ = self.restore_core().await?;
        let clocks = {
            let borrow = self.core.borrow();
            borrow
                .as_ref()
                .map(|core| SpectatorClocks {
                    black_remaining_ms: core.clock_remaining_main_ms(Color::Black).max(0) as u64,
                    white_remaining_ms: core.clock_remaining_main_ms(Color::White).max(0) as u64,
                    side_to_move: core.current_turn(),
                })
                // 終局済み room などで CoreRoom を復元しない場合も snapshot 形式を保つ。
                .unwrap_or_else(|| initial_spectator_clocks(cfg))
        };

        let moves = self.load_spectator_snapshot_moves(cfg, finished).await?;
        let last_ply_in_snapshot = u32::try_from(moves.len()).unwrap_or(u32::MAX);

        let lines = build_spectator_snapshot(SpectatorSnapshotInput {
            config: cfg,
            moves: &moves,
            clocks: &clocks,
            // 結果行は下で attachment の配信記録を通す。
            finalized: None,
        });
        for line in &lines {
            send_line(ws, line)?;
        }

        // snapshot 完了。attachment の last_ply を更新し、queue を flush する。
        // queue 内の手は `ply > last_ply_in_snapshot` のみ送る (= snapshot に
        // 含まれた手と重複しない broadcast のみ)。flush は flag を false へ戻す。
        self.set_spectator_snapshot_last_ply(ws, last_ply_in_snapshot)?;
        self.flush_spectator_snapshot_queue(ws).await?;
        if let Some(finished) = finished {
            let entries = [crate::attachment::FinalizingBroadcast {
                target: BroadcastTarget::Spectators,
                line: finished.result_code.clone(),
                ply: None,
            }];
            let mut att = ws
                .deserialize_attachment::<WsAttachment>()?
                .ok_or_else(|| Error::RustError("snapshot attachment missing".into()))?;
            att.deliver_terminal(
                &entries,
                |line| send_line(ws, line),
                |att| ws.serialize_attachment(att),
                |code, reason| {
                    let _ = ws.close(Some(code), Some(reason.to_owned()));
                },
            )?;
            if att.terminal_aborted() || !att.terminal_complete(&entries) {
                return Err(Error::RustError("snapshot terminal delivery failed".into()));
            }
        }
        Ok(())
    }

    async fn load_spectator_snapshot_moves(
        &self,
        cfg: &PersistedConfig,
        finished: &Option<FinishedState>,
    ) -> Result<Vec<MoveRow>> {
        let moves = self.load_moves().await?;
        if finished.is_none() || !moves.is_empty() {
            return Ok(moves);
        }

        let first_prev_ms = cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms);
        let pending: Option<ExportPendingState> =
            self.state.storage().get(KEY_EXPORT_PENDING).await?;
        if let Some(pending) = pending
            && pending.game_id == cfg.game_id
        {
            // 初手前の終局も、保存済み本文が完全な原本である。
            return Ok(move_rows_from_exported_csa(&pending.csa_text, first_prev_ms));
        }

        match self.load_kifu_by_game_id(&GameId::new(cfg.game_id.clone())).await {
            Ok(Some(csa_text)) => {
                let rows = move_rows_from_exported_csa(&csa_text, first_prev_ms);
                if rows.is_empty() {
                    crate::structured_log!(
                        event: "finished_spectator_snapshot_moves_empty",
                        component: "game_room",
                        game_id: cfg.game_id,
                    );
                }
                Ok(rows)
            }
            Ok(None) => {
                crate::structured_log!(
                    event: "finished_spectator_snapshot_kifu_missing",
                    component: "game_room",
                    game_id: cfg.game_id,
                );
                Err(Error::RustError("finished snapshot kifu missing".into()))
            }
            Err(e) => {
                crate::structured_log!(
                    event: "finished_spectator_snapshot_kifu_load_failed",
                    component: "game_room",
                    game_id: cfg.game_id,
                    err: format!("{e:?}"),
                );
                Err(e)
            }
        }
    }

    /// queue 消去前に休眠すると同じ行を再送し得るため、送信前に取り出しを保存する。
    async fn flush_spectator_snapshot_queue(&self, ws: &WebSocket) -> Result<()> {
        loop {
            let Some(mut att) = ws.deserialize_attachment::<WsAttachment>()? else {
                return Ok(());
            };
            let WsAttachment::Spectator {
                last_ply_in_snapshot,
                pending_queue,
                snapshot_in_progress,
                ..
            } = &mut att
            else {
                return Ok(());
            };
            if pending_queue.is_empty() {
                *snapshot_in_progress = false;
                ws.serialize_attachment(&att)?;
                return Ok(());
            }
            let (line, ply) = pending_queue.remove(0);
            let included = ply.is_some_and(|ply| ply <= *last_ply_in_snapshot);
            ws.serialize_attachment(&att)?;
            if !included {
                // 欠落した snapshot を正常完了扱いにしない。
                send_line(ws, &line)?;
            }
        }
    }

    /// `WsAttachment::Spectator` の snapshot 関連 3 フィールドを一括更新する。
    ///
    /// `room_id` は既存値を保持する (上書きしない)。Spectator でない場合は no-op。
    fn set_spectator_snapshot_state(
        &self,
        ws: &WebSocket,
        snapshot_in_progress: bool,
        last_ply_in_snapshot: u32,
        pending_queue: Vec<(String, Option<u32>)>,
    ) -> Result<()> {
        let att = ws
            .deserialize_attachment::<WsAttachment>()
            .map_err(|e| Error::RustError(format!("deserialize_attachment: {e}")))?;
        let Some(WsAttachment::Spectator {
            room_id,
            terminal_sent,
            terminal_in_flight,
            terminal_aborted,
            ..
        }) = att
        else {
            return Ok(());
        };
        let updated = WsAttachment::Spectator {
            room_id,
            snapshot_in_progress,
            last_ply_in_snapshot,
            pending_queue,
            terminal_sent,
            terminal_in_flight,
            terminal_aborted,
        };
        ws.serialize_attachment(&updated)
            .map_err(|e| Error::RustError(format!("serialize_attachment: {e}")))
    }

    /// snapshot 構築完了時に `last_ply_in_snapshot` のみ更新する補助関数。
    /// `snapshot_in_progress` / `pending_queue` は queue が積まれていれば残す。
    fn set_spectator_snapshot_last_ply(&self, ws: &WebSocket, last_ply: u32) -> Result<()> {
        let att = ws
            .deserialize_attachment::<WsAttachment>()
            .map_err(|e| Error::RustError(format!("deserialize_attachment: {e}")))?;
        let Some(WsAttachment::Spectator {
            room_id,
            snapshot_in_progress,
            pending_queue,
            terminal_sent,
            terminal_in_flight,
            terminal_aborted,
            ..
        }) = att
        else {
            return Ok(());
        };
        let updated = WsAttachment::Spectator {
            room_id,
            snapshot_in_progress,
            last_ply_in_snapshot: last_ply,
            pending_queue,
            terminal_sent,
            terminal_in_flight,
            terminal_aborted,
        };
        ws.serialize_attachment(&updated)
            .map_err(|e| Error::RustError(format!("serialize_attachment: {e}")))
    }

    /// プレイヤー接続から受け付ける制御系コマンドを処理する。
    ///
    /// `is_admin` は当該 session が `%%ADMIN <token>` で昇格済みかを示す。
    /// `%%SETBUOY` / `%%DELETEBUOY` 等の admin 権限要求コマンドは本フラグが
    /// `true` のときのみ受理する (https://github.com/SH11235/rshogi/issues/621)。
    ///
    /// `Some(replies)` を返した場合は、呼び出し側が返信行を送って通常の
    /// `CoreRoom::handle_line` 経路をスキップする。
    async fn handle_player_control_command(
        &self,
        handle: &str,
        is_admin: bool,
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
                if !is_admin {
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
                if !is_admin {
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

    /// `%%ADMIN [<token>]` を受信した player 接続の admin elevation を処理する。
    ///
    /// 成功時 (`verify_admin_token_str` OK): WS attachment の `is_admin` を
    /// `true` に上書きし、`##[ADMIN] OK` を返す。
    /// 失敗時 (`Err(_)` 全 variant: TokenNotConfigured / MissingCredential /
    /// TokenMismatch): `##[ADMIN] PERMISSION_DENIED` を返す。3 variant を
    /// 区別せず同じ応答を返すことで、admin command の認識有無や secret
    /// configured 状態の leak を防ぐ (Copilot review 指摘)。
    /// 呼び出し側は本関数の返値を順次 `send_line` で WS に流す契約。
    ///
    /// `parse_admin_line` が `Some("")` を返した token 部欠落ケース (`%%ADMIN`
    /// 単体等) もここに到達し、`verify_admin_token_str("")` →
    /// `MissingCredential` → 同じ `PERMISSION_DENIED` 経路に乗る。
    async fn handle_admin_elevation(&self, ws: &WebSocket, token: &str) -> Result<Vec<String>> {
        match crate::admin_auth::verify_admin_token_str(token, &self.env) {
            Ok(()) => {
                self.upgrade_attachment_to_admin(ws)?;
                Ok(vec!["##[ADMIN] OK".to_owned(), "##[ADMIN] END".to_owned()])
            }
            Err(_) => Ok(vec![
                "##[ADMIN] PERMISSION_DENIED".to_owned(),
                "##[ADMIN] END".to_owned(),
            ]),
        }
    }

    /// 既存の `WsAttachment::Player` を `is_admin = true` 版に上書きする。
    /// `Player` 以外の variant に対しては no-op (`Spectator` / `Pending` は
    /// admin 昇格対象外という型 invariant を [`WsAttachment::with_admin`] 側で
    /// 守る契約)。
    fn upgrade_attachment_to_admin(&self, ws: &WebSocket) -> Result<()> {
        let att: WsAttachment = ws
            .deserialize_attachment()
            .map_err(|e| Error::RustError(format!("deserialize_attachment: {e}")))?
            .unwrap_or(WsAttachment::Pending);
        let updated = att.with_admin();
        ws.serialize_attachment(&updated)
            .map_err(|e| Error::RustError(format!("serialize_attachment: {e}")))
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
            crate::structured_log!(
                event: "buoy_reservation_etag_mismatch",
                component: "game_room",
                game_name: game_name.as_str(),
                attempt: attempt + 1,
                max_attempts: MAX_ATTEMPTS,
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
            black_player_id,
            white_handle,
            white_player_id,
            game_name,
        } = evaluate_match(&slots)
        else {
            return Ok(false);
        };
        self.start_match(
            &black_handle,
            &black_player_id,
            &white_handle,
            &white_player_id,
            &game_name,
        )
        .await
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
        // Defense-in-depth: %%DELETEBUOY 経路から `buoys/` prefix 以外の R2
        // オブジェクト (棋譜 `<YYYY>/...` / `kifu-by-id/` / live-games-index 等) が
        // 削除されないよう runtime で prefix を検証する (https://github.com/SH11235/rshogi/issues/624)。
        // `buoy_object_key` は常に prefix を埋めるため通常経路では fail しない
        // が、将来の helper 改修で prefix が抜けた場合の tripwire として残す。
        if !key.starts_with(BUOY_KEY_PREFIX) {
            return Err(Error::RustError(format!(
                "delete_buoy refused: key '{key}' is not under '{BUOY_KEY_PREFIX}' prefix"
            )));
        }
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
    ///
    /// 指し手を観戦者へ送った直後には、CoreRoom が保持する ms 粒度の権威時計を
    /// `##[CLOCK]` 行として観戦者だけへ追加送信する。対局者向け CSA wire は変更せず、
    /// 旧観戦 client は未知の control 行を無視できる。clock payload の ply は指し手と
    /// 揃える一方、pending queue 上は `None` として snapshot 後に必ず flush する。
    /// snapshot が新しい指し手を取り込んでも、その直前に採取した古い snapshot 時計を
    /// 毎手 clock で上書きできるようにするためである。
    async fn dispatch_broadcasts(&self, entries: &[BroadcastEntry]) -> Result<()> {
        // 終局の行は保存済みの broadcasts から接続単位で一度だけ配るため、ここでは送らない。
        if self.load_finalizing().await?.is_some() {
            return Ok(());
        }
        // borrow を await の外で完結させる。1 回の HandleResult に盤面を進める指し手は
        // 高々 1 件なので、post-move の clock snapshot を全 move entry で共有できる。
        let spectator_clocks = if entries.iter().any(is_move_broadcast) {
            self.core.borrow().as_ref().map(|core| SpectatorClocks {
                black_remaining_ms: core.clock_remaining_main_ms(Color::Black).max(0) as u64,
                white_remaining_ms: core.clock_remaining_main_ms(Color::White).max(0) as u64,
                side_to_move: core.current_turn(),
            })
        } else {
            None
        };

        let clock_insert = spectator_clock_insert_after(entries);
        for (index, entry) in entries.iter().enumerate() {
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
                    self.send_to_spectators(entry.line.as_str(), entry.ply).await?;
                }
            }
            if matches!(entry.target, BroadcastTarget::All) {
                self.send_to_spectators(entry.line.as_str(), entry.ply).await?;
            }
            if let (Some(clocks), Some((insert_after, ply))) =
                (spectator_clocks.as_ref(), clock_insert)
                && index == insert_after
            {
                let clock_line = build_spectator_clock_update(clocks, ply);
                // payload の ply は client 側の position binding に使う。queue の ply は
                // move 重複排除専用なので、clock は None として必ず flush する。
                self.send_to_spectators(&clock_line, None).await?;
            }
        }
        Ok(())
    }

    /// 同一 room の対局者 + 観戦者全員へ chat を relay する。
    async fn relay_chat(&self, sender: &str, message: &str) -> Result<()> {
        let line = format!("##[CHAT] {sender}: {message}");
        self.send_to_role(Role::Black, &line).await?;
        self.send_to_role(Role::White, &line).await?;
        // chat は指し手では無いので ply = None (= snapshot 中の queue でも常に
        // flush される非指し手 broadcast)。
        self.send_to_spectators(&line, None).await
    }

    fn dispatch_terminal(&self, entries: &[crate::attachment::FinalizingBroadcast]) -> Result<()> {
        let mut first_error = None;
        for ws in self.state.get_websockets() {
            let mut att = match ws.deserialize_attachment::<WsAttachment>() {
                Ok(Some(att)) => att,
                Ok(None) => continue,
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            };
            if att.terminal_aborted() {
                continue;
            }
            let spectator = matches!(att, WsAttachment::Spectator { .. });
            let delivery = att.deliver_terminal(
                entries,
                |line| send_line(&ws, line),
                |attachment| ws.serialize_attachment(attachment),
                |code, reason| {
                    let _ = ws.close(Some(code), Some(reason.to_owned()));
                },
            );
            if let Err(error) = delivery {
                if spectator {
                    // 観戦者の配信障害で対局の確定を止めない。
                    let _ = ws.close(Some(1011), Some("terminal delivery failed".to_owned()));
                    crate::structured_log!(
                        event: "spectator_terminal_delivery_failed",
                        component: "game_room",
                        err: format!("{error:?}"),
                    );
                } else {
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn finalize_if_ended(&self, result: &HandleResult) -> Result<()> {
        self.finalize_with_trigger(result, false).await
    }

    async fn finalize_with_trigger(&self, result: &HandleResult, alarm_driven: bool) -> Result<()> {
        let HandleOutcome::GameEnded(_) = result.outcome else {
            return Ok(());
        };
        if self.finalizing.replace(true) {
            if let Some(verdict) = self.load_finalizing().await? {
                self.ensure_finalization_alarm(verdict.attempt, false).await?;
            }
            return Ok(());
        }
        let finalized = self.finalize_game(result, alarm_driven).await;
        self.finalizing.set(false);
        finalized
    }

    /// 終局の裁定と終局時刻を告知前に保存する。保存済みならそれを返し、
    /// 再試行でも最初に決めた裁定と終局時刻を使う。
    async fn record_verdict(&self, result: &HandleResult) -> Result<Option<FinalizingState>> {
        let HandleOutcome::GameEnded(game_result) = &result.outcome else {
            return Ok(None);
        };
        if let Some(saved) = self.load_finalizing().await? {
            return Ok(Some(saved));
        }
        let mut broadcasts: Vec<crate::attachment::FinalizingBroadcast> =
            result.broadcasts.iter().map(Into::into).collect();
        if let Some((index, ply)) = spectator_clock_insert_after(&result.broadcasts)
            && let Some(core) = self.core.borrow().as_ref()
        {
            let clocks = SpectatorClocks {
                black_remaining_ms: core.clock_remaining_main_ms(Color::Black).max(0) as u64,
                white_remaining_ms: core.clock_remaining_main_ms(Color::White).max(0) as u64,
                side_to_move: core.current_turn(),
            };
            broadcasts.insert(
                index + 1,
                crate::attachment::FinalizingBroadcast {
                    target: BroadcastTarget::Spectators,
                    line: build_spectator_clock_update(&clocks, ply),
                    // snapshot 後にも時計を更新するため、指し手の重複排除から外す。
                    ply: None,
                },
            );
        }
        let state = FinalizingState {
            result: game_result.clone(),
            ended_at_ms: self.now_ms(),
            broadcasts,
            attempt: 0,
        };
        self.state.storage().put(KEY_FINALIZING, &state).await?;
        Ok(Some(state))
    }

    async fn load_finalizing(&self) -> Result<Option<FinalizingState>> {
        self.state.storage().get(KEY_FINALIZING).await
    }

    async fn ensure_finalization_alarm(&self, attempt: u32, replace: bool) -> Result<()> {
        if replace || self.state.storage().get_alarm().await?.is_none() {
            let index =
                (attempt.saturating_sub(1) as usize).min(FINALIZE_RETRY_DELAYS_SEC.len() - 1);
            self.state
                .storage()
                .set_alarm(Duration::from_secs(FINALIZE_RETRY_DELAYS_SEC[index]))
                .await?;
        }
        Ok(())
    }

    async fn finalize_game(&self, result: &HandleResult, alarm_driven: bool) -> Result<()> {
        let Some(mut verdict) = self.record_verdict(result).await? else {
            return Ok(());
        };
        let advance = alarm_driven || verdict.attempt == 0;
        if advance {
            verdict.attempt = verdict.attempt.saturating_add(1);
        }
        // alarm の予約失敗を runtime が再試行しても、猶予を消費しない。
        // 予約後の保存失敗なら、前の attempt と予約済み alarm から再開できる。
        self.ensure_finalization_alarm(verdict.attempt, advance).await?;
        if advance {
            self.state.storage().put(KEY_FINALIZING, &verdict).await?;
        }
        if self.config.borrow().is_none() {
            let cfg = self.state.storage().get(KEY_CONFIG).await?;
            *self.config.borrow_mut() = cfg;
        }
        let game_result = &verdict.result;
        self.finalization_step(
            self.dispatch_terminal(&verdict.broadcasts),
            verdict.attempt,
            "terminal_delivery",
        )?;
        use rshogi_csa_server::record::kifu::primary_result_code;
        let code = primary_result_code(game_result).to_owned();
        let ended_at_ms = verdict.ended_at_ms;
        let attempt = self
            .finalization_step(
                self.export_kifu_to_r2(game_result, ended_at_ms).await,
                verdict.attempt,
                "export_load_moves",
            )?
            .unwrap_or(ExportAttempt::Skipped);
        self.finalization_step(
            self.try_persist_floodgate_history(game_result, &code, ended_at_ms).await,
            verdict.attempt,
            "floodgate_history",
        )?;
        // 外部 I/O の待機中に snapshot が完了した接続にも送る。送信済みの行は
        // attachment で除外されるので、既存接続へは再送しない。
        self.finalization_step(
            self.dispatch_terminal(&verdict.broadcasts),
            verdict.attempt,
            "terminal_delivery_after_export",
        )?;

        // export 全成功なら `exported_at_ms` を埋め、retry 経路は不要。
        // 一部失敗なら `exported_at_ms = None` で書き、後述の pending 経路で
        // 再 PUT を予約する。「retry できない skip 失敗」も `exported_at_ms = None`
        // にしておくと観測上は欠損として残る (運用上の手動補修対象)。
        let exported_at_ms = if attempt.is_complete() {
            Some(ended_at_ms)
        } else {
            None
        };
        let export_pending = attempt.into_pending();
        // 棋譜本文と失敗キーはここにしか無いため、確定より前に保存する。
        // 確定後に落ちると KEY_FINISHED が再開を止め、再試行の手掛かりが失われる。
        if let Some(pending) = &export_pending {
            self.state.storage().put(KEY_EXPORT_PENDING, pending).await?;
        } else if exported_at_ms.is_some() {
            // 前回 pending 保存後に KEY_FINISHED 保存が失敗していた場合の残骸。
            // 今回は全 PUT 成功済みなので、alarm を止める前に片付ける。
            self.state.storage().delete(KEY_EXPORT_PENDING).await?;
        }
        // export も再試行予約も無い場合は、棋譜の唯一の原本として moves を残す。
        let keep_moves = exported_at_ms.is_none() && export_pending.is_none();
        let finished = FinishedState {
            result_code: code,
            ended_at_ms,
            exported_at_ms,
        };
        self.state.storage().put(KEY_FINISHED, &finished).await?;
        // KEY_FINISHED が以後の再開を止めるため、削除失敗は無害。
        let _ = self.state.storage().delete(KEY_FINALIZING).await;
        // 終局保存に失敗しても alarm から再試行できるよう、解除は保存後に行う。
        // export の再試行が残る場合は、再試行 alarm の予約が成功するまで既存の
        // alarm を残す。予約前に止まっても、残った alarm が export 再試行へ回る。
        if export_pending.is_none() {
            let _ = self.state.storage().delete_alarm().await;
        }
        // grace / agree-timeout / time-up 系の alarm/registry を片付けてから
        // ExportRetry alarm を張る。順序を逆にすると `delete_grace_alarm_state`
        // が ExportRetry タグを巻き込んで消す race がある。
        self.delete_grace_alarm_state().await?;

        // pending は確定前に保存済み。種別タグと retry alarm を貼る。
        // alarm 書き込みは best-effort で失敗ログのみ残し、`finalize_if_ended` の
        // 残り処理 (live-games-index 削除 / WS close) を必ず進める (R2 export retry 主契約)。
        if let Some(pending) = export_pending {
            self.schedule_export_retry(pending).await;
        }

        // KEY_FINISHED が確定した後に live-games-index entry を best-effort で
        // 削除する (https://github.com/SH11235/rshogi/issues/549 §4)。「終局 entry も live entry も無い」矛盾
        // 状態を最小化するため、`KEY_FINISHED` put より後に delete を置く。
        // delete 失敗時は orphan として残るが、cron sweep (https://github.com/SH11235/rshogi/issues/551) が
        // 後追い掃除する契約。`play_started_at_ms` が None のまま終局した
        // (= AGREE 前 abnormal 等) ケースは live entry を put していないので
        // delete も skip する。
        //
        // delete key は put と同じ `live_games_index_key(play_started_at_ms, game_id)`
        // を使う契約 (https://github.com/SH11235/rshogi/issues/853)。in-memory
        // `self.config` は GameEnded finalize 到達時点で常に Some のはず (core と
        // lockstep で set される) だが、将来のリファクタや想定外の経路で None に
        // なっても live entry を取り残さないよう、無ければ永続化済み `KEY_CONFIG`
        // から読み直して delete key を組む。それでも config が取れない場合だけ、
        // 無言スキップせず error ログで可視化する (本 issue で「tail に delete
        // イベント・エラーが出ない」= 無言スキップが調査を難しくしたため)。
        let in_mem_cfg = self.config.borrow().clone();
        let cfg_for_delete: Option<PersistedConfig> = match in_mem_cfg {
            Some(c) => Some(c),
            None => self.state.storage().get(KEY_CONFIG).await.ok().flatten(),
        };
        match cfg_for_delete {
            Some(cfg) => {
                // `play_started_at_ms` が None なら live entry は未 put なので削除不要。
                if let Some(started_at_ms) = cfg.play_started_at_ms {
                    self.try_delete_live_games_index(&cfg, started_at_ms).await;
                }
            }
            None => {
                crate::structured_log!(
                    event: "live_games_index_delete_skip",
                    component: "game_room",
                    level: "error",
                    reason: "config_missing",
                );
            }
        }

        // moves テーブルを cleanup (https://github.com/SH11235/rshogi/issues/637)。
        // `KEY_FINISHED` put 後に呼ぶことで、削除失敗時も後続 `ensure_core_loaded`
        // が finished ガードで早期 return し replay は走らない (二重防御)。
        // 失敗は best-effort で吸収し、WS close 等の残り処理は必ず進める。
        if keep_moves {
            crate::structured_log!(
                event: "moves_kept_without_export",
                component: "game_room",
                level: "error",
            );
        } else {
            self.clear_moves().await;
        }

        // CoreRoom を落とす。再度 ensure_core_loaded しても finished ガードで戻る。
        self.core.borrow_mut().take();

        for ws in self.state.get_websockets() {
            match ws.deserialize_attachment::<WsAttachment>() {
                Ok(Some(att)) if att.terminal_aborted() => {
                    // abort 時の close 自体が失敗していても、行を再送せず切断を再試行する。
                    let _ = ws.close(Some(1011), Some("terminal delivery failed".to_owned()));
                }
                // 試行上限で配信を諦めた接続を正常終了に見せない。
                Ok(Some(att)) if !att.terminal_complete(&verdict.broadcasts) => {
                    let _ = ws.close(Some(1011), Some("terminal delivery failed".to_owned()));
                }
                Ok(Some(
                    WsAttachment::Player {
                        terminal_in_flight: Some(_),
                        ..
                    }
                    | WsAttachment::Spectator {
                        terminal_in_flight: Some(_),
                        ..
                    },
                ))
                | Err(_) => {
                    let _ = ws.close(Some(1011), Some("terminal delivery failed".to_owned()));
                }
                _ => {
                    let _ = ws.close(Some(1000), Some("game finished".to_owned()));
                }
            }
        }
        Ok(())
    }

    fn finalization_step<T>(
        &self,
        result: Result<T>,
        attempt: u32,
        step: &str,
    ) -> Result<Option<T>> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(error) if attempt as usize > FINALIZE_RETRY_DELAYS_SEC.len() => {
                crate::structured_log!(event: "finalization_retry_exhausted", component: "game_room",
                    level: "error", step: step, attempt: attempt, err: format!("{error:?}"));
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// 終局時 export PUT 失敗の retry を予約する。
    ///
    /// 本文は確定前に保存済み。タグと予約の失敗時は既存の finalization alarm が
    /// orphaned_export 分岐から再開するため、ここではログを残して処理を続ける。
    async fn schedule_export_retry(&self, pending: ExportPendingState) {
        if let Err(e) = self
            .state
            .storage()
            .put(KEY_PENDING_ALARM_KIND, &PendingAlarmKind::ExportRetry)
            .await
        {
            crate::structured_log!(
                event: "export_retry_kind_put_failed",
                component: "game_room",
                game_id: pending.game_id,
                err: format!("{e:?}"),
            );
            return;
        }
        let Some(delay_ms) = next_retry_delay_ms(pending.attempt) else {
            // 最初の予約で `attempt = 0` のため到達しない契約。防御的に log のみ。
            crate::structured_log!(
                event: "export_retry_initial_exhausted",
                component: "game_room",
                game_id: pending.game_id,
                attempt: pending.attempt,
            );
            return;
        };
        if let Err(e) = self.state.storage().set_alarm(Duration::from_millis(delay_ms)).await {
            crate::structured_log!(
                event: "export_retry_alarm_set_failed",
                component: "game_room",
                game_id: pending.game_id,
                delay_ms: delay_ms,
                err: format!("{e:?}"),
            );
        } else {
            crate::structured_log!(
                event: "export_retry_scheduled",
                component: "game_room",
                game_id: pending.game_id,
                attempt: pending.attempt,
                delay_ms: delay_ms,
                failed_count: pending.failed_keys.len(),
            );
        }
    }

    /// `PendingAlarmKind::ExportRetry` で alarm が発火したときの再 PUT 経路。
    ///
    /// `KEY_EXPORT_PENDING` から保存済の本文 + 失敗 key 一覧を読み、各 key に
    /// 対して再 PUT を試みる。全成功で `exported_at_ms` を埋めて pending を消し、
    /// 一部失敗なら `attempt` を進めて次の遅延 (`RETRY_DELAYS_SEC[attempt]`) で
    /// alarm を再予約する。retry 全消費後は `KEY_PENDING_ALARM_KIND` と alarm を
    /// 消し、pending entry のみ残置する (運用観測用)。
    async fn handle_export_retry_alarm(&self) -> Result<()> {
        let pending: Option<ExportPendingState> =
            self.state.storage().get(KEY_EXPORT_PENDING).await?;
        let Some(mut pending) = pending else {
            // pending が無い (race / 旧 deploy 状態の cleanup) は alarm タグだけ
            // 片付けて終了する。`KEY_FINISHED` 経路は既に確定済の前提。
            crate::structured_log!(
                event: "export_retry_no_pending",
                component: "game_room",
            );
            let _ = self.state.storage().delete(KEY_PENDING_ALARM_KIND).await;
            let _ = self.state.storage().delete_alarm().await;
            return Ok(());
        };

        // 打ち切り後に古い alarm が再配信されても R2 PUT を再開しない。
        // pending 本文は原本として残す。
        if is_exhausted(pending.attempt) {
            self.state.storage().delete(KEY_PENDING_ALARM_KIND).await?;
            self.state.storage().delete_alarm().await?;
            return Ok(());
        }

        let bucket = match self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "export_retry_bucket_missing",
                    component: "game_room",
                    game_id: pending.game_id,
                    attempt: pending.attempt,
                    err: format!("{e:?}"),
                );
                // bucket binding 不在 = config 不正。retry を進めても解決しない
                // ので alarm を停止し pending は残置 (deploy 修正で再開する想定、
                // 自動回復経路は持たない)。
                let _ = self.state.storage().delete(KEY_PENDING_ALARM_KIND).await;
                let _ = self.state.storage().delete_alarm().await;
                return Ok(());
            }
        };

        // R2 / 進捗保存 / 完了印のどこで中断しても再開できるよう先に予約する。
        // 最終試行でも完了処理の storage 障害に備え、同じ遅延で予約を残す。
        let delay_ms = next_retry_delay_ms(pending.attempt.saturating_add(1))
            .or_else(|| next_retry_delay_ms(pending.attempt))
            .ok_or_else(|| Error::RustError("export retry delay missing".into()))?;
        self.state.storage().set_alarm(Duration::from_millis(delay_ms)).await?;

        // 失敗 key を順に再 PUT。成功した key は `still_failed` に積まない。
        let mut still_failed: Vec<FailedExportObject> =
            Vec::with_capacity(pending.failed_keys.len());
        for failed in pending.failed_keys.drain(..) {
            let body: Vec<u8> = match failed.body_kind {
                ExportBodyKind::Csa => pending.csa_text.as_bytes().to_vec(),
                ExportBodyKind::Meta => pending.meta_body.clone(),
            };
            match bucket.put(&failed.key, body).execute().await {
                Ok(_) => {
                    crate::structured_log!(
                        event: "export_retry_put_ok",
                        component: "game_room",
                        game_id: pending.game_id,
                        key: failed.key,
                        attempt: pending.attempt,
                    );
                }
                Err(e) => {
                    crate::structured_log!(
                        event: "export_retry_put_failed",
                        component: "game_room",
                        game_id: pending.game_id,
                        key: failed.key,
                        attempt: pending.attempt,
                        err: format!("{e:?}"),
                    );
                    still_failed.push(failed);
                }
            }
        }

        if still_failed.is_empty() {
            // 全成功: pending を消し、`exported_at_ms` を埋め、alarm/タグを片付ける。
            self.complete_export_retry(&pending).await?;
            return Ok(());
        }

        // 一部失敗: attempt を進めて次回 alarm を予約する (上限超なら停止)。
        let next_attempt = pending.attempt.saturating_add(1);
        pending.failed_keys = still_failed;
        pending.attempt = next_attempt;

        if is_exhausted(next_attempt) {
            crate::structured_log!(
                event: "export_retry_exhausted",
                component: "game_room",
                game_id: pending.game_id,
                attempt: next_attempt,
                remaining: pending.failed_keys.len(),
            );
            // pending を最新 attempt で書き直して観測性を保ち、alarm/タグだけ
            // 停止する。
            self.state.storage().put(KEY_EXPORT_PENDING, &pending).await?;
            self.state.storage().delete(KEY_PENDING_ALARM_KIND).await?;
            self.state.storage().delete_alarm().await?;
            return Ok(());
        }

        self.state.storage().put(KEY_EXPORT_PENDING, &pending).await?;
        crate::structured_log!(event: "export_retry_rescheduled", component: "game_room",
            game_id: pending.game_id, attempt: next_attempt, delay_ms: delay_ms,
            failed_count: pending.failed_keys.len());
        Ok(())
    }

    /// 完了印を保存してから pending と alarm を消す。途中の失敗では予約済み
    /// alarm と本文を残して再開する。完了印の保存後は同じ R2 PUT を再実行しても安全。
    async fn complete_export_retry(&self, pending: &ExportPendingState) -> Result<()> {
        let mut finished = self
            .state
            .storage()
            .get::<FinishedState>(KEY_FINISHED)
            .await?
            .ok_or_else(|| Error::RustError("export retry: finished state missing".into()))?;
        finished.exported_at_ms = Some(pending.ended_at_ms);
        self.state.storage().put(KEY_FINISHED, &finished).await?;
        self.state.storage().delete(KEY_EXPORT_PENDING).await?;
        self.state.storage().delete(KEY_PENDING_ALARM_KIND).await?;
        self.state.storage().delete_alarm().await?;
        crate::structured_log!(event: "export_retry_completed", component: "game_room",
            game_id: pending.game_id, attempt: pending.attempt);
        Ok(())
    }

    /// 本文を組めない履歴読込障害は終局処理で再試行し、R2 PUT 障害は本文ごと別途再試行する。
    async fn export_kifu_to_r2(
        &self,
        game_result: &rshogi_csa_server::game::result::GameResult,
        ended_at_ms: u64,
    ) -> Result<ExportAttempt> {
        use rshogi_csa_server::record::kifu::{KifuMove, KifuRecord};

        let cfg = match self.config.borrow().as_ref() {
            Some(c) => c.clone(),
            None => {
                // PersistedConfig 未確定 = AGREE 前の致命的経路で finalize に
                // 入った race。CSA 本文を組み立てる材料がないので skip する。
                crate::structured_log!(
                    event: "export_skip",
                    component: "game_room",
                    reason: "config_missing",
                );
                return Ok(ExportAttempt::Skipped);
            }
        };

        let moves_rows = self.load_moves().await?;
        // MoveRow は client が送ってきた raw CSA 行 (`+7776FU,T3` や Floodgate
        // 形式 `+7776FU,'* 123 pv...`) を保持している。snapshot と同じ共有ヘルパ
        // (`parse_move_row_line` / `move_elapsed_secs`) で token / コメントを抽出し、
        // 消費時間は at_ms 差分から再接続時計補償を引いて秒に丸め、`KifuMove` に
        // 変換する。これでライブ `T` / snapshot / 終局棋譜が同じ実効経過時間に
        // なる。Floodgate の評価値 PV コメントは `KifuMove::comment` に残す。
        let first_prev_ms = cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms);
        let elapsed = move_elapsed_secs(&moves_rows, first_prev_ms);
        let mut kifu_moves: Vec<KifuMove> = Vec::with_capacity(moves_rows.len());
        for (m, sec) in moves_rows.iter().zip(elapsed) {
            let (token_str, comment) = parse_move_row_line(&m.line);
            kifu_moves.push(KifuMove {
                token: rshogi_csa_server::types::CsaMoveToken::new(token_str),
                elapsed_sec: sec,
                comment: comment.map(str::to_owned),
            });
        }

        // 後続の `KifuRecord::moves` で `kifu_moves` を move する前に手数を退避する。
        // `moves_count` は viewer 配信 API 用 index entry に埋め込む。
        let moves_count_for_index: u32 = kifu_moves.len() as u32;

        // `time_section` は clock の初期設定値に依存し、持ち時間の残量には
        // 左右されないので cfg から再構築しても同じ出力になる。
        let time_section = cfg.clock.format_time_section();

        let start_str = format_csa_datetime(cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms));
        let end_str = format_csa_datetime(ended_at_ms);

        let initial_position = match cfg.initial_sfen.as_deref() {
            Some(sfen) => match position_section_from_sfen(sfen) {
                Ok(p) => p,
                Err(reason) => {
                    crate::structured_log!(
                        event: "export_skip",
                        component: "game_room",
                        game_id: cfg.game_id,
                        reason: "invalid_sfen",
                        detail: reason,
                    );
                    return Ok(ExportAttempt::Skipped);
                }
            },
            None => standard_initial_position_block(),
        };

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
            initial_position,
            moves: kifu_moves,
            result: game_result.clone(),
        };
        let text = record.build_v2();

        let date_path = format_date_path(cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms));
        let date_key = format!("{date_path}/{}.csa", cfg.game_id);
        let by_id_key = kifu_by_id_object_key(&cfg.game_id);

        let bucket = match self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "export_skip",
                    component: "game_room",
                    game_id: cfg.game_id,
                    reason: "bucket_binding",
                    err: format!("{e:?}"),
                );
                return Ok(ExportAttempt::Skipped);
            }
        };

        // 4 つの PUT を集約する。各 PUT は独立して試行し、失敗した key だけを
        // `failed_keys` に積む。CSA 本文 PUT が失敗しても meta / index PUT は試す
        // (best-effort 全 put → retry で残りを順送り)。
        let mut failed_keys: Vec<FailedExportObject> = Vec::with_capacity(4);
        let csa_bytes = text.as_bytes().to_vec();
        for (key, label) in [
            (date_key.as_str(), "date_key"),
            (by_id_key.as_str(), "by_id_key"),
        ] {
            if let Err(e) = bucket.put(key, csa_bytes.clone()).execute().await {
                crate::structured_log!(
                    event: "export_put_failed",
                    component: "game_room",
                    game_id: cfg.game_id,
                    label: label,
                    key: key,
                    err: format!("{e:?}"),
                );
                failed_keys.push(FailedExportObject {
                    key: key.to_owned(),
                    body_kind: ExportBodyKind::Csa,
                });
            }
        }

        // viewer 配信 API 用の正準メタ (`kifu-by-id/<id>.meta.json`) と派生
        // インデックス (`games-index/<inv>-<id>.json`)。
        //
        // 書き込み順序 (https://github.com/SH11235/rshogi/issues/551 設計 v3 §1):
        //   1. csa 本文 / by-id (上で完了)
        //   2. `kifu-by-id/<id>.meta.json` — backfill / orphan sweep の真の判定
        //      キー (primary)。
        //   3. `games-index/<inv>-<id>.json` — meta から派生する一覧索引
        //      (secondary)。failure 時は backfill cron が meta を起点に再生成する。
        //
        // `source` 判定は `live-games-index/` の対局開始時 entry と完全に揃える
        // ため、`games_index::resolve_index_source` 共通 helper に集約済 (Issue
        // #549 設計 v3 §3)。
        let source = resolve_index_source(&self.env);

        let (result_kind, end_reason) = classify_result(game_result);
        let black_player_id = if cfg.black_player_id.is_empty() {
            crate::player_identity::legacy_player_id(&cfg.black_handle)
        } else {
            cfg.black_player_id.clone()
        };
        let white_player_id = if cfg.white_player_id.is_empty() {
            crate::player_identity::legacy_player_id(&cfg.white_handle)
        } else {
            cfg.white_player_id.clone()
        };
        let entry = GamesIndexEntry {
            game_id: &cfg.game_id,
            started_at_ms: cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms),
            ended_at_ms,
            black_handle: &cfg.black_handle,
            white_handle: &cfg.white_handle,
            black_player_id: Some(&black_player_id),
            white_player_id: Some(&white_player_id),
            result_kind,
            end_reason,
            // CSA は理論上 0..=u32::MAX 手は到達しないが、API contract は
            // 整数手数を要求するため u32 に正規化する。`DEFAULT_MAX_MOVES = 256`
            // 上限のため通常は 0..=256 に収まる。
            moves_count: moves_count_for_index,
            clock: IndexClockSpec::from_server(&cfg.clock),
            source,
        };

        // 1 度シリアライズして meta primary / games-index secondary の双方に
        // 流用する。serialize 失敗は両 put を skip して終了する (どちらも meta
        // 起点なので片方だけ書く意味がない)。
        let body = match serde_json::to_vec(&entry) {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "games_index_skip",
                    component: "game_room",
                    game_id: cfg.game_id,
                    reason: "serialize",
                    err: format!("{e:?}"),
                );
                // CSA 本文 PUT は試行済 (failed_keys に積まれているかも) だが、
                // meta/index は serialize 失敗でこの経路で PUT 試行できていない。
                // 4 PUT 全成功にはなり得ないので `from_partial_attempt` で
                // `Complete` を絶対返さない経路に倒す。
                return Ok(ExportAttempt::from_partial_attempt(
                    cfg.game_id.clone(),
                    ended_at_ms,
                    text,
                    Vec::new(),
                    failed_keys,
                ));
            }
        };

        let meta_key = kifu_by_id_meta_key(&cfg.game_id);
        if let Err(e) = bucket.put(&meta_key, body.clone()).execute().await {
            crate::structured_log!(
                event: "kifu_by_id_meta_put_failed",
                component: "game_room",
                game_id: cfg.game_id,
                meta_key: meta_key,
                err: format!("{e:?}"),
            );
            failed_keys.push(FailedExportObject {
                key: meta_key.clone(),
                body_kind: ExportBodyKind::Meta,
            });
        }

        let index_key = match games_index_key(ended_at_ms, &cfg.game_id) {
            Ok(k) => k,
            Err(e) => {
                crate::structured_log!(
                    event: "games_index_skip",
                    component: "game_room",
                    game_id: cfg.game_id,
                    reason: "key",
                    err: format!("{e:?}"),
                );
                // games-index key 生成失敗は retry 不可。pending には CSA / meta
                // のみ残す。index PUT が抜けるため `from_partial_attempt` で
                // `Complete` 経路を塞ぐ。
                return Ok(ExportAttempt::from_partial_attempt(
                    cfg.game_id.clone(),
                    ended_at_ms,
                    text,
                    body,
                    failed_keys,
                ));
            }
        };
        let games_index_written = match bucket.put(&index_key, body.clone()).execute().await {
            Ok(_) => true,
            Err(e) => {
                crate::structured_log!(
                    event: "games_index_put_failed",
                    component: "game_room",
                    game_id: cfg.game_id,
                    inv_key: index_key,
                    err: format!("{e:?}"),
                );
                failed_keys.push(FailedExportObject {
                    key: index_key.clone(),
                    body_kind: ExportBodyKind::Meta,
                });
                false
            }
        };

        // R2 が正本なので D1 検索 index は best-effort。欠落は cron backfill が
        // 後から補完し、ここでの失敗は対局終了・R2 retry 判定へ影響させない。
        if games_index_written
            && let Err(e) = crate::games_search_index::upsert_entry(&self.env, &entry).await
        {
            crate::structured_log!(
                event: "games_search_index_upsert_failed",
                component: "game_room",
                game_id: cfg.game_id,
                err: format!("{e:?}"),
            );
        }

        if failed_keys.is_empty() {
            crate::structured_log!(
                event: "export_complete",
                component: "game_room",
                game_id: cfg.game_id,
                key: date_key,
            );
        }
        // 4 オブジェクトすべての PUT を試行できた経路。`failed_keys` の中身で
        // `Complete` / `Pending` を選ぶ。
        Ok(ExportAttempt::from_full_attempt(
            cfg.game_id.clone(),
            ended_at_ms,
            text,
            body,
            failed_keys,
        ))
    }

    async fn try_persist_floodgate_history(
        &self,
        game_result: &rshogi_csa_server::game::result::GameResult,
        result_code: &str,
        ended_at_ms: u64,
    ) -> Result<()> {
        let storage = match resolve_floodgate_history_storage(&self.env) {
            Ok(Some(s)) => s,
            Ok(None) => return Ok(()),
            Err(e) => {
                // `Err` 経路は `parse_allow_floodgate_features` の解析エラーまたは
                // `validate_floodgate_feature_gate` の opt-in 漏れ等の **設定不正**。
                // opt-in 未有効 (`Ok(None)`) と区別がつくよう "config error" を明示する。
                crate::structured_log!(
                    event: "floodgate_history_config_error",
                    component: "game_room",
                    err: format!("{e}"),
                );
                return Ok(());
            }
        };

        let cfg = match self.config.borrow().as_ref() {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        let start_ms = cfg.play_started_at_ms.unwrap_or(cfg.matched_at_ms);
        let entry = FloodgateHistoryEntry {
            game_id: cfg.game_id.clone(),
            game_name: cfg.game_name.clone(),
            black: cfg.black_handle.clone(),
            white: cfg.white_handle.clone(),
            start_time: format_rfc3339_utc(start_ms),
            end_time: format_rfc3339_utc(ended_at_ms),
            result_code: result_code.to_owned(),
            winner: winner_of(game_result).map(HistoryColor::from),
        };

        storage
            .append(&entry)
            .await
            .map_err(|error| Error::RustError(format!("{error:?}")))
    }

    /// マッチ開始直前の致命的条件（buoy 枯渇等）で対局を開始できない場合に、
    /// 既に LOGIN OK を受けている Player ロールの WS 全員にエラー行を送出し、
    /// 接続を閉じてスロットを空にする。
    ///
    /// ここでクリアしないと、スロットは Match 状態のまま残ってしまい 2 人目に
    /// Game_Summary もエラーも届かないため、部屋が永久に詰まる。
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
    /// 観戦者や対局進行を止めず、エラーは log に落として継続する。観戦者 1 人の
    /// 切断が DO を不安定化させないようにする。
    ///
    /// `ply` は指し手 broadcast の場合の手数 (1 始まり)。指し手以外
    /// (START / 終局通知 / CHAT 等) では `None` を渡す。snapshot 送信中の ws へは
    /// この行を per-ws pending queue に積み、send は飛ばす (= snapshot 完了後に
    /// flush 経路で `ply > last_ply_in_snapshot` のみ送出される)。
    async fn send_to_spectators(&self, line: &str, ply: Option<u32>) -> Result<()> {
        for ws in self.state.get_websockets() {
            let att: Option<WsAttachment> =
                ws.deserialize_attachment::<WsAttachment>().ok().flatten();
            let Some(WsAttachment::Spectator {
                room_id,
                snapshot_in_progress,
                last_ply_in_snapshot,
                mut pending_queue,
                terminal_sent,
                terminal_in_flight,
                terminal_aborted,
            }) = att
            else {
                continue;
            };
            if snapshot_in_progress {
                // https://github.com/SH11235/rshogi/issues/627: queue を push する前に上限を判定する。
                // - 行数 > `MAX_SPECTATOR_QUEUE_ITEMS`
                // - bytes (各 line の `len()` 総和 + 新規 line 分) > `MAX_SPECTATOR_QUEUE_BYTES`
                // のいずれか満たす場合は、attachment を上書きせずに観戦者を切断する。
                // serialize_attachment をスキップすることで、close 後の hibernation
                // 復帰時にも肥大化した queue が残らないことを保証する。
                let next_items = pending_queue.len().saturating_add(1);
                let current_bytes: usize = pending_queue.iter().map(|(s, _)| s.len()).sum();
                let next_bytes = current_bytes.saturating_add(line.len());
                if next_items > MAX_SPECTATOR_QUEUE_ITEMS || next_bytes > MAX_SPECTATOR_QUEUE_BYTES
                {
                    crate::structured_log!(
                        event: "spectator_queue_overflow",
                        component: "game_room",
                        room_id: room_id,
                        items: next_items,
                        bytes: next_bytes,
                        limit_items: MAX_SPECTATOR_QUEUE_ITEMS,
                        limit_bytes: MAX_SPECTATOR_QUEUE_BYTES,
                    );
                    let _ = ws.close(Some(1009), Some("spectator queue overflow".to_owned()));
                    continue;
                }
                // snapshot 送信中は queue に積むだけ。flush 経路で重複手は弾く。
                pending_queue.push((line.to_owned(), ply));
                let updated = WsAttachment::Spectator {
                    room_id,
                    snapshot_in_progress,
                    last_ply_in_snapshot,
                    pending_queue,
                    terminal_sent,
                    terminal_in_flight,
                    terminal_aborted,
                };
                if let Err(e) = ws.serialize_attachment(&updated) {
                    crate::structured_log!(
                        event: "spectator_queue_serialize_failed",
                        component: "game_room",
                        err: format!("{e:?}"),
                    );
                }
                continue;
            }
            if let Err(e) = send_line(&ws, line) {
                crate::structured_log!(
                    event: "spectator_send_failed",
                    component: "game_room",
                    err: format!("{e:?}"),
                );
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

    /// 保存済み裁定は replay できない投了や反則も含むため、復元より優先する。
    async fn ensure_core_loaded(&self) -> Result<()> {
        if self.load_finished().await?.is_some() {
            self.core.borrow_mut().take();
            return Ok(());
        }
        if self.load_finalizing().await?.is_some() {
            return self.resume_pending_finalization(false).await;
        }
        // 未保存の終局は着手保存の中断で生じ得るため、必ず履歴から判定し直す。
        if self
            .core
            .borrow()
            .as_ref()
            .is_some_and(|core| matches!(core.status(), rshogi_csa_server::GameStatus::Finished(_)))
        {
            self.core.borrow_mut().take();
        }
        if let Some((cfg, reason)) = self.restore_core().await? {
            self.force_finalize_unrecoverable(&cfg, reason).await?;
        }
        self.resume_pending_finalization(false).await?;
        if self.core.borrow().is_some() {
            self.retry_live_games_index_if_needed().await?;
        }
        Ok(())
    }

    async fn resume_pending_finalization(&self, alarm_driven: bool) -> Result<()> {
        if self.load_finished().await?.is_some() {
            self.core.borrow_mut().take();
            return Ok(());
        }
        if let Some(saved) = self.load_finalizing().await? {
            self.core.borrow_mut().take();
            let result = HandleResult {
                outcome: HandleOutcome::GameEnded(saved.result),
                broadcasts: saved
                    .broadcasts
                    .into_iter()
                    .map(|entry| BroadcastEntry {
                        target: entry.target,
                        line: CsaLine::new(entry.line),
                        ply: entry.ply,
                    })
                    .collect(),
            };
            self.finalize_with_trigger(&result, alarm_driven).await?;
        }
        Ok(())
    }

    /// snapshot の END より先に通知・切断しないよう、復元不能でも確定を呼び出し側へ委ねる。
    async fn restore_core(&self) -> Result<Option<(PersistedConfig, &'static str)>> {
        if self.core.borrow().is_some() {
            return Ok(None);
        }
        if self.load_finished().await?.is_some() {
            return Ok(None);
        }
        let cfg_opt: Option<PersistedConfig> = self.state.storage().get(KEY_CONFIG).await?;
        let Some(cfg) = cfg_opt else {
            return Ok(None);
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
            ReplaySummary::Restored {
                mut core,
                last_result,
            } => {
                // `replay_core_room` は確定済み各手の credit を適用済み。まだ着手
                // されていない現手番にも先行保存済みの次 ply credit があれば戻す。
                let next_ply = i64::from(core.moves_played()).saturating_add(1);
                if let Some(credit) = self.load_reconnect_clock_credit(next_ply)? {
                    core.restore_current_turn_reconnect_credit_ms(
                        credit.max(0) as u64,
                        cfg.clock.reconnect_compensation_limit_ms(),
                    );
                }
                // `core` は `Box<CoreRoom>` で返るためここで unbox する
                // (`ReplaySummary` の variant 間サイズ差対策、persistence.rs 参照)。
                *self.core.borrow_mut() = Some(*core);
                *self.config.borrow_mut() = Some(cfg);
                if let Some(result) = pending_finalization(last_result) {
                    self.record_verdict(&result).await?;
                }

                // 計時起点は「直前の指し手の at_ms（初手前は play_started_at_ms）」で
                // 統一し、live / replay / alarm すべてで同じ規則を使う
                // (https://github.com/SH11235/rshogi/issues/852)。replay 完了時点で
                // core の turn_started_at_ms は最終手の at_ms に一致している
                // (`apply_move` が各手で now_ms=at_ms へ更新するため) ので、cold-start
                // でも起点を「今」へ張り直さない。張り直すと休眠を跨いだ長考が毎回
                // チャラになり、実ギャップ全額課金の replay 計時と乖離して幽霊 TimeUp
                // → core 復元失敗で対局が凍結・live 永久残留する。休眠を跨いだ長考の
                // 時間切れは `alarm` ハンドラが真の残時間 (`current_turn_remaining_ms`)
                // で判定して force_time_up する。
            }
            ReplaySummary::InvalidSfen { reason } => {
                crate::structured_log!(
                    event: "replay_invalid_sfen",
                    component: "game_room",
                    level: "error",
                    reason: reason,
                );
                return Ok(Some((cfg, "replay_invalid_sfen")));
            }
            ReplaySummary::UnknownColor { ply, color } => {
                crate::structured_log!(
                    event: "replay_unknown_color",
                    component: "game_room",
                    level: "error",
                    ply: ply,
                    color: color,
                );
                return Ok(Some((cfg, "replay_unknown_color")));
            }
            ReplaySummary::MoveReplayFailed { ply, line, reason } => {
                crate::structured_log!(
                    event: "replay_move_failed",
                    component: "game_room",
                    level: "error",
                    ply: ply,
                    line: line,
                    reason: reason,
                );
                return Ok(Some((cfg, "replay_move_failed")));
            }
        }
        Ok(None)
    }

    /// cold-start replay が非 Restored (復元不能) だったとき、対局を `#ABNORMAL`
    /// で強制終局させ、live-games-index から確実に外す安全網
    /// (https://github.com/SH11235/rshogi/issues/852)。
    ///
    /// root fix (計時統一) 後は幽霊 TimeUp 自体が起きないが、永続化データの破損等で
    /// replay できないケースは残る。従来はログのみで core=None のままだったため、
    /// 着手が無言破棄され、観戦 snapshot は空、時間切れ alarm も no-op、finalize
    /// 不発で live に永久残留していた。本メソッドは:
    ///
    /// 1. `KEY_FINISHED` を `#ABNORMAL`（勝敗不明の中断扱い、`GameResult::Abnormal`）
    ///    で確定させ、以後の着手を終局済み扱いにする
    /// 2. grace / alarm 系タグを片付ける
    /// 3. live-games-index entry を削除する (play_started 済みで put 済みのケース)
    /// 4. 復元できなかった moves を原本として保存する
    /// 5. error レベルの構造化ログを出す (root fix 後は稀な経路なので alert 対象)
    /// 6. 両 WS を閉じて「終局として応答」する (無言破棄をやめる)
    ///
    /// R2 棋譜 export は局面を組めない (= replay できない) ため行わず、
    /// `exported_at_ms: None` を残す。
    async fn force_finalize_unrecoverable(
        &self,
        cfg: &PersistedConfig,
        reason_event: &str,
    ) -> Result<()> {
        use rshogi_csa_server::game::result::GameResult;
        use rshogi_csa_server::record::kifu::primary_result_code;

        // 既に終局済みなら二重終局しない (race ガード)。
        if self.load_finished().await?.is_some() {
            return Ok(());
        }

        if self.load_finalizing().await?.is_some() {
            return self.resume_pending_finalization(false).await;
        }

        let ended_at_ms = self.now_ms();
        let result_code = primary_result_code(&GameResult::Abnormal { winner: None });
        let finished = FinishedState {
            result_code: result_code.to_owned(),
            ended_at_ms,
            exported_at_ms: None,
        };
        // KEY_FINISHED を最優先で確定させ、以後の着手が終局済み扱いになるようにする。
        self.state.storage().put(KEY_FINISHED, &finished).await?;

        // grace / agree-timeout / turn alarm 系のタグを片付ける。
        self.delete_grace_alarm_state().await?;

        // live-games-index を削除する (play_started 済み = entry を put 済みの
        // ケースのみ対象。未 put の場合は削除不要)。
        if let Some(started_at_ms) = cfg.play_started_at_ms {
            self.try_delete_live_games_index(cfg, started_at_ms).await;
        }

        // export していないため moves は唯一の原本。復元不能でも破棄しない。
        // KEY_FINISHED ガードにより再 replay は走らない。

        // core は生成できていない (= None) が、防御的に take しておく。
        self.core.borrow_mut().take();

        crate::structured_log!(
            event: "unrecoverable_force_finalized",
            component: "game_room",
            level: "error",
            game_id: cfg.game_id,
            reason_event: reason_event,
            result_code: result_code,
        );

        // 結果を配信していないため、正常完了として閉じない。
        for ws in self.state.get_websockets() {
            let _ = ws.close(Some(1011), Some("game finished (unrecoverable state)".to_owned()));
        }
        Ok(())
    }

    /// `live-games-index/<inv>-<id>.json` の put が `mark_play_started` 経路で
    /// 抜けた／hibernation で isolate が落ちた場合のリカバリ。
    ///
    /// 条件: 同 isolate 寿命中に未 put (`live_index_put_done == false`) かつ
    /// `play_started_at_ms.is_some()` (= 対局開始済) かつ未終局
    /// (`load_finished == None`)。3 つすべて満たすときに 1 回だけ
    /// `try_put_live_games_index` を呼ぶ。put 成功で flag が立つので、同
    /// isolate 内の後続 `ensure_core_loaded` では re-entry しない。
    async fn retry_live_games_index_if_needed(&self) -> Result<()> {
        if self.live_index_put_done.get() {
            return Ok(());
        }
        if self.load_finished().await?.is_some() {
            return Ok(());
        }
        let cfg_for_put: Option<PersistedConfig> = self
            .config
            .borrow()
            .as_ref()
            .filter(|c| c.play_started_at_ms.is_some())
            .cloned();
        if let Some(c) = cfg_for_put {
            self.try_put_live_games_index(&c).await;
        }
        Ok(())
    }

    /// 初めて `HandleOutcome::GameStarted` を観測した時刻を cfg に書き込む。
    /// 2 手目以降は冪等に no-op として扱い、storage への再書き込みを避ける。
    ///
    /// `KEY_CONFIG` の put 成功後にのみ live-games-index entry の put を
    /// best-effort で試みる。put 成功時のみ `live_index_put_done` を立てて、
    /// 同 isolate 内で `ensure_core_loaded` 経由の retry が走らないようにする。
    /// put 失敗時は flag を立てず、次回の `ensure_core_loaded` 呼び出しで
    /// retry させる (R2 put は同一キーで idempotent)。
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
            // KEY_CONFIG put 成功後にのみ live index put を試みる。これが
            // Err を返しても finalize 経路ではないので最終的な動作には影響
            // しない (best-effort)。
            self.try_put_live_games_index(&c).await;
        }
        Ok(())
    }

    /// `live-games-index/<inv>-<id>.json` を best-effort で put する。
    ///
    /// `cfg.play_started_at_ms` が `None` の場合は live index put が成立しない
    /// (= mark_play_started 前) ため早期 return する。すべての失敗 (key
    /// validation / serialize / R2 put) を [`structured_log!`](crate::structured_log)
    /// で吸収し、`Result` は返さない。put 成功時のみ `live_index_put_done` を
    /// `true` にセットする。
    async fn try_put_live_games_index(&self, cfg: &PersistedConfig) {
        let Some(started_at_ms) = cfg.play_started_at_ms else {
            return;
        };

        let key = match live_games_index_key(started_at_ms, &cfg.game_id) {
            Ok(k) => k,
            Err(e) => {
                crate::structured_log!(
                    event: "live_games_index_skip",
                    component: "game_room",
                    game_id: cfg.game_id,
                    reason: "key",
                    err: format!("{e:?}"),
                );
                return;
            }
        };

        let source = resolve_index_source(&self.env);
        let entry = LiveGamesIndexEntry {
            game_id: &cfg.game_id,
            started_at_ms,
            black_handle: &cfg.black_handle,
            white_handle: &cfg.white_handle,
            clock: IndexClockSpec::from_server(&cfg.clock),
            source,
        };

        let body = match serde_json::to_vec(&entry) {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "live_games_index_skip",
                    component: "game_room",
                    game_id: cfg.game_id,
                    reason: "serialize",
                    err: format!("{e:?}"),
                );
                return;
            }
        };

        let bucket = match self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "live_games_index_skip",
                    component: "game_room",
                    game_id: cfg.game_id,
                    reason: "bucket",
                    err: format!("{e}"),
                );
                return;
            }
        };

        match bucket.put(&key, body).execute().await {
            Ok(_) => {
                self.live_index_put_done.set(true);
            }
            Err(e) => {
                crate::structured_log!(
                    event: "live_games_index_put_failed",
                    component: "game_room",
                    game_id: cfg.game_id,
                    key: key,
                    err: format!("{e:?}"),
                );
            }
        }
    }

    /// `live-games-index/<inv>-<id>.json` を best-effort で delete する。
    ///
    /// 終局確定 (`KEY_FINISHED` put 成功) 直後に呼び、live 一覧から進行中
    /// 表示を消す。R2 delete は idempotent なので、transient error には最大
    /// [`LIVE_INDEX_DELETE_MAX_ATTEMPTS`] 回まで retry し
    /// (https://github.com/SH11235/rshogi/issues/629)、各 attempt 間に
    /// [`LIVE_INDEX_DELETE_BACKOFF_MS`] の wall-clock 待機を挟む。
    ///
    /// 全試行が失敗した場合は `live_games_index_delete_giveup` イベントを記録
    /// して諦める。残った orphan は
    /// https://github.com/SH11235/rshogi/issues/551 の sweep ジョブ
    /// (`run_live_orphan_sweep`) が 15 分以内に掃除する契約
    /// (https://github.com/SH11235/rshogi/issues/629 で 1 時間 → 15 分に短縮)。
    ///
    /// key 生成失敗 / bucket binding 解決失敗は retry 不能なので 1 回で諦める
    /// (`live_games_index_delete_skip` を出して return)。
    async fn try_delete_live_games_index(&self, cfg: &PersistedConfig, started_at_ms: u64) {
        let key = match live_games_index_key(started_at_ms, &cfg.game_id) {
            Ok(k) => k,
            Err(e) => {
                crate::structured_log!(
                    event: "live_games_index_delete_skip",
                    component: "game_room",
                    game_id: cfg.game_id,
                    reason: "key",
                    err: format!("{e:?}"),
                );
                return;
            }
        };

        let bucket = match self.env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "live_games_index_delete_skip",
                    component: "game_room",
                    game_id: cfg.game_id,
                    reason: "bucket",
                    err: format!("{e}"),
                );
                return;
            }
        };

        for attempt in 0..LIVE_INDEX_DELETE_MAX_ATTEMPTS {
            match bucket.delete(&key).await {
                Ok(_) => return,
                Err(e) => {
                    crate::structured_log!(
                        event: "live_games_index_delete_failed",
                        component: "game_room",
                        game_id: cfg.game_id,
                        key: key,
                        attempt: attempt + 1,
                        err: format!("{e:?}"),
                    );
                    // 最終 attempt の後は backoff せず giveup ログへ抜ける。
                    if let Some(&backoff_ms) = LIVE_INDEX_DELETE_BACKOFF_MS.get(attempt as usize) {
                        Delay::from(std::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
            }
        }
        crate::structured_log!(
            event: "live_games_index_delete_giveup",
            component: "game_room",
            game_id: cfg.game_id,
            key: key,
            attempts: LIVE_INDEX_DELETE_MAX_ATTEMPTS,
        );
    }

    /// `moves` テーブルを ply 昇順で読み出し、同じ ply の再接続時計補償を結合する。
    async fn load_moves(&self) -> Result<Vec<MoveRow>> {
        let sql = self.state.storage().sql();
        let cursor = sql.exec(
            "SELECT m.ply, m.color, m.line, m.at_ms, \
                    COALESCE(c.credited_ms, 0) AS reconnect_credit_ms \
             FROM moves AS m \
             LEFT JOIN reconnect_clock_credits AS c ON c.ply = m.ply \
             ORDER BY m.ply ASC",
            None,
        )?;
        let rows: Vec<MoveRow> = cursor.to_array()?;
        Ok(rows)
    }

    /// まだ `moves` に入っていない現手番を含め、指定 ply の再接続時計補償を読む。
    fn load_reconnect_clock_credit(&self, ply: i64) -> Result<Option<i64>> {
        #[derive(Deserialize)]
        struct CreditRow {
            credited_ms: i64,
        }
        let cursor = self.state.storage().sql().exec(
            "SELECT credited_ms FROM reconnect_clock_credits WHERE ply = ?",
            vec![ply.into()],
        )?;
        let rows: Vec<CreditRow> = cursor.to_array()?;
        Ok(rows.into_iter().next().map(|row| row.credited_ms))
    }

    /// `moves` と `reconnect_clock_credits` テーブルを空にする
    /// (https://github.com/SH11235/rshogi/issues/637)。
    ///
    /// 終局時に `finalize_if_ended` から呼び出され、同 room_id を再利用するリファクタ
    /// が将来入った場合でも `replay_core_room` が古い moves を再生して
    /// `MoveReplayFailed` を起こすのを防ぐ。現状は `KEY_FINISHED` ガードで弾かれる
    /// 経路だが、二重防御として moves 行を確実に削除しておく。
    ///
    /// 失敗は best-effort で [`structured_log!`](crate::structured_log) のみに落とし、
    /// `Result` には漏らさない (呼び出し側 `finalize_if_ended` の WS close /
    /// live-games-index delete などの終局処理を止めないため)。`KEY_FINISHED` put が
    /// 成功している前提で呼び出すため、削除失敗で moves が残っても、後続
    /// `ensure_core_loaded` は finished ガードで早期 return し replay は走らない。
    async fn clear_moves(&self) {
        let sql = self.state.storage().sql();
        if let Err(e) = sql.exec("DELETE FROM moves", None) {
            crate::structured_log!(
                event: "clear_moves_failed",
                component: "game_room",
                err: format!("{e:?}"),
            );
        }
        if let Err(e) = sql.exec("DELETE FROM reconnect_clock_credits", None) {
            crate::structured_log!(
                event: "clear_reconnect_clock_credits_failed",
                component: "game_room",
                err: format!("{e:?}"),
            );
        }
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
        let color_str = color_to_str(color);
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

    /// websocket_close で対局中の切断を grace registry に登録する経路。
    ///
    /// 1. `CoreRoom` を確保 (cold start なら replay) し、現在局面のスナップショット
    ///    と切断側用の Game_Summary 文字列 (現在盤面で再構築) を組み立てる
    /// 2. `PersistedConfig` から切断側に発行済の `reconnect_token` を取り出す
    ///    （未発行なら grace 経路に乗らないので Err で旧経路にフォールバック）
    /// 3. `PendingReconnect` を `KEY_GRACE_REGISTRY` に保存
    /// 4. alarm 種別を `GraceExpired` でマークし、`grace_duration` 後に発火する
    ///    `state.alarm()` を予約する (既存 turn alarm より早い場合のみ上書きする
    ///    ため、現状予約済の alarm を `get_alarm` で読み取って比較する)
    async fn enter_grace_window(&self, role: Role, grace_duration: Duration) -> Result<()> {
        self.ensure_core_loaded().await?;
        let cfg = self
            .config
            .borrow()
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::RustError("enter_grace_window: config missing".into()))?;
        // 既存 turn alarm の予定発火時刻を grace 経路前に取得する。grace deadline
        // と比較して早い方を残すために使い、再接続成立後の turn alarm は core の
        // 固定計時起点 + 現在予算 + 永続化済み累積 credit から再構成する。
        let existing_alarm = self.state.storage().get_alarm().await.ok().flatten();
        let pending = {
            let borrow = self.core.borrow();
            let core = borrow.as_ref().ok_or_else(|| {
                Error::RustError("enter_grace_window: core missing after ensure_core_loaded".into())
            })?;
            self.build_pending_reconnect(core, &cfg, role, grace_duration)?
        };
        // 既存の turn alarm より grace deadline が早ければ上書き、遅ければ既存
        // alarm (時間切れ) を残す。`get_alarm` は次回発火時刻 (epoch ms) を返し、
        // 未予約なら `None`。
        let now_ms = self.now_ms();
        let grace_deadline_ms = pending.deadline_ms;
        let (alarm_kind, should_set_grace) =
            classify_alarm_after_enter_grace(existing_alarm, grace_deadline_ms);

        // `KEY_PENDING_ALARM_KIND` → `KEY_GRACE_REGISTRY` の順で put する。
        // `put_multiple` を使うとローカル struct の
        // `#[serde(rename = "...")]` が各定数と一致する暗黙契約になり、定数を
        // rename した際に silent failure となるリスクがあるため、定数を直接
        // `put` に渡す形に分解する。
        //
        // 2 回の awaited put は単一 transaction ではないため、DO crash 等で
        // 中間状態が persist し得る。順序を「kind 先 → registry 後」と固定する
        // ことで、中間状態は常に `kind=GraceExpired/TimeUp かつ registry=None`
        // のみとなる。alarm 発火時は `handle_grace_expired_alarm` が registry
        // 不在を検出して `delete_grace_alarm_state` で kind を片付け、`Ok` で
        // return するため、orphan が残らない。逆順 (registry 先) だと「registry
        // 有 / kind 無」状態が起こり得て、`alarm()` が kind=None を TimeUp と
        // 解釈し `force_time_up` を走らせる経路が成立するため、この順序固定は
        // 防御として効く。
        self.state.storage().put(KEY_PENDING_ALARM_KIND, &alarm_kind).await?;
        self.state.storage().put(KEY_GRACE_REGISTRY, &pending).await?;
        if should_set_grace {
            let delay = grace_deadline_ms.saturating_sub(now_ms).saturating_add(ALARM_SAFETY_MS);
            self.state.storage().set_alarm(Duration::from_millis(delay)).await?;
        }
        crate::structured_log!(
            event: "entered_grace_window",
            component: "game_room",
            role: format!("{role:?}"),
            grace_secs: grace_duration.as_secs(),
        );
        Ok(())
    }

    /// `enter_grace_window` の純粋ロジック部分。`CoreRoom` の現状から
    /// `PendingReconnect` を組み立てる (snapshot / Game_Summary / token 取り出し)。
    fn build_pending_reconnect(
        &self,
        core: &CoreRoom,
        cfg: &PersistedConfig,
        role: Role,
        grace_duration: Duration,
    ) -> Result<PendingReconnect> {
        let disconnected_color = role.to_core();
        let token = match disconnected_color {
            Color::Black => cfg.black_reconnect_token.as_deref(),
            Color::White => cfg.white_reconnect_token.as_deref(),
        }
        .ok_or_else(|| {
            Error::RustError(format!(
                "build_pending_reconnect: no reconnect_token issued for {disconnected_color:?}"
            ))
        })?
        .to_owned();
        let position_section = position_section_from_position(core.position());
        let snapshot = ReconnectSnapshot {
            position_section: position_section.clone(),
            black_remaining_ms: core.clock_remaining_main_ms(Color::Black).max(0) as u64,
            white_remaining_ms: core.clock_remaining_main_ms(Color::White).max(0) as u64,
            current_turn: color_to_str(core.current_turn()).to_owned(),
            // CoreRoom は最終手 token を露出していないため、moves テーブルから
            // 再現する経路 (cold start 時) と整合するよう、現時点では None とする。
            // E2E では Reconnect_State の `Last_Move:` 行省略動作を許容する。
            last_move: None,
        };

        // 切断側宛の Game_Summary を「切断時点の現在局面」で再構築する。再接続
        // クライアントは初接続時と同じ `Reconnect_Token:` 拡張行を再受信できる。
        // `cfg.clock` (= 対局開始時に確定した preset 由来 ClockSpec) を使うことで、
        // `CLOCK_PRESETS` 配下では再接続時の `Time:` セクションが対局開始時と一致する。
        let summary = GameSummaryBuilder {
            game_id: GameId::new(cfg.game_id.clone()),
            black: PlayerName::new(cfg.black_handle.clone()),
            white: PlayerName::new(cfg.white_handle.clone()),
            time_section: cfg.clock.format_time_section(),
            position_section,
            rematch_on_draw: false,
            to_move: core.current_turn(),
            entering_king_rule: cfg.entering_king_rule(),
            black_reconnect_token: cfg.black_reconnect_token.as_deref().map(ReconnectToken::new),
            white_reconnect_token: cfg.white_reconnect_token.as_deref().map(ReconnectToken::new),
        };
        let game_summary_for_disconnected = summary.build_for(disconnected_color);

        let now_ms = self.now_ms();
        // `Duration::as_millis()` は `u128` を返すため、`u64::try_from` で
        // 範囲外をサチュレートさせて silent truncation を避ける (実用上の grace は
        // 数十秒〜数時間オーダーで、`u64::MAX` ms の到達は無いが防御的に書く)。
        let grace_ms = u64::try_from(grace_duration.as_millis()).unwrap_or(u64::MAX);
        let deadline_ms = now_ms.saturating_add(grace_ms);
        let disconnected_handle = match disconnected_color {
            Color::Black => cfg.black_handle.clone(),
            Color::White => cfg.white_handle.clone(),
        };
        Ok(PendingReconnect {
            disconnected_handle,
            disconnected_color: color_to_str(disconnected_color).to_owned(),
            expected_token: token,
            deadline_ms,
            snapshot,
            game_summary_for_disconnected,
        })
    }

    /// alarm が `GraceExpired` 種別で発火した経路。registry を読んで切断側を
    /// `force_abnormal` で確定させる。registry が無い (race で削除済み) なら
    /// 何もしない。
    async fn handle_grace_expired_alarm(&self) -> Result<()> {
        let pending: Option<PendingReconnect> =
            self.state.storage().get(KEY_GRACE_REGISTRY).await?;
        let Some(pending) = pending else {
            // 再接続が成立して registry が片付けられた直後の race 等。alarm kind
            // も並行で TimeUp に戻されているはずだが、念のため tag を片付けておく。
            self.delete_grace_alarm_state().await?;
            return Ok(());
        };
        self.ensure_core_loaded().await?;
        // 復元が終局を確定した場合、予約済みの export retry を保持する。
        if self.load_finished().await?.is_some() {
            return Ok(());
        }
        let role = match color_from_str(&pending.disconnected_color) {
            Ok(c) => Role::from_core(c),
            Err(e) => {
                crate::structured_log!(
                    event: "grace_alarm_invalid_color",
                    component: "game_room",
                    err: format!("{e}"),
                );
                self.delete_grace_alarm_state().await?;
                return Ok(());
            }
        };
        let result_opt =
            self.core.borrow_mut().as_mut().map(|core| core.force_abnormal(role.to_core()));
        if let Some(result) = result_opt {
            self.record_verdict(&result).await?;
            self.dispatch_broadcasts(&result.broadcasts).await?;
            // `finalize_if_ended` は内部で `delete_grace_alarm_state` を呼んだ後に
            // ExportRetry alarm を貼り直す可能性がある (R2 export retry)。ここで重ねて
            // `delete_grace_alarm_state` を呼ぶと `KEY_PENDING_ALARM_KIND=ExportRetry`
            // を巻き込んで削除してしまい、retry alarm が発火しても kind が `None`
            // で `KEY_FINISHED` ガードに弾かれる。`finalize_if_ended` の責務に任せ
            // 再 cleanup しない。
            self.finalize_if_ended(&result).await?;
        } else {
            // force_abnormal が呼べなかった (CoreRoom 不在 = cold start 後 replay
            // 失敗等) ケースでは finalize_if_ended が走らないので、grace registry /
            // alarm tag は本経路で明示的に片付ける必要がある。
            self.delete_grace_alarm_state().await?;
        }
        Ok(())
    }

    /// grace registry が無い再接続要求への fallback。DO instance が
    /// `websocket_close` を経ずに破棄されると (deploy 移行やランタイム都合の退避で
    /// 実行中 invocation が cancel される)、`enter_grace_window` が走らず pending
    /// entry が残らない。一方 `PersistedConfig` の reconnect_token は storage に
    /// 残っているため、対局が進行中 (`KEY_FINISHED` 未設定) かつ該当 role の
    /// player socket が現存しなければ、「いま切断された」ものとして現在状態から
    /// `PendingReconnect` を合成し、通常の照合経路 (token 一致検証込み) に乗せる。
    async fn build_cold_rejoin_pending(&self, role: Role) -> Result<Option<PendingReconnect>> {
        if self.load_finished().await?.is_some() {
            return Ok(None);
        }
        let Some(cfg) = self.config.borrow().as_ref().cloned() else {
            return Ok(None);
        };
        // AGREE 未成立 (対局未開始) の部屋は対象外。この段階で合成すると成功時の
        // `delete_grace_alarm_state` が `AgreeTimeout` の alarm kind を消し、
        // `AgreeWaiting` の core では TimeUp 経路が no-op のため部屋が永久残留する。
        // 未開始クライアントは通常 LOGIN でやり直せばよく、放置は AgreeTimeout が
        // 掃除する。
        if cfg.play_started_at_ms.is_none() {
            return Ok(None);
        }
        // grace 設定は対局開始時に `PersistedConfig.reconnect_grace_ms` へ固定
        // されている。現在の `Env` を読み直すと「対局開始後に設定変更 + deploy →
        // DO 再起動」で開始時に token を発行済みの対局を拒否してしまうため、
        // `websocket_close` 経路と同じく永続化済みの値を使う。0 / 未設定 =
        // reconnect 無効な対局なので合成しない。
        let grace = match cfg.reconnect_grace_ms {
            Some(ms) if ms > 0 => Duration::from_millis(ms),
            _ => return Ok(None),
        };
        // 同 role の player socket が生きているなら切断ではない (二重接続要求)。
        for ws in self.state.get_websockets() {
            if let Ok(Some(WsAttachment::Player {
                role: attached_role,
                ..
            })) = ws.deserialize_attachment::<WsAttachment>()
                && attached_role == role
            {
                return Ok(None);
            }
        }
        let core_borrow = self.core.borrow();
        let Some(core) = core_borrow.as_ref() else {
            return Ok(None);
        };
        let pending = self.build_pending_reconnect(core, &cfg, role, grace)?;
        Ok(Some(pending))
    }

    /// 再接続した側が現手番で、残時間が秒読み 1 回分を下回っている場合だけ不足分を
    /// 補償する。同一手番の累積補償も秒読み 1 回分を上限とし、意図的な連続切断で
    /// 思考時間を無制限に増やせないようにする。
    ///
    /// 次の ply に対応する SQL 行へ累積値を書いてから in-memory core に反映する。
    /// 書き込み後に isolate が失われても `ensure_core_loaded` が同じ credit を replay
    /// 後の core へ戻す。着手後も行を残し、将来の replay で過去の補償を再適用する。
    async fn compensate_reconnect_clock_if_needed(&self, role: Role, now_ms: u64) -> Result<u64> {
        let cfg = self.config.borrow().as_ref().cloned().ok_or_else(|| {
            Error::RustError("reconnect clock compensation: config missing".into())
        })?;
        let limit_ms = cfg.clock.reconnect_compensation_limit_ms();
        if limit_ms == 0 {
            return Ok(0);
        }

        let color = role.to_core();
        let plan = {
            let core_borrow = self.core.borrow();
            let Some(core) = core_borrow.as_ref() else {
                return Ok(0);
            };
            if core.current_turn() != color {
                return Ok(0);
            }
            let remaining_before_ms = core.current_turn_remaining_ms(now_ms).unwrap_or(0);
            let granted_ms =
                core.additional_reconnect_credit_ms(now_ms, limit_ms, limit_ms).unwrap_or(0);
            let total_credited_ms =
                core.current_turn_reconnect_credit_ms().saturating_add(granted_ms).min(limit_ms);
            (core.moves_played(), remaining_before_ms, granted_ms, total_credited_ms)
        };
        let (moves_played, remaining_before_ms, granted_ms, total_credited_ms) = plan;
        if granted_ms == 0 {
            return Ok(0);
        }

        let target_ply = i64::from(moves_played).saturating_add(1);
        let total_credited_i64 = i64::try_from(total_credited_ms).map_err(|_| {
            Error::RustError("reconnect clock compensation exceeds SQLite INTEGER".into())
        })?;
        self.state.storage().sql().exec(
            "INSERT INTO reconnect_clock_credits(ply, credited_ms) VALUES (?, ?) \
             ON CONFLICT(ply) DO UPDATE SET credited_ms = excluded.credited_ms",
            vec![target_ply.into(), total_credited_i64.into()],
        )?;

        let remaining_after_ms = {
            let mut core_borrow = self.core.borrow_mut();
            let Some(core) = core_borrow.as_mut() else {
                return Ok(0);
            };
            core.restore_current_turn_reconnect_credit_ms(total_credited_ms, limit_ms);
            core.current_turn_remaining_ms(now_ms).unwrap_or(0)
        };
        crate::structured_log!(
            event: "reconnect_clock_compensated",
            component: "game_room",
            game_id: cfg.game_id.as_str(),
            role: format!("{role:?}"),
            moves_played: moves_played,
            granted_ms: granted_ms,
            total_credited_ms: total_credited_ms,
            remaining_before_ms: remaining_before_ms,
            remaining_after_ms: remaining_after_ms,
        );
        Ok(granted_ms)
    }

    /// LOGIN 行で `reconnect:<game_id>+<token>` が指定されたクライアントを受理し、
    /// grace 中対局へ再参加させる。
    ///
    /// 失敗ケース (`reconnect_unknown_game` / `handle_mismatch` / `color_mismatch` /
    /// `token_mismatch` / `expired`) はすべて `LOGIN:incorrect reconnect_rejected`
    /// で返す (拒否理由を分けると side-channel で「特定 handle / game_id が grace
    /// 中に存在するか」を識別できるため、wire 上は統一)。詳細は console_log の
    /// サーバーログ側にだけ残す。`reconnect_already_resumed` は token 知識を持つ
    /// 正当者の二重接続経路で情報漏洩リスクが無いため原因を分けて返す。
    async fn handle_reconnect_request(
        &self,
        ws: &WebSocket,
        handle: &str,
        role: Role,
        req: ReconnectRequest,
    ) -> Result<()> {
        if self.load_finalizing().await?.is_some() || self.load_finished().await?.is_some() {
            send_line(ws, "LOGIN:incorrect reconnect_rejected")?;
            return Ok(());
        }
        // DO instance が hibernate から起床した直後の再接続でも CoreRoom を
        // ロードできるよう、registry 検索の前に `ensure_core_loaded` を呼ぶ。
        // 成功確定後の `current_game_name_or_empty` / 状態再送はロード済を前提に
        // できるので、この 1 箇所だけで grace 経路全体の cold-start 互換が成立する。
        self.ensure_core_loaded().await?;
        if self.load_finalizing().await?.is_some() || self.load_finished().await?.is_some() {
            send_line(ws, "LOGIN:incorrect reconnect_rejected")?;
            return Ok(());
        }
        let pending: Option<PendingReconnect> =
            self.state.storage().get(KEY_GRACE_REGISTRY).await.ok().flatten();
        let pending = match pending {
            Some(pending) => pending,
            None => match self.build_cold_rejoin_pending(role).await? {
                Some(pending) => {
                    crate::structured_log!(
                        event: "reconnect_cold_rejoin",
                        component: "game_room",
                        game_id: req.game_id.as_str(),
                        role: format!("{role:?}"),
                    );
                    pending
                }
                None => {
                    crate::structured_log!(
                        event: "reconnect_rejected",
                        component: "game_room",
                        reason: "no_pending_entry",
                        game_id: req.game_id.as_str(),
                    );
                    send_line(ws, "LOGIN:incorrect reconnect_rejected")?;
                    return Ok(());
                }
            },
        };

        // game_id 照合は registry 検索 (DO instance = 1 対局専属) で済むため、
        // ここでは LOGIN 経由の game_id が現在対局と一致するかだけ確認する。
        let cfg_game_id = self.config.borrow().as_ref().map(|c| c.game_id.clone());
        if cfg_game_id.as_deref() != Some(req.game_id.as_str()) {
            // DO instance が想定と違う対局に紐づいている (game_id 未一致)。
            crate::structured_log!(
                event: "reconnect_rejected",
                component: "game_room",
                reason: "game_id_mismatch",
                requested_game_id: req.game_id.as_str(),
                current_game_id: format!("{cfg_game_id:?}"),
            );
            send_line(ws, "LOGIN:incorrect reconnect_rejected")?;
            return Ok(());
        }

        let now_ms = self.now_ms();
        let outcome = pending.match_request(handle, role.to_core(), req.token.as_str(), now_ms);
        match outcome {
            ReconnectMatchOutcome::Accepted => {}
            ReconnectMatchOutcome::Rejected => {
                crate::structured_log!(
                    event: "reconnect_rejected",
                    component: "game_room",
                    reason: "handle_color_token_mismatch",
                    handle: handle,
                    role: format!("{role:?}"),
                );
                send_line(ws, "LOGIN:incorrect reconnect_rejected")?;
                return Ok(());
            }
            ReconnectMatchOutcome::Expired => {
                crate::structured_log!(
                    event: "reconnect_rejected",
                    component: "game_room",
                    reason: "grace_expired",
                    deadline_ms: pending.deadline_ms,
                    now_ms: now_ms,
                );
                send_line(ws, "LOGIN:incorrect reconnect_rejected")?;
                return Ok(());
            }
        }

        // token / handle / color / grace の全照合が通った再接続だけを補償対象にする。
        // LOGIN OK より先に永続化し、補償保存失敗時は成功応答を返さない。
        self.compensate_reconnect_clock_if_needed(role, now_ms).await?;

        // 成功確定。LOGIN OK → resume 送出 → attachment を Player に差し替え →
        // grace registry / alarm tag を片付ける順で進める。
        let game_name = self.current_game_name_or_empty().await?;
        let login_name = format!("{handle}+{game_name}+{}", color_to_str(role.to_core()));
        send_line(ws, &LoginReply::Ok { name: login_name }.to_line())?;
        // 状態再送 (Game_Summary + Reconnect_State ブロック) は複数行なので
        // 1 行ずつ `send_line` に分解する。`build_resume_message` の改行終端が
        // 各行末改行と整合するので `lines()` でそのまま reuse できる。
        let resume =
            build_resume_message(&pending.game_summary_for_disconnected, &pending.snapshot);
        for line in resume.lines() {
            send_line(ws, line)?;
        }

        let att = WsAttachment::player(role, handle.to_owned(), game_name);
        ws.serialize_attachment(&att)
            .map_err(|e| Error::RustError(format!("attach player on reconnect: {e}")))?;

        self.delete_grace_alarm_state().await?;
        // 再接続クライアントが指し手を送らず放置しても確実に turn deadline が
        // 発火するよう、即時 alarm を貼り直す。deadline は以前の alarm へ今回の
        // 差分を足すのでなく、固定の手番開始時刻 + 現在予算 + 永続化済み累積
        // credit + 通信マージン + safety から絶対時刻として再構成する。これにより
        // credit 保存と alarm 更新の間で isolate が失われても、次の cold rejoin が
        // 復元済み credit を反映できる。off-turn は credit を付与しないため延長なし。
        let now = self.now_ms();
        let turn_alarm_epoch_ms = {
            let core_borrow = self.core.borrow();
            core_borrow.as_ref().and_then(|core| {
                let margin = self
                    .config
                    .borrow()
                    .as_ref()
                    .map(|c| c.time_margin_ms)
                    .unwrap_or(DEFAULT_TIME_MARGIN_MS);
                core.current_turn_deadline_ms()
                    .map(|deadline| deadline.saturating_add(margin).saturating_add(ALARM_SAFETY_MS))
            })
        };
        if let Some(final_epoch) = turn_alarm_epoch_ms {
            // `set_alarm(Duration)` は「now から N ms 後」に発火する API なので
            // delay = final_epoch - now で渡す。`saturating_sub` は now が final_epoch
            // を既に過ぎている場合に 0 を返し、即時発火させて time_up に進める。
            let delay_ms = final_epoch.saturating_sub(now);
            self.state.storage().set_alarm(Duration::from_millis(delay_ms)).await?;
        } else {
            // CoreRoom 不在 (異常系)。alarm を解除して保守的に振る舞う。
            let _ = self.state.storage().delete_alarm().await;
        }
        crate::structured_log!(
            event: "reconnect_succeeded",
            component: "game_room",
            handle: handle,
            role: format!("{role:?}"),
        );
        Ok(())
    }

    /// 現在 DO の対局 game_name (`PersistedConfig.game_name`)。LOGIN OK 応答で
    /// `<handle>+<game_name>+<color>` 形式を再構築するために使う。config 未設定
    /// なら空文字を返す (handshake は LOGIN OK 後に拒否されている経路では呼ば
    /// れないため、空文字到達は契約違反として handle される想定)。
    async fn current_game_name_or_empty(&self) -> Result<String> {
        if let Some(cfg) = self.config.borrow().as_ref() {
            return Ok(cfg.game_name.clone());
        }
        let cfg_opt: Option<PersistedConfig> = self.state.storage().get(KEY_CONFIG).await?;
        Ok(cfg_opt.map(|c| c.game_name).unwrap_or_default())
    }

    async fn load_pending_alarm_kind(&self) -> Result<Option<PendingAlarmKind>> {
        self.state.storage().get(KEY_PENDING_ALARM_KIND).await
    }

    async fn delete_grace_alarm_state(&self) -> Result<()> {
        let _ = self
            .state
            .storage()
            .delete_multiple(vec![KEY_GRACE_REGISTRY, KEY_PENDING_ALARM_KIND])
            .await;
        Ok(())
    }

    /// AGREE 待ち TTL タグだけを片付ける best-effort helper (https://github.com/SH11235/rshogi/issues/600)。
    /// `HandleOutcome::GameStarted` 観測時に呼ぶ。`KEY_GRACE_REGISTRY` は
    /// AGREE 経路では存在しない (grace 経路は対局成立後にしか入らない) ため
    /// 触らない。
    async fn clear_agree_timeout_tag(&self) {
        let _ = self.state.storage().delete(KEY_PENDING_ALARM_KIND).await;
    }

    /// alarm が `AgreeTimeout` 種別で発火した経路 (https://github.com/SH11235/rshogi/issues/600)。
    ///
    /// - 既に終局済 / 対局成立済の場合は no-op (race ガード)。
    /// - そうでなければ両 player WS に `##[ERROR] agree_timeout` を送って close
    ///   し、slot を解放、`KEY_FINISHED` で再 LOGIN を弾く。`play_started_at_ms`
    ///   が None のまま到達するため live-games-index は put 前で cleanup 不要、
    ///   R2 棋譜エクスポートも skip する (`finalize_if_ended` を呼ばない)。
    /// - `core` が in-memory に残っていれば落とす (= 後続の `ensure_core_loaded`
    ///   は `KEY_FINISHED` ガードで早期 return する)。
    async fn handle_agree_timeout_alarm(&self) -> Result<()> {
        // 終局済 (既に何らかの経路で確定) ならタグだけ片付けて終了。
        if self.load_finished().await?.is_some() {
            let _ = self.state.storage().delete(KEY_PENDING_ALARM_KIND).await;
            return Ok(());
        }
        // race: alarm 直前で AGREE が成立していて `mark_play_started` 済なら
        // 何もしない。タグも `clear_agree_timeout_tag` 経路で消える (race で
        // 残っているなら念のため明示削除)。
        let cfg_opt: Option<PersistedConfig> = self.state.storage().get(KEY_CONFIG).await?;
        if let Some(cfg) = cfg_opt.as_ref() {
            if cfg.play_started_at_ms.is_some() {
                let _ = self.state.storage().delete(KEY_PENDING_ALARM_KIND).await;
                return Ok(());
            }
        }

        crate::structured_log!(
            event: "agree_timeout_fired",
            component: "game_room",
        );
        // `abort_pending_match_with_error` と同じ pending 解放ロジックを再利用
        // して、両 player に `##[ERROR] agree_timeout` を返した上で slot を空に
        // 戻す。
        self.abort_pending_match_with_error("##[ERROR] agree_timeout").await?;
        // 後続の LOGIN を弾くため `KEY_FINISHED` を立てる。`result_code` は
        // 終局集計には乗らない (R2 棋譜・games-index も put しない) が、
        // `load_finished` ガードを駆動する目的で観測可能な値を埋めておく。
        let finished = FinishedState {
            result_code: "agree_timeout".to_owned(),
            ended_at_ms: self.now_ms(),
            // R2 棋譜 export を行わない経路なので `exported_at_ms` を立てる
            // 必要はない。`Some(now_ms)` にすると「export 完了」と誤って観測
            // されるため、`None` のまま (= retry 待ちでもなく export 不要の
            // 終了ステータス) を意味する値にしておく。
            exported_at_ms: None,
        };
        self.state.storage().put(KEY_FINISHED, &finished).await?;
        // alarm 種別タグを片付ける (alarm は既に発火済なので `delete_alarm` は
        // 不要だが、防御的に明示削除しても副作用はない)。
        let _ = self.state.storage().delete(KEY_PENDING_ALARM_KIND).await;
        // 万一 `core` が残っていれば落とす (`ensure_core_loaded` は finished
        // ガードで以後何もロードしない)。
        self.core.borrow_mut().take();
        Ok(())
    }
}

/// LOGIN handle 自称防止。`WORKERS_HANDLE_AUTH` whitelist の
/// 1 LOGIN あたり 1 回 fetch + parse して、登録 handle に限り SHA256(password)
/// を要求する。
///
/// 戻り値:
/// - `Ok(false)` → LOGIN を allow (whitelist 未宣言 / handle 非登録 / password 一致)。
/// - `Ok(true)` → LOGIN を deny。`LOGIN:incorrect handle_auth_failed` 送出済み、
///   呼び出し側は 1003 で WS を close する責務。
///
/// fail-closed 規約:
/// - registry parse 失敗 (JSON 不正 / 重複 / hash 形式不正) → deny。
///   `whitelist が設定済か否か` を leak しないよう、verify mismatch と同じ
///   `handle_auth_failed` reason で uniform に拒否する。
/// - registry parse 成功 + `requires_auth == false` → allow (self-claim 既定挙動)。
/// - registry parse 成功 + `verify` mismatch → deny。
fn enforce_handle_auth(ws: &WebSocket, env: &Env, handle: &str, password: &str) -> Result<bool> {
    let registry = match load_handle_auth_registry(env) {
        Ok(r) => r,
        Err(e) => {
            crate::structured_log!(
                event: "handle_auth_registry_invalid",
                component: "game_room",
                err: format!("{e}"),
            );
            send_line(ws, "LOGIN:incorrect handle_auth_failed")?;
            return Ok(true);
        }
    };
    if !registry.requires_auth(handle) {
        return Ok(false);
    }
    match registry.verify(handle, password) {
        Ok(()) => Ok(false),
        Err(HandleAuthError::PasswordMismatch) => {
            crate::structured_log!(
                event: "handle_auth_failed",
                component: "game_room",
                handle: handle,
                reason: "password_mismatch",
            );
            send_line(ws, "LOGIN:incorrect handle_auth_failed")?;
            Ok(true)
        }
        Err(e) => {
            // `NotConfigured` 等の本来到達しない経路に出ても、fail-closed で
            // uniform に reject する (silently allow しない)。
            crate::structured_log!(
                event: "handle_auth_failed",
                component: "game_room",
                handle: handle,
                reason: "unexpected_verify_error",
                err: format!("{e}"),
            );
            send_line(ws, "LOGIN:incorrect handle_auth_failed")?;
            Ok(true)
        }
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
    let total_time_ms = env.var(ConfigKeys::TOTAL_TIME_MS).ok().map(|v| v.to_string());
    let byoyomi_ms = env.var(ConfigKeys::BYOYOMI_MS).ok().map(|v| v.to_string());
    let total_time_min = env.var(ConfigKeys::TOTAL_TIME_MIN).ok().map(|v| v.to_string());
    let byoyomi_min = env.var(ConfigKeys::BYOYOMI_MIN).ok().map(|v| v.to_string());
    parse_clock_spec(
        clock_kind.as_deref(),
        total_time_sec.as_deref(),
        byoyomi_sec.as_deref(),
        total_time_ms.as_deref(),
        byoyomi_ms.as_deref(),
        total_time_min.as_deref(),
        byoyomi_min.as_deref(),
    )
    .map_err(Error::RustError)
}

/// `game_name` 別の時計プリセットを解決する。
///
/// - `CLOCK_PRESETS` が宣言済み (= 1 件以上 entry を持つ) で `game_name` がヒット
///   → 該当 `ClockSpec` を返す。
/// - `CLOCK_PRESETS` が未宣言 / 空配列
///   → `load_clock_spec_from_env` で global clock を返す（後方互換）。
/// - `CLOCK_PRESETS` 宣言済みかつ `game_name` 未登録
///   → `Err`（strict mode）。Lobby 側 (`handle_login_lobby`) は同じプリセット表で
///   事前に弾いており Err はクライアントに届かない（DO ログにのみ出る）。本経路に
///   到達するのは Lobby を経由しない単体テスト経路、もしくはプリセット書き換え直後の
///   race など限定的なケース。
///
/// **キャッシュなし設計** (https://github.com/SH11235/rshogi/issues/641): Lobby DO もキャッシュを廃止して毎 LOGIN
/// 時に env を読み直す方針に揃えたため、本関数は両 DO 共通の挙動になっている。
/// Cloudflare DO は hibernation から起床しても `OnceCell` キャッシュが更新されず、
/// deploy で `CLOCK_PRESETS` を変更しても旧 instance が古い値を保持する race が
/// 発生していたため、両 DO とも env を毎回読み直す方針に統一した。GameRoom DO は
/// 1 対局 = 1 インスタンスで `start_match` のたった 1 回しか評価しないため
/// もともと re-parse コストは問題にならない。
fn resolve_game_preset_for_game(env: &Env, game_name: &str) -> Result<crate::config::GamePreset> {
    use crate::config::{GamePreset, PresetResolution, resolve_game_preset_from_presets_map};
    let raw = env.var(ConfigKeys::CLOCK_PRESETS).ok().map(|v| v.to_string());
    let presets = parse_clock_presets(raw.as_deref()).map_err(Error::RustError)?;
    match resolve_game_preset_from_presets_map(&presets, game_name) {
        PresetResolution::Fallback => Ok(GamePreset {
            clock: load_clock_spec_from_env(env)?,
            max_moves: None,
        }),
        PresetResolution::Hit(preset) => Ok(preset),
        PresetResolution::Unknown => Err(Error::RustError(format!(
            "CLOCK_PRESETS: unknown game_name {game_name:?}; configure preset or remove strict mode"
        ))),
    }
}

/// `RECONNECT_GRACE_SECONDS` env を読み、Floodgate features の opt-in
/// (`ALLOW_FLOODGATE_FEATURES`) と整合しているか検証して `Duration` を返す。
///
/// ```text
/// grace=0 (or unset)  : OK (Floodgate gate を通さず保守的既定)
/// grace>0 + allow=true: OK (再接続プロトコル有効)
/// grace>0 + allow=false: Err (Floodgate features の opt-in 漏れ)
/// ```
///
/// 設定不正は `Err(String)` で返し、呼び出し側は安全側に grace を無効化する経路に
/// 落とす (websocket_close で旧 force_abnormal 経路にフォールバック)。
///
/// 実体ロジックは [`resolve_reconnect_grace_from_strings`] (host 単体テスト可能な
/// pure helper) に委譲し、本関数は `worker::Env` 依存を切り出す薄い shim に閉じる。
/// `AGREE_TIMEOUT_SECONDS` env を読み、AGREE 待ち TTL を `Duration` で返す
/// (https://github.com/SH11235/rshogi/issues/600)。値の解釈は [`parse_agree_timeout_duration`] に従う:
/// `None` / 空文字 / `0` / 非数値は [`crate::config::DEFAULT_AGREE_TIMEOUT_SEC`]
/// にフォールバックする (env 不正で stuck DO が無限残存する事態を避ける)。
fn resolve_agree_timeout(env: &Env) -> Duration {
    let raw = env.var(ConfigKeys::AGREE_TIMEOUT_SECONDS).ok().map(|v| v.to_string());
    parse_agree_timeout_duration(raw.as_deref())
}

fn resolve_reconnect_grace(env: &Env) -> std::result::Result<Duration, String> {
    let grace_raw = env.var(ConfigKeys::RECONNECT_GRACE_SECONDS).ok().map(|v| v.to_string());
    let allow_raw = env.var(ConfigKeys::ALLOW_FLOODGATE_FEATURES).ok().map(|v| v.to_string());
    resolve_reconnect_grace_from_strings(grace_raw.as_deref(), allow_raw.as_deref())
}

/// `ENTERING_KING_RULE` env を読み、`%KACHI` 判定ルールを解決する。
///
/// 不正トークンは grace と違って対局を abort せず、ログを出して
/// [`crate::config::DEFAULT_ENTERING_KING_RULE`] にフォールバックする (1 台の typo で
/// 全対局を落とさず、標準ルールで走らせて運用者に気付かせる)。
fn resolve_entering_king_rule(env: &Env) -> rshogi_core::types::EnteringKingRule {
    let raw = env.var(ConfigKeys::ENTERING_KING_RULE).ok().map(|v| v.to_string());
    match resolve_entering_king_rule_from_string(raw.as_deref(), DEFAULT_ENTERING_KING_RULE) {
        Ok(rule) => rule,
        Err(token) => {
            crate::structured_log!(
                event: "entering_king_rule_config_error",
                component: "game_room",
                token: token,
            );
            DEFAULT_ENTERING_KING_RULE
        }
    }
}

/// `ALLOW_FLOODGATE_FEATURES` env と `FLOODGATE_HISTORY_BUCKET` binding を読み、
/// 履歴永続化用の `R2FloodgateHistoryStorage` を返す。opt-in されていない、または
/// dev 環境で binding が宣言されていない場合は `Ok(None)` で skip する。
///
/// `validate_floodgate_feature_gate` の `enable_floodgate_history` ブランチに
/// 対応するため、master switch (`allow`) が立っている前提で intent を組み、
/// 設定不正は `Err(String)` で呼び出し側 (`finalize_if_ended`) のログ経路に流す。
fn resolve_floodgate_history_storage(
    env: &Env,
) -> std::result::Result<Option<R2FloodgateHistoryStorage>, String> {
    let allow_raw = env.var(ConfigKeys::ALLOW_FLOODGATE_FEATURES).ok().map(|v| v.to_string());
    let allow = parse_allow_floodgate_features(allow_raw.as_deref())?;
    if !allow {
        return Ok(None);
    }
    let intent = FloodgateFeatureIntent {
        enable_floodgate_history: true,
        ..FloodgateFeatureIntent::default()
    };
    validate_floodgate_feature_gate(allow, intent)?;

    if env.bucket(ConfigKeys::FLOODGATE_HISTORY_BUCKET_BINDING).is_err() {
        return Ok(None);
    }
    Ok(Some(R2FloodgateHistoryStorage::new(
        env.clone(),
        ConfigKeys::FLOODGATE_HISTORY_BUCKET_BINDING,
    )))
}
