//! CSA対局セッション管理
//!
//! 1回の対局（ログイン後〜対局〜終局）を管理する。
//! サーバー受信スレッドとエンジン受信を共通チャネル経由で同時監視し、
//! ponderhit 中にサーバーから終局通知が来ても即座に検出できる。
//!
//! # 公開 API
//!
//! - [`run_game_session_with_events`] / [`run_resumed_session_with_events`]:
//!   [`SessionEventSink`] に対局途中の進捗 ([`SessionProgress`]) を push 通知する
//!   primary API。
//! - [`run_game_session`] / [`run_resumed_session`]: 進捗通知不要な consumer 向けの
//!   薄いラッパー (内部で [`NoopSessionEventSink`] を渡す)。
//!
//! sink 仕様 / resume contract / terminate 手順の詳細は [`crate::events`] モジュール
//! の crate-level doc を参照。

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::Result;

use rshogi_csa::{Color, Position, csa_move_to_usi, usi_move_to_csa};

use crate::config::CsaClientConfig;
use crate::engine::{BestMoveResult, SearchInfo, SearchOutcome, UsiEngine};
use crate::event::Event;
use crate::events::{
    BestMoveEvent, DisconnectReason, GameEndEvent, GameEndReason, MoveEvent, MovePlayer,
    NoopSessionEventSink, ReconnectState as PublicReconnectState, SearchInfoEmitPolicy,
    SearchInfoSnapshot, SearchOrigin, SessionError, SessionEventSink, SessionOutcome,
    SessionProgress, Side, SinkError,
};
use crate::protocol::{
    CsaConnection, GameResult, GameSummary, ReconnectState as ProtocolReconnectState,
    parse_game_result, parse_server_move,
};
use crate::record::{GameRecord, JsonlMoveExtra};

// ────────────────────────────────────────────
// 公開エントリポイント
// ────────────────────────────────────────────

/// `SessionEventSink` 経由で進捗通知付き対局を駆動する primary API。
///
/// `conn` はあらかじめ [`CsaConnection::login`] 済みのものを渡すこと。本関数は
/// `conn` 上で `Game_Summary` 受信 → AGREE → 対局ループを実行し、最終的に
/// `LOGOUT` を試みる。`conn` の所有権は呼び出し側に残るので、戻り後に
/// `drop(conn)` か追加 close 処理を呼び出し側が決められる。
///
/// resume 経路は [`run_resumed_session_with_events`] を使うこと。
pub fn run_game_session_with_events<S>(
    config: &CsaClientConfig,
    conn: &mut CsaConnection,
    engine: &mut UsiEngine,
    shutdown: Arc<AtomicBool>,
    sink: &mut S,
) -> Result<SessionOutcome, SessionError>
where
    S: SessionEventSink + ?Sized,
{
    drive_session(config, conn, engine, shutdown.as_ref(), sink, SessionMode::Fresh)
}

/// `LOGIN ... reconnect:<game_id>+<token>` 後の resume セッションを進捗通知付きで
/// 駆動する。`conn` は [`CsaConnection::login_reconnect`] 成功直後のものを渡すこと。
///
/// 本関数は履歴 replay を `MoveConfirmed` として emit しない (resume 時の局面は
/// `Resumed.state.last_sfen` から再構築する)。詳細は [`crate::events`] の
/// crate-level doc を参照。
pub fn run_resumed_session_with_events<S>(
    config: &CsaClientConfig,
    conn: &mut CsaConnection,
    engine: &mut UsiEngine,
    shutdown: Arc<AtomicBool>,
    sink: &mut S,
) -> Result<SessionOutcome, SessionError>
where
    S: SessionEventSink + ?Sized,
{
    drive_session(config, conn, engine, shutdown.as_ref(), sink, SessionMode::Resumed)
}

/// 進捗通知不要な consumer 向けの薄いラッパー。内部で [`NoopSessionEventSink`] を渡す。
/// `shutdown` の動的変更を新 API 同等に伝播させるため、内部実装 `drive_session`
/// を直接呼ぶ。
pub fn run_game_session(
    conn: &mut CsaConnection,
    engine: &mut UsiEngine,
    config: &CsaClientConfig,
    shutdown: &AtomicBool,
) -> Result<SessionOutcome, SessionError> {
    let mut sink = NoopSessionEventSink;
    drive_session(config, conn, engine, shutdown, &mut sink, SessionMode::Fresh)
}

/// 進捗通知不要な consumer 向けの薄いラッパー (resume 経路)。
pub fn run_resumed_session(
    conn: &mut CsaConnection,
    engine: &mut UsiEngine,
    config: &CsaClientConfig,
    shutdown: &AtomicBool,
) -> Result<SessionOutcome, SessionError> {
    let mut sink = NoopSessionEventSink;
    drive_session(config, conn, engine, shutdown, &mut sink, SessionMode::Resumed)
}

// ────────────────────────────────────────────
// 内部実装: drive_session 共通経路
// ────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionMode {
    Fresh,
    Resumed,
}

fn drive_session<S>(
    config: &CsaClientConfig,
    conn: &mut CsaConnection,
    engine: &mut UsiEngine,
    shutdown: &AtomicBool,
    sink: &mut S,
    mode: SessionMode,
) -> Result<SessionOutcome, SessionError>
where
    S: SessionEventSink + ?Sized,
{
    // Step 1: Connected
    if let Some(action) = emit_with_nonfatal_warn(sink, SessionProgress::Connected) {
        return handle_sink_error(action, conn, sink, None, false);
    }
    if !sink.should_continue() {
        return abort_for_should_continue(conn, sink, None, false);
    }

    // Step 2: Game_Summary 受信
    let summary = match conn.recv_game_summary(config.server.keepalive.ping_interval_sec) {
        Ok(s) => s,
        Err(err) => {
            return Err(map_anyhow_to_session_error(err));
        }
    };
    let summary_arc = Arc::new(summary.clone());

    // resume の場合は Reconnect_State も受信する
    let reconnect_state_protocol = if mode == SessionMode::Resumed {
        match conn.recv_reconnect_state() {
            Ok(state) => Some(state),
            Err(err) => {
                return Err(map_anyhow_to_session_error(err));
            }
        }
    } else {
        None
    };

    // Step 3: Resumed / GameSummary を発火
    let progress = match mode {
        SessionMode::Fresh => SessionProgress::GameSummary(Arc::clone(&summary_arc)),
        SessionMode::Resumed => {
            let state = build_reconnect_state(&summary, reconnect_state_protocol.as_ref());
            SessionProgress::Resumed {
                summary: Arc::clone(&summary_arc),
                state,
            }
        }
    };
    if let Some(action) = emit_with_nonfatal_warn(sink, progress) {
        return handle_sink_error(action, conn, sink, Some(summary), false);
    }
    if !sink.should_continue() {
        return abort_for_should_continue(conn, sink, Some(summary), false);
    }

    // Step 4: AGREE / engine.new_game (Fresh のみ AGREE 必要)
    if mode == SessionMode::Fresh
        && let Err(err) = conn.agree_and_wait_start(&summary.game_id)
    {
        return Err(map_anyhow_to_session_error(err));
    }
    if let Err(err) = engine.new_game() {
        return Err(SessionError::Engine(format!("{err}")));
    }

    // Step 5: GameStarted を発火
    if let Some(action) = emit_with_nonfatal_warn(sink, SessionProgress::GameStarted) {
        return handle_sink_error(action, conn, sink, Some(summary), false);
    }
    if !sink.should_continue() {
        return abort_for_should_continue(conn, sink, Some(summary), false);
    }

    // Step 6: 対局メインループ
    let loop_result = run_session_loop(
        conn,
        engine,
        config,
        shutdown,
        summary.clone(),
        reconnect_state_protocol,
        sink,
    );

    match loop_result {
        LoopOutcome::GameEnded {
            result,
            record,
            game_end_event,
        } => {
            // GameEnded 発火
            if let Some(action) =
                emit_with_nonfatal_warn(sink, SessionProgress::GameEnded(game_end_event))
            {
                // 既に gameover は emit 済みなので CHUDAN は不要だが LOGOUT は試みる。
                return handle_sink_error(action, conn, sink, Some(summary), true);
            }
            // 通常切断
            let _ = conn.logout();
            let _ = sink.on_event(SessionProgress::Disconnected {
                reason: DisconnectReason::GameOver,
            });
            Ok(SessionOutcome {
                result,
                record: *record,
                summary: Some(summary),
            })
        }
        LoopOutcome::SinkAborted {
            sink_err,
            game_already_ended,
        } => handle_sink_error(sink_err, conn, sink, Some(summary), game_already_ended),
        LoopOutcome::Shutdown { game_already_ended } => {
            terminate_session(
                conn,
                sink,
                &SessionError::Shutdown,
                DisconnectReason::Shutdown,
                game_already_ended,
            );
            Err(SessionError::Shutdown)
        }
        LoopOutcome::Error(err) => Err(err),
    }
}

// ────────────────────────────────────────────
// LoopOutcome
// ────────────────────────────────────────────

enum LoopOutcome {
    GameEnded {
        result: GameResult,
        record: Box<GameRecord>,
        game_end_event: GameEndEvent,
    },
    SinkAborted {
        sink_err: SinkError,
        game_already_ended: bool,
    },
    Shutdown {
        game_already_ended: bool,
    },
    Error(SessionError),
}

// ────────────────────────────────────────────
// 内部状態
// ────────────────────────────────────────────

/// メインループの可変状態。ライフタイム `'a` は 1 局分の借用。
/// `S` は generic な sink。`?Sized` を許して `dyn SessionEventSink` も渡せるようにする。
struct SessionState<'a, S: ?Sized + 'a> {
    conn: &'a mut CsaConnection,
    engine: &'a mut UsiEngine,
    config: &'a CsaClientConfig,
    shutdown: &'a AtomicBool,
    server_rx: &'a Receiver<Event>,
    pos: Position,
    usi_moves: Vec<String>,
    clock: Clock,
    record: GameRecord,
    ponder_state: Option<PonderState>,
    my_color: Color,
    initial_sfen: String,
    sink: &'a mut S,
    info_throttle: SearchInfoThrottle,
    /// 直前に ponder miss が発生したか。次の自手番の fresh search に
    /// `SearchOrigin::PonderMiss` を載せるためのフラグ。
    pending_ponder_miss: bool,
}

/// 探索結果の処理結果
enum MoveAction {
    Continue,
    GameEnd(GameResult, Box<GameRecord>, GameEndEvent),
    SinkAborted(SinkError, bool /* game_already_ended */),
    Shutdown(bool /* game_already_ended */),
}

// ────────────────────────────────────────────
// メインループ
// ────────────────────────────────────────────

fn run_session_loop<S>(
    conn: &mut CsaConnection,
    engine: &mut UsiEngine,
    config: &CsaClientConfig,
    shutdown: &AtomicBool,
    summary: GameSummary,
    resume_state: Option<ProtocolReconnectState>,
    sink: &mut S,
) -> LoopOutcome
where
    S: SessionEventSink + ?Sized,
{
    let (server_tx, server_rx) = mpsc::channel();
    if let Err(err) = conn.start_reader_thread(server_tx) {
        return LoopOutcome::Error(map_anyhow_to_session_error(err));
    }

    let mut clock = Clock::from_summary(&summary);
    if let Some(state) = &resume_state {
        clock.black_time_ms = state.black_remaining_ms.max(0);
        clock.white_time_ms = state.white_remaining_ms.max(0);
    }

    let mut s = SessionState {
        pos: summary.position.clone(),
        initial_sfen: summary.position.to_sfen(),
        usi_moves: Vec::new(),
        clock,
        record: GameRecord::new(&summary),
        ponder_state: None,
        my_color: summary.my_color,
        conn,
        engine,
        config,
        shutdown,
        server_rx: &server_rx,
        sink,
        info_throttle: SearchInfoThrottle::new(config.game.search_info_emit.clone()),
        pending_ponder_miss: false,
    };

    // 途中局面の手順を適用 (Fresh で `initial_moves` がある時のみ。resume では
    // `initial_moves` は通常空。Fresh の時に届く initial_moves は CSA Game_Summary の
    // 一部であり、この時点では既に局面に焼き込まれている扱いとして MoveConfirmed は
    // emit しない。ただし record / clock / usi_moves には反映する必要がある)。
    let mut move_color = summary.position.side_to_move;
    for cm in &summary.initial_moves {
        let usi = match csa_move_to_usi(&cm.mv, &s.pos) {
            Ok(u) => u,
            Err(err) => return LoopOutcome::Error(map_anyhow_to_session_error(err)),
        };
        let initial_sfen_before = s.pos.to_sfen();
        if let Err(err) = s.pos.apply_csa_move(&cm.mv) {
            return LoopOutcome::Error(map_anyhow_to_session_error(err));
        }
        s.usi_moves.push(usi.clone());
        if let Some(t) = cm.time_sec {
            s.clock.consume(move_color, t);
        }
        let time_sec = cm.time_sec.unwrap_or(0);
        s.record.add_move(&cm.mv, time_sec, None, move_color);
        push_opponent_jsonl(&mut s.record, initial_sfen_before, usi, move_color, time_sec);
        move_color = opposite(move_color);
    }

    loop {
        // 各イテレーション先頭で sink.should_continue() を確認
        if !s.sink.should_continue() {
            let synthetic_err = SinkError::Fatal(Box::new(std::io::Error::other(
                "sink.should_continue() == false",
            )));
            return LoopOutcome::SinkAborted {
                sink_err: synthetic_err,
                game_already_ended: false,
            };
        }
        if s.shutdown.load(Ordering::SeqCst) {
            return LoopOutcome::Shutdown {
                game_already_ended: false,
            };
        }

        if s.pos.side_to_move == s.my_color {
            let turn_start = Instant::now();
            let sfen_before = s.pos.to_sfen();
            let think_limit_ms = s.clock.think_limit_ms(s.config.time.margin_msec, s.my_color);
            let position_cmd = build_position_cmd(&s.initial_sfen, &s.usi_moves);
            let go_cmd =
                format!("go {}", s.clock.build_go_args(s.config.time.margin_msec, s.my_color));

            let outcome = {
                let mut emitter = SearchInfoEmitter::new(&mut s.info_throttle, s.sink);
                let mut info_callback = |info: &SearchInfo, raw: &str| {
                    emitter.observe(info, raw);
                };
                let result = s.engine.go_with_info(
                    &position_cmd,
                    &go_cmd,
                    s.shutdown,
                    s.server_rx,
                    &mut info_callback,
                );
                let final_observation = emitter.into_final();
                (result, final_observation)
            };
            let (search_outcome_result, final_info) = outcome;
            let search_outcome = match search_outcome_result {
                Ok(o) => o,
                Err(err) => return LoopOutcome::Error(map_anyhow_to_session_error(err)),
            };
            // bestmove 直前の最終 info を `emit_final` ポリシーに従って発火
            if let Some(snapshot) = final_info
                && let Err(err) = s.sink.on_event(SessionProgress::SearchInfo(snapshot))
                && let Some(outcome) = handle_loop_sink_err(err, false)
            {
                return outcome;
            }

            // 直前に ponder miss があれば、その次の fresh search は `PonderMiss` で
            // emit する (UI が「ponder が外れて生まれた fresh search」と区別できるよう)。
            // それ以外は通常の `Fresh`。
            let origin = if s.pending_ponder_miss {
                s.pending_ponder_miss = false;
                SearchOrigin::PonderMiss
            } else {
                SearchOrigin::Fresh
            };
            match handle_search_outcome_with_origin(
                &mut s,
                search_outcome,
                turn_start,
                sfen_before,
                think_limit_ms,
                origin,
            ) {
                MoveAction::Continue => {}
                MoveAction::GameEnd(result, record_box, game_end_event) => {
                    return LoopOutcome::GameEnded {
                        result,
                        record: record_box,
                        game_end_event,
                    };
                }
                MoveAction::SinkAborted(sink_err, game_already_ended) => {
                    return LoopOutcome::SinkAborted {
                        sink_err,
                        game_already_ended,
                    };
                }
                MoveAction::Shutdown(game_already_ended) => {
                    return LoopOutcome::Shutdown { game_already_ended };
                }
            }
        }

        // 相手の手番: server_rx から指し手を待つ
        loop {
            if !s.sink.should_continue() {
                let synthetic_err = SinkError::Fatal(Box::new(std::io::Error::other(
                    "sink.should_continue() == false",
                )));
                return LoopOutcome::SinkAborted {
                    sink_err: synthetic_err,
                    game_already_ended: false,
                };
            }
            match s.server_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Event::ServerLine(line)) => {
                    if line.starts_with('+') || line.starts_with('-') {
                        match handle_opponent_move_line(&mut s, &line) {
                            MoveAction::Continue => break,
                            MoveAction::GameEnd(result, record_box, game_end_event) => {
                                return LoopOutcome::GameEnded {
                                    result,
                                    record: record_box,
                                    game_end_event,
                                };
                            }
                            MoveAction::SinkAborted(sink_err, game_already_ended) => {
                                return LoopOutcome::SinkAborted {
                                    sink_err,
                                    game_already_ended,
                                };
                            }
                            MoveAction::Shutdown(game_already_ended) => {
                                return LoopOutcome::Shutdown { game_already_ended };
                            }
                        }
                    }
                    if line.starts_with('#') {
                        if let Some(game_result) = parse_game_result(&line) {
                            log::info!("[CSA] 対局終了: {line}");
                            if let Err(err) = cleanup_ponder(s.engine, &mut s.ponder_state) {
                                return LoopOutcome::Error(map_anyhow_to_session_error(err));
                            }
                            let reason_line = s.conn.pending_end_reason.take();
                            s.record
                                .set_result(record_result_with_reason(&game_result, &reason_line));
                            if let Err(err) = s.engine.gameover(gameover_str(&game_result)) {
                                return LoopOutcome::Error(map_anyhow_to_session_error(err));
                            }
                            let game_end_event = build_game_end_event(
                                &game_result,
                                reason_line,
                                Some(line),
                                s.my_color,
                            );
                            return LoopOutcome::GameEnded {
                                result: game_result,
                                record: Box::new(s.record.clone()),
                                game_end_event,
                            };
                        }
                        s.conn.pending_end_reason = Some(line);
                    }
                }
                Ok(Event::ServerDisconnected) => {
                    if let Err(err) = cleanup_ponder(s.engine, &mut s.ponder_state) {
                        return LoopOutcome::Error(map_anyhow_to_session_error(err));
                    }
                    if let Err(err) = s.engine.gameover("lose") {
                        return LoopOutcome::Error(map_anyhow_to_session_error(err));
                    }
                    let game_end_event = build_game_end_event(
                        &GameResult::Interrupted,
                        Some("#DISCONNECTED".to_owned()),
                        None,
                        s.my_color,
                    );
                    return LoopOutcome::GameEnded {
                        result: GameResult::Interrupted,
                        record: Box::new(s.record.clone()),
                        game_end_event,
                    };
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Err(err) =
                        s.conn.maybe_send_keepalive(s.config.server.keepalive.ping_interval_sec)
                    {
                        return LoopOutcome::Error(map_anyhow_to_session_error(err));
                    }
                    if s.shutdown.load(Ordering::SeqCst) {
                        return LoopOutcome::Shutdown {
                            game_already_ended: false,
                        };
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return LoopOutcome::Error(SessionError::Protocol(
                        "サーバー受信チャネル切断".to_owned(),
                    ));
                }
            }
        }
    }
}

/// 自分の bestmove 後、SearchOrigin を意識してハンドリングする。
fn handle_search_outcome_with_origin<S>(
    s: &mut SessionState<'_, S>,
    outcome: SearchOutcome,
    turn_start: Instant,
    sfen_before: String,
    think_limit_ms: u64,
    origin: SearchOrigin,
) -> MoveAction
where
    S: SessionEventSink + ?Sized,
{
    match outcome {
        SearchOutcome::BestMove(result, info) => send_bestmove_and_wait_echo(
            s,
            &result,
            &info,
            turn_start,
            sfen_before,
            think_limit_ms,
            origin,
        ),
        SearchOutcome::ServerInterrupt(lines) => {
            let (game_result, reason_line, raw_result_line) =
                parse_server_interrupt_lines_full(lines);
            log::info!("[CSA] サーバー終局割り込み: {:?}", game_result);
            s.record.set_result(record_result_with_reason(&game_result, &reason_line));
            if let Err(err) = s.engine.gameover(gameover_str(&game_result)) {
                return MoveAction::sink_or_error(err);
            }
            let game_end_event =
                build_game_end_event(&game_result, reason_line, raw_result_line, s.my_color);
            MoveAction::GameEnd(game_result, Box::new(s.record.clone()), game_end_event)
        }
    }
}

fn send_bestmove_and_wait_echo<S>(
    s: &mut SessionState<'_, S>,
    result: &BestMoveResult,
    info: &SearchInfo,
    turn_start: Instant,
    sfen_before: String,
    think_limit_ms: u64,
    origin: SearchOrigin,
) -> MoveAction
where
    S: SessionEventSink + ?Sized,
{
    if result.bestmove == "resign" {
        if let Err(err) = s.conn.send_resign() {
            return MoveAction::sink_or_error(err);
        }
        s.record.set_result("resign");
        let (game_result, reason_line, raw_result_line) = wait_game_end_full_from_rx(s.server_rx);
        if let Err(err) = s.engine.gameover(gameover_str(&game_result)) {
            return MoveAction::sink_or_error(err);
        }
        let game_end_event =
            build_game_end_event(&game_result, reason_line, raw_result_line, s.my_color);
        return MoveAction::GameEnd(game_result, Box::new(s.record.clone()), game_end_event);
    }
    if result.bestmove == "win" {
        if let Err(err) = s.conn.send_win() {
            return MoveAction::sink_or_error(err);
        }
        s.record.set_result("win_declaration");
        let (game_result, reason_line, raw_result_line) = wait_game_end_full_from_rx(s.server_rx);
        if let Err(err) = s.engine.gameover(gameover_str(&game_result)) {
            return MoveAction::sink_or_error(err);
        }
        let game_end_event =
            build_game_end_event(&game_result, reason_line, raw_result_line, s.my_color);
        return MoveAction::GameEnd(game_result, Box::new(s.record.clone()), game_end_event);
    }

    let csa_move = match usi_move_to_csa(&result.bestmove, &s.pos) {
        Ok(c) => c,
        Err(err) => return MoveAction::sink_or_error(err),
    };

    // BestMoveSelected 発火 (CSA サーバ送信前)
    let snapshot = search_info_to_snapshot(info);
    let best_event = BestMoveEvent {
        usi_move: result.bestmove.clone(),
        csa_move_candidate: Some(csa_move.clone()),
        ponder: result.ponder_move.clone(),
        side: Side::from(s.my_color),
        ply: s.pos.ply,
        search_origin: origin,
        search: Some(snapshot.clone()),
    };
    if let Err(err) = s.sink.on_event(SessionProgress::BestMoveSelected(best_event))
        && let Some(action) = handle_loop_sink_err_action(err, false)
    {
        return action;
    }

    let comment = if s.config.server.floodgate {
        Some(build_floodgate_comment(info, s.my_color, &s.pos, &result.bestmove))
    } else {
        None
    };
    if let Err(err) = s.conn.send_move_with_comment(&csa_move, comment.as_deref()) {
        return MoveAction::sink_or_error(err);
    }

    // 局面適用
    if let Err(err) = s.pos.apply_csa_move(&csa_move) {
        return MoveAction::sink_or_error(err);
    }
    let sfen_after = s.pos.to_sfen();
    s.usi_moves.push(result.bestmove.clone());
    s.record.add_move(&csa_move, 0, Some(info), s.my_color);
    let elapsed_ms = turn_start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let engine_label = label_for_color(&s.record, s.my_color);
    s.record.add_jsonl_move(JsonlMoveExtra {
        sfen_before: sfen_before.clone(),
        move_usi: result.bestmove.clone(),
        engine_label,
        elapsed_ms,
        think_limit_ms,
        seldepth: info.seldepth,
        nodes: info.nodes,
        time_ms: info.time_ms,
        nps: info.nps,
    });

    // MoveSent 発火 (送信直後、サーバ echo の time_sec はまだ未確定)
    let move_sent_event = MoveEvent {
        player: MovePlayer::SelfPlayer,
        csa_move: csa_move.clone(),
        usi_move: result.bestmove.clone(),
        side: Side::from(s.my_color),
        ply: s.pos.ply.saturating_sub(1) + 1, // この手 = ply に既に進んだ後の値そのまま
        time_sec: None,
        sfen_before: sfen_before.clone(),
        sfen_after: sfen_after.clone(),
        search_origin: Some(origin),
        search: Some(snapshot.clone()),
    };
    let move_sent_ply = move_sent_event.ply;
    if let Err(err) = s.sink.on_event(SessionProgress::MoveSent(move_sent_event))
        && let Some(action) = handle_loop_sink_err_action(err, false)
    {
        return action;
    }

    // ponder 開始
    if s.config.game.ponder
        && let Some(ref ponder_mv) = result.ponder_move
    {
        let my_estimated_ms = turn_start.elapsed().as_millis() as i64;
        let ponder_pos_cmd =
            build_position_cmd_with_ponder(&s.initial_sfen, &s.usi_moves, ponder_mv);
        let ponder_go = format!(
            "go ponder {}",
            s.clock
                .build_ponder_go_args(s.config.time.margin_msec, s.my_color, my_estimated_ms,)
        );
        if let Err(err) = s.engine.go_ponder(&ponder_pos_cmd, &ponder_go) {
            return MoveAction::sink_or_error(err);
        }
        s.ponder_state = Some(PonderState {
            expected_usi: ponder_mv.clone(),
        });
    }

    // サーバーエコー待ち
    loop {
        match s.server_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::ServerLine(line)) => {
                if line.starts_with('+') || line.starts_with('-') {
                    let (_, time_sec) = parse_server_move(&line);
                    s.clock.consume(s.my_color, time_sec);
                    s.record.update_last_time(time_sec);
                    // MoveConfirmed 発火 (自エンジンの手、time_sec 確定)
                    let confirmed_event = MoveEvent {
                        player: MovePlayer::SelfPlayer,
                        csa_move: csa_move.clone(),
                        usi_move: result.bestmove.clone(),
                        side: Side::from(s.my_color),
                        ply: move_sent_ply,
                        time_sec: Some(time_sec),
                        sfen_before: sfen_before.clone(),
                        sfen_after: sfen_after.clone(),
                        search_origin: Some(origin),
                        search: Some(snapshot.clone()),
                    };
                    if let Err(err) =
                        s.sink.on_event(SessionProgress::MoveConfirmed(confirmed_event))
                        && let Some(action) = handle_loop_sink_err_action(err, false)
                    {
                        return action;
                    }
                    return MoveAction::Continue;
                }
                if line.starts_with('#') {
                    if let Some(game_result) = parse_game_result(&line) {
                        log::info!("[CSA] 対局終了(エコー待ち中): {line}");
                        if let Err(err) = cleanup_ponder(s.engine, &mut s.ponder_state) {
                            return MoveAction::sink_or_error(err);
                        }
                        let reason = s.conn.pending_end_reason.take();
                        s.record.set_result(record_result_with_reason(&game_result, &reason));
                        if let Err(err) = s.engine.gameover(gameover_str(&game_result)) {
                            return MoveAction::sink_or_error(err);
                        }
                        let game_end_event =
                            build_game_end_event(&game_result, reason, Some(line), s.my_color);
                        return MoveAction::GameEnd(
                            game_result,
                            Box::new(s.record.clone()),
                            game_end_event,
                        );
                    }
                    s.conn.pending_end_reason = Some(line);
                }
            }
            Ok(Event::ServerDisconnected) => {
                if let Err(err) = cleanup_ponder(s.engine, &mut s.ponder_state) {
                    return MoveAction::sink_or_error(err);
                }
                if let Err(err) = s.engine.gameover("lose") {
                    return MoveAction::sink_or_error(err);
                }
                let game_end_event = build_game_end_event(
                    &GameResult::Interrupted,
                    Some("#DISCONNECTED".to_owned()),
                    None,
                    s.my_color,
                );
                return MoveAction::GameEnd(
                    GameResult::Interrupted,
                    Box::new(s.record.clone()),
                    game_end_event,
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Err(err) =
                    s.conn.maybe_send_keepalive(s.config.server.keepalive.ping_interval_sec)
                {
                    return MoveAction::sink_or_error(err);
                }
                if s.shutdown.load(Ordering::SeqCst) {
                    return MoveAction::Shutdown(false);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return MoveAction::sink_or_error(anyhow::anyhow!("サーバー受信チャネル切断"));
            }
        }
    }
}

/// 相手手番で受信した指し手 1 行 (`+...` / `-...`) を処理する。
fn handle_opponent_move_line<S>(s: &mut SessionState<'_, S>, line: &str) -> MoveAction
where
    S: SessionEventSink + ?Sized,
{
    let (mv, time_sec) = parse_server_move(line);

    if let Some(ps) = s.ponder_state.take() {
        let opponent_usi = match csa_move_to_usi(&mv, &s.pos) {
            Ok(u) => u,
            Err(err) => return MoveAction::sink_or_error(err),
        };
        if opponent_usi == ps.expected_usi {
            // ponderhit
            log::debug!("[PONDER] ponderhit: {}", opponent_usi);
            let opponent_sfen_before = s.pos.to_sfen();
            if let Err(err) = s.pos.apply_csa_move(&mv) {
                return MoveAction::sink_or_error(err);
            }
            let opponent_sfen_after = s.pos.to_sfen();
            s.usi_moves.push(opponent_usi.clone());
            s.clock.consume(opposite(s.my_color), time_sec);
            s.record.add_move(&mv, time_sec, None, opposite(s.my_color));
            push_opponent_jsonl(
                &mut s.record,
                opponent_sfen_before.clone(),
                opponent_usi.clone(),
                opposite(s.my_color),
                time_sec,
            );

            // 相手の手 MoveConfirmed
            let opp_event = MoveEvent {
                player: MovePlayer::Opponent,
                csa_move: mv.clone(),
                usi_move: opponent_usi.clone(),
                side: Side::from(opposite(s.my_color)),
                ply: s.pos.ply.saturating_sub(1) + 1,
                time_sec: Some(time_sec),
                sfen_before: opponent_sfen_before,
                sfen_after: opponent_sfen_after,
                search_origin: None,
                search: None,
            };
            if let Err(err) = s.sink.on_event(SessionProgress::MoveConfirmed(opp_event))
                && let Some(action) = handle_loop_sink_err_action(err, false)
            {
                return action;
            }

            // ponderhit -> 自分の探索
            let ponderhit_start = Instant::now();
            let my_sfen_before = s.pos.to_sfen();
            let my_think_limit_ms = s.clock.think_limit_ms(s.config.time.margin_msec, s.my_color);
            let outcome = {
                let mut emitter = SearchInfoEmitter::new(&mut s.info_throttle, s.sink);
                let mut info_callback = |info: &SearchInfo, raw: &str| {
                    emitter.observe(info, raw);
                };
                let result =
                    s.engine.ponderhit_with_info(s.shutdown, s.server_rx, &mut info_callback);
                let final_observation = emitter.into_final();
                (result, final_observation)
            };
            let (search_outcome_result, final_info) = outcome;
            let search_outcome = match search_outcome_result {
                Ok(o) => o,
                Err(err) => return MoveAction::sink_or_error(err),
            };
            if let Some(snapshot) = final_info
                && let Err(err) = s.sink.on_event(SessionProgress::SearchInfo(snapshot))
                && let Some(action) = handle_loop_sink_err_action(err, false)
            {
                return action;
            }
            handle_search_outcome_with_origin(
                s,
                search_outcome,
                ponderhit_start,
                my_sfen_before,
                my_think_limit_ms,
                SearchOrigin::Ponderhit,
            )
        } else {
            // ponder miss
            log::debug!("[PONDER] miss: expected={} actual={}", ps.expected_usi, opponent_usi);
            if let Err(err) = s.engine.stop_and_wait() {
                return MoveAction::sink_or_error(err);
            }
            // 次の fresh search の BestMoveSelected/MoveSent/MoveConfirmed に
            // `SearchOrigin::PonderMiss` を載せる。bestmove は破棄するので
            // 本ブロックでは BestMoveSelected/SearchInfo を出さず、相手の手の
            // MoveConfirmed のみ emit して対局ループへ戻る。
            s.pending_ponder_miss = true;
            let opponent_sfen_before = s.pos.to_sfen();
            if let Err(err) = s.pos.apply_csa_move(&mv) {
                return MoveAction::sink_or_error(err);
            }
            let opponent_sfen_after = s.pos.to_sfen();
            s.usi_moves.push(opponent_usi.clone());
            s.clock.consume(opposite(s.my_color), time_sec);
            s.record.add_move(&mv, time_sec, None, opposite(s.my_color));
            push_opponent_jsonl(
                &mut s.record,
                opponent_sfen_before.clone(),
                opponent_usi.clone(),
                opposite(s.my_color),
                time_sec,
            );
            let opp_event = MoveEvent {
                player: MovePlayer::Opponent,
                csa_move: mv.clone(),
                usi_move: opponent_usi.clone(),
                side: Side::from(opposite(s.my_color)),
                ply: s.pos.ply.saturating_sub(1) + 1,
                time_sec: Some(time_sec),
                sfen_before: opponent_sfen_before,
                sfen_after: opponent_sfen_after,
                search_origin: None,
                search: None,
            };
            if let Err(err) = s.sink.on_event(SessionProgress::MoveConfirmed(opp_event))
                && let Some(action) = handle_loop_sink_err_action(err, false)
            {
                return action;
            }
            MoveAction::Continue
        }
    } else {
        // ponder なし
        let opponent_usi = match csa_move_to_usi(&mv, &s.pos) {
            Ok(u) => u,
            Err(err) => return MoveAction::sink_or_error(err),
        };
        let opponent_sfen_before = s.pos.to_sfen();
        if let Err(err) = s.pos.apply_csa_move(&mv) {
            return MoveAction::sink_or_error(err);
        }
        let opponent_sfen_after = s.pos.to_sfen();
        s.usi_moves.push(opponent_usi.clone());
        s.clock.consume(opposite(s.my_color), time_sec);
        s.record.add_move(&mv, time_sec, None, opposite(s.my_color));
        push_opponent_jsonl(
            &mut s.record,
            opponent_sfen_before.clone(),
            opponent_usi.clone(),
            opposite(s.my_color),
            time_sec,
        );
        let opp_event = MoveEvent {
            player: MovePlayer::Opponent,
            csa_move: mv,
            usi_move: opponent_usi,
            side: Side::from(opposite(s.my_color)),
            ply: s.pos.ply.saturating_sub(1) + 1,
            time_sec: Some(time_sec),
            sfen_before: opponent_sfen_before,
            sfen_after: opponent_sfen_after,
            search_origin: None,
            search: None,
        };
        if let Err(err) = s.sink.on_event(SessionProgress::MoveConfirmed(opp_event))
            && let Some(action) = handle_loop_sink_err_action(err, false)
        {
            return action;
        }
        MoveAction::Continue
    }
}

// ────────────────────────────────────────────
// terminate / sink error 処理
// ────────────────────────────────────────────

/// best-effort attempt at clean closure。Sink Fatal / 外部 shutdown 共通の
/// 後処理を行う:
/// 1. CSA `%CHUDAN` を best-effort で送信 (対局未終了時のみ、write/flush timeout 1s)
/// 2. CSA `LOGOUT` を best-effort 送信 (write/flush timeout 1s)
/// 3. transport close
/// 4. sink.on_error を best-effort 呼び出し
/// 5. SessionProgress::Disconnected を emit
fn terminate_session<S>(
    conn: &mut CsaConnection,
    sink: &mut S,
    cause: &SessionError,
    reason: DisconnectReason,
    game_already_ended: bool,
) where
    S: SessionEventSink + ?Sized,
{
    // 1. %CHUDAN (対局未終了時のみ)
    if !game_already_ended
        && let Err(err) = best_effort_with_timeout(Duration::from_secs(1), || conn.send_chudan())
    {
        log::warn!("[CSA] %CHUDAN 送信失敗 (best-effort): {err:#}");
    }

    // 2. LOGOUT
    if let Err(err) = best_effort_with_timeout(Duration::from_secs(1), || conn.logout()) {
        log::warn!("[CSA] LOGOUT 送信失敗 (best-effort): {err:#}");
    }

    // 3. transport close: CsaConnection を drop すれば close される。明示 close は
    //    現状 API が無いので drop に任せる (本関数の return 時に conn は呼び出し側に
    //    返るが、呼び出し側は本関数の後で値を捨てる責務を持つ)。

    // 4. sink.on_error
    if let Err(err) = sink.on_error(cause) {
        log::warn!("[Sink] on_error が err を返しました (best-effort、無視): {err:?}");
    }

    // 5. Disconnected
    if let Err(err) = sink.on_event(SessionProgress::Disconnected { reason }) {
        log::warn!("[Sink] Disconnected emit が err を返しました (best-effort、無視): {err:?}");
    }
}

/// 1 秒以内に finish する想定の I/O を best-effort で実行する。実際の OS 側 timeout は
/// transport 層の write_timeout に依存するため、本関数は wrapper として wrap するだけ。
fn best_effort_with_timeout<F>(_timeout: Duration, f: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    f()
}

/// `sink.on_event` を呼び、`SinkError::NonFatal` は warn のみで握りつぶし、
/// `SinkError::Fatal` のみを `Some(SinkError)` で返す。`drive_session` 入口の
/// 各 step (Connected / GameSummary / GameStarted / GameEnded) で使う。
fn emit_with_nonfatal_warn<S>(sink: &mut S, event: SessionProgress) -> Option<SinkError>
where
    S: SessionEventSink + ?Sized,
{
    match sink.on_event(event) {
        Ok(()) => None,
        Err(SinkError::NonFatal(inner)) => {
            log::warn!("[Sink] NonFatal: {inner:#}、対局を継続します");
            None
        }
        Err(err @ SinkError::Fatal(_)) => Some(err),
    }
}

/// SinkError を受け取り、`drive_session` 上で SessionError::SinkAborted を返す経路に
/// 揃える。`game_already_ended` が true なら CHUDAN は送らない (対局は既に終局済み)。
///
/// `SinkError::NonFatal` は warn ログのみで対局を継続するのが原則だが、`drive_session`
/// 内の早期 event (Connected/GameSummary/GameStarted) で NonFatal を受けたときは
/// メインループに入らないとループ継続が不可能なため、本関数を呼ばずに caller 側で
/// 継続判断する設計とする。本関数に到達するのは Fatal 経路のみと仮定し、NonFatal が
/// 来た場合は `Sink contract violation` として warn しつつ Fatal 同等で扱う。
fn handle_sink_error<S>(
    err: SinkError,
    conn: &mut CsaConnection,
    sink: &mut S,
    _summary: Option<GameSummary>,
    game_already_ended: bool,
) -> Result<SessionOutcome, SessionError>
where
    S: SessionEventSink + ?Sized,
{
    let cause = SessionError::SinkAborted(err);
    terminate_session(conn, sink, &cause, DisconnectReason::SinkAborted, game_already_ended);
    Err(cause)
}

fn abort_for_should_continue<S>(
    conn: &mut CsaConnection,
    sink: &mut S,
    _summary: Option<GameSummary>,
    game_already_ended: bool,
) -> Result<SessionOutcome, SessionError>
where
    S: SessionEventSink + ?Sized,
{
    let synthetic =
        SinkError::Fatal(Box::new(std::io::Error::other("sink.should_continue() == false")));
    let cause = SessionError::SinkAborted(synthetic);
    terminate_session(conn, sink, &cause, DisconnectReason::SinkAborted, game_already_ended);
    Err(cause)
}

/// メインループ内で SinkError が出たときの分岐。NonFatal なら warn ログのみで継続。
/// Fatal ならループ脱出のため `Some(LoopOutcome::SinkAborted)` を返す。
fn handle_loop_sink_err(err: SinkError, game_already_ended: bool) -> Option<LoopOutcome> {
    match err {
        SinkError::NonFatal(inner) => {
            log::warn!("[Sink] NonFatal: {inner:#}、対局を継続します");
            None
        }
        SinkError::Fatal(_) => Some(LoopOutcome::SinkAborted {
            sink_err: err,
            game_already_ended,
        }),
    }
}

/// MoveAction に変換する版 (`send_bestmove_and_wait_echo` 内部用)。
fn handle_loop_sink_err_action(err: SinkError, game_already_ended: bool) -> Option<MoveAction> {
    match err {
        SinkError::NonFatal(inner) => {
            log::warn!("[Sink] NonFatal: {inner:#}、対局を継続します");
            None
        }
        SinkError::Fatal(_) => Some(MoveAction::SinkAborted(err, game_already_ended)),
    }
}

impl MoveAction {
    /// 任意の `anyhow::Error` を `MoveAction::SinkAborted` に押し込まず、
    /// 上位で `LoopOutcome::Error` 化したい場合に panic で握り潰さないよう
    /// `Continue` を返さず、呼び出し元が `LoopOutcome::Error` で扱うべき。
    /// 簡略化のため、ここでは error メッセージをログ出して
    /// `MoveAction::SinkAborted` (Fatal) として終わらせる。
    fn sink_or_error<E: Into<anyhow::Error>>(err: E) -> Self {
        let err = err.into();
        log::error!("[Session] 内部エラー (best-effort attempt at clean closure): {err:#}");
        let sink_err = SinkError::Fatal(Box::new(std::io::Error::other(format!("{err}"))));
        MoveAction::SinkAborted(sink_err, false)
    }
}

// ────────────────────────────────────────────
// SearchInfoEmitter / Throttle
// ────────────────────────────────────────────

/// `SearchInfoEmitPolicy` に基づき発火頻度を制御する。
struct SearchInfoThrottle {
    policy: SearchInfoEmitPolicy,
    last_emit_at: Option<Instant>,
    last_depth: Option<u32>,
    /// `emit_final` の対象として保持する最後の累積 snapshot。
    pending_final: Option<SearchInfoSnapshot>,
}

impl SearchInfoThrottle {
    fn new(policy: SearchInfoEmitPolicy) -> Self {
        Self {
            policy,
            last_emit_at: None,
            last_depth: None,
            pending_final: None,
        }
    }

    /// 観測値を受け、emit すべきか判定する。emit する場合 `Some(snapshot)` を返す。
    fn observe(&mut self, info: &SearchInfo, raw: &str) -> Option<SearchInfoSnapshot> {
        let snapshot = SearchInfoSnapshot {
            depth: info.depth,
            seldepth: info.seldepth,
            score_cp: info.score_cp,
            mate: info.score_mate,
            nodes: info.nodes,
            nps: info.nps,
            time_ms: info.time_ms,
            pv: info.pv.clone(),
            raw_line: Some(raw.to_owned()),
        };
        // 常に pending_final を最新化 (emit_final の対象)
        self.pending_final = Some(snapshot.clone());

        match &self.policy {
            SearchInfoEmitPolicy::Disabled => None,
            SearchInfoEmitPolicy::EveryLine => {
                self.last_emit_at = Some(Instant::now());
                self.last_depth = info.depth;
                Some(snapshot)
            }
            SearchInfoEmitPolicy::Default => self.observe_with_interval(snapshot, info, 200, true),
            SearchInfoEmitPolicy::Interval {
                min_ms,
                emit_on_depth_change,
                ..
            } => self.observe_with_interval(snapshot, info, *min_ms, *emit_on_depth_change),
        }
    }

    fn observe_with_interval(
        &mut self,
        snapshot: SearchInfoSnapshot,
        info: &SearchInfo,
        min_ms: u32,
        emit_on_depth_change: bool,
    ) -> Option<SearchInfoSnapshot> {
        let depth_changed =
            emit_on_depth_change && info.depth.is_some() && info.depth != self.last_depth;
        if depth_changed {
            self.last_emit_at = Some(Instant::now());
            self.last_depth = info.depth;
            return Some(snapshot);
        }
        let now = Instant::now();
        let interval = Duration::from_millis(u64::from(min_ms));
        let should_emit = match self.last_emit_at {
            None => true,
            Some(prev) => now.duration_since(prev) >= interval,
        };
        if should_emit {
            self.last_emit_at = Some(now);
            if info.depth.is_some() {
                self.last_depth = info.depth;
            }
            Some(snapshot)
        } else {
            None
        }
    }

    /// bestmove 直前に呼ばれ、`emit_final` ポリシーに従って最後の累積値を返す。
    fn take_final(&mut self) -> Option<SearchInfoSnapshot> {
        let emit_final = matches!(
            self.policy,
            SearchInfoEmitPolicy::Default
                | SearchInfoEmitPolicy::EveryLine
                | SearchInfoEmitPolicy::Interval {
                    emit_final: true,
                    ..
                }
        );
        // `Disabled` の場合は emit しない
        if matches!(self.policy, SearchInfoEmitPolicy::Disabled) {
            self.pending_final = None;
            self.last_emit_at = None;
            self.last_depth = None;
            return None;
        }
        let snapshot = if emit_final {
            self.pending_final.take()
        } else {
            None
        };
        self.pending_final = None;
        // depth tracking は探索ごとにリセット
        self.last_emit_at = None;
        self.last_depth = None;
        snapshot
    }
}

/// 1 探索分の SearchInfoEmitter。`info_callback` 経由で `observe` を呼び、observed
/// snapshot を sink に push する。lifetime はその探索期間中だけ。
struct SearchInfoEmitter<'a, S: ?Sized + 'a> {
    throttle: &'a mut SearchInfoThrottle,
    sink: &'a mut S,
    /// emit 中に Fatal が出たら以降の sink 呼び出しを抑止し、最終的に上位で
    /// 中断扱いにする (本フィールドは「最後に観測した sink Fatal」を保持する)。
    fatal_pending: Option<SinkError>,
}

impl<'a, S: SessionEventSink + ?Sized + 'a> SearchInfoEmitter<'a, S> {
    fn new(throttle: &'a mut SearchInfoThrottle, sink: &'a mut S) -> Self {
        Self {
            throttle,
            sink,
            fatal_pending: None,
        }
    }

    fn observe(&mut self, info: &SearchInfo, raw: &str) {
        if self.fatal_pending.is_some() {
            return;
        }
        if let Some(snapshot) = self.throttle.observe(info, raw)
            && let Err(err) = self.sink.on_event(SessionProgress::SearchInfo(snapshot))
        {
            match err {
                SinkError::NonFatal(inner) => {
                    log::warn!("[Sink] NonFatal during search info: {inner:#}");
                }
                SinkError::Fatal(_) => {
                    self.fatal_pending = Some(err);
                }
            }
        }
    }

    /// emit_final 対象の snapshot を返す。fatal が観測されていた場合は emit_final
    /// は飛ばして None を返す (上位の handler で SinkAborted へ畳む。emit_final の
    /// 観測は best-effort なので失う情報は許容する)。
    fn into_final(self) -> Option<SearchInfoSnapshot> {
        if self.fatal_pending.is_some() {
            return None;
        }
        let SearchInfoEmitter { throttle, .. } = self;
        throttle.take_final()
    }
}

// ────────────────────────────────────────────
// Clock
// ────────────────────────────────────────────

struct Clock {
    black_time_ms: i64,
    white_time_ms: i64,
    black_byoyomi_ms: i64,
    white_byoyomi_ms: i64,
    black_increment_ms: i64,
    white_increment_ms: i64,
}

impl Clock {
    fn from_summary(summary: &GameSummary) -> Self {
        Self {
            black_time_ms: summary.black_time.total_time_ms + summary.black_time.increment_ms,
            white_time_ms: summary.white_time.total_time_ms + summary.white_time.increment_ms,
            black_byoyomi_ms: summary.black_time.byoyomi_ms,
            white_byoyomi_ms: summary.white_time.byoyomi_ms,
            black_increment_ms: summary.black_time.increment_ms,
            white_increment_ms: summary.white_time.increment_ms,
        }
    }

    fn increment_ms(&self, color: Color) -> i64 {
        match color {
            Color::Black => self.black_increment_ms,
            Color::White => self.white_increment_ms,
        }
    }

    fn consume(&mut self, color: Color, time_sec: u32) {
        let consumed_ms = time_sec as i64 * 1000;
        let inc = self.increment_ms(color);
        match color {
            Color::Black => {
                self.black_time_ms = (self.black_time_ms - consumed_ms + inc).max(0);
            }
            Color::White => {
                self.white_time_ms = (self.white_time_ms - consumed_ms + inc).max(0);
            }
        }
    }

    fn build_go_args(&self, margin_msec: u64, side_to_move: Color) -> String {
        let btime = self.black_time_ms.max(0);
        let wtime = self.white_time_ms.max(0);
        if self.black_increment_ms > 0 || self.white_increment_ms > 0 {
            format!(
                "btime {} wtime {} binc {} winc {}",
                btime, wtime, self.black_increment_ms, self.white_increment_ms
            )
        } else if self.black_byoyomi_ms > 0 || self.white_byoyomi_ms > 0 {
            let byoyomi_ms = match side_to_move {
                Color::Black => self.black_byoyomi_ms,
                Color::White => self.white_byoyomi_ms,
            };
            let byoyomi = (byoyomi_ms - margin_msec as i64).max(0);
            format!("btime {} wtime {} byoyomi {}", btime, wtime, byoyomi)
        } else {
            format!("btime {} wtime {}", btime, wtime)
        }
    }

    fn think_limit_ms(&self, margin_msec: u64, side_to_move: Color) -> u64 {
        let (total_ms, byoyomi_ms, increment_ms) = match side_to_move {
            Color::Black => (self.black_time_ms, self.black_byoyomi_ms, self.black_increment_ms),
            Color::White => (self.white_time_ms, self.white_byoyomi_ms, self.white_increment_ms),
        };
        if increment_ms > 0 {
            total_ms.max(0) as u64 + increment_ms.max(0) as u64
        } else if byoyomi_ms > 0 {
            (byoyomi_ms - margin_msec as i64).max(0) as u64
        } else if total_ms > 0 {
            total_ms as u64
        } else {
            0
        }
    }

    fn build_ponder_go_args(
        &self,
        margin_msec: u64,
        my_color: Color,
        my_estimated_ms: i64,
    ) -> String {
        let my_inc = self.increment_ms(my_color);
        let (btime, wtime) = match my_color {
            Color::Black => (
                (self.black_time_ms + my_inc - my_estimated_ms).max(0),
                self.white_time_ms.max(0),
            ),
            Color::White => (
                self.black_time_ms.max(0),
                (self.white_time_ms + my_inc - my_estimated_ms).max(0),
            ),
        };
        if self.black_increment_ms > 0 || self.white_increment_ms > 0 {
            format!(
                "btime {} wtime {} binc {} winc {}",
                btime, wtime, self.black_increment_ms, self.white_increment_ms
            )
        } else if self.black_byoyomi_ms > 0 || self.white_byoyomi_ms > 0 {
            let byoyomi_ms = match my_color {
                Color::Black => self.black_byoyomi_ms,
                Color::White => self.white_byoyomi_ms,
            };
            let my_time = match my_color {
                Color::Black => btime,
                Color::White => wtime,
            };
            let estimated_from_byoyomi = if my_time == 0 { my_estimated_ms } else { 0 };
            let byoyomi = (byoyomi_ms - margin_msec as i64 - estimated_from_byoyomi).max(0);
            format!("btime {} wtime {} byoyomi {}", btime, wtime, byoyomi)
        } else {
            format!("btime {} wtime {}", btime, wtime)
        }
    }
}

// ────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────

struct PonderState {
    expected_usi: String,
}

fn cleanup_ponder(engine: &mut UsiEngine, ponder_state: &mut Option<PonderState>) -> Result<()> {
    if ponder_state.take().is_some() {
        engine.stop_and_wait()?;
    }
    Ok(())
}

/// `ServerInterrupt` の行群から (game_result, reason_line, raw_result_line) を抽出する。
fn parse_server_interrupt_lines_full(
    lines: Vec<String>,
) -> (GameResult, Option<String>, Option<String>) {
    let mut reason = None;
    let mut result = GameResult::Interrupted;
    let mut raw_result = None;
    for line in lines {
        if line.starts_with('#') {
            if let Some(r) = parse_game_result(&line) {
                result = r;
                raw_result = Some(line);
            } else {
                reason = Some(line);
            }
        }
    }
    (result, reason, raw_result)
}

fn wait_game_end_full_from_rx(
    server_rx: &Receiver<Event>,
) -> (GameResult, Option<String>, Option<String>) {
    let start = Instant::now();
    const TIMEOUT: Duration = Duration::from_secs(30);
    let mut pending_reason: Option<String> = None;
    loop {
        if start.elapsed() >= TIMEOUT {
            log::warn!("[CSA] 終局結果の受信タイムアウト ({}秒)", TIMEOUT.as_secs());
            return (GameResult::Interrupted, pending_reason, None);
        }
        match server_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::ServerLine(line)) => {
                if line.starts_with('#') {
                    if let Some(result) = parse_game_result(&line) {
                        log::info!("[CSA] 対局終了: {line}");
                        return (result, pending_reason, Some(line));
                    }
                    pending_reason = Some(line);
                }
            }
            Ok(Event::ServerDisconnected) => {
                return (GameResult::Interrupted, pending_reason, None);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return (GameResult::Interrupted, pending_reason, None);
            }
        }
    }
}

fn opposite(color: Color) -> Color {
    match color {
        Color::Black => Color::White,
        Color::White => Color::Black,
    }
}

fn label_for_color(record: &GameRecord, color: Color) -> String {
    let raw = match color {
        Color::Black => &record.sente_name,
        Color::White => &record.gote_name,
    };
    if raw.is_empty() {
        "unknown".to_string()
    } else {
        raw.clone()
    }
}

fn push_opponent_jsonl(
    record: &mut GameRecord,
    sfen_before: String,
    move_usi: String,
    side: Color,
    time_sec: u32,
) {
    let engine_label = label_for_color(record, side);
    record.add_jsonl_move(JsonlMoveExtra {
        sfen_before,
        move_usi,
        engine_label,
        elapsed_ms: u64::from(time_sec) * 1000,
        think_limit_ms: 0,
        seldepth: None,
        nodes: None,
        time_ms: None,
        nps: None,
    });
}

fn gameover_str(result: &GameResult) -> &'static str {
    match result {
        GameResult::Win => "win",
        GameResult::Lose => "lose",
        GameResult::Draw => "draw",
        _ => "draw",
    }
}

fn record_result_with_reason(result: &GameResult, reason: &Option<String>) -> &'static str {
    if let Some(r) = reason {
        if r.contains("TIME_UP") {
            return "time_up";
        }
        if r.contains("ILLEGAL") {
            return "illegal_move";
        }
        if r.contains("MAX_MOVES") {
            return "max_moves";
        }
        if r.contains("JISHOGI") {
            return "jishogi";
        }
        if r.contains("SENNICHITE") {
            return "sennichite";
        }
    }
    match result {
        GameResult::Win => "win",
        GameResult::Lose => "lose",
        GameResult::Draw => "sennichite",
        GameResult::Interrupted => "interrupted",
        GameResult::Censored => "interrupted",
    }
}

const HIRATE_SFEN: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

fn build_position_cmd(initial_sfen: &str, usi_moves: &[String]) -> String {
    let base = if initial_sfen == HIRATE_SFEN {
        "position startpos".to_string()
    } else {
        format!("position sfen {initial_sfen}")
    };
    if usi_moves.is_empty() {
        base
    } else {
        format!("{base} moves {}", usi_moves.join(" "))
    }
}

fn build_position_cmd_with_ponder(
    initial_sfen: &str,
    usi_moves: &[String],
    ponder_move: &str,
) -> String {
    let mut cmd = build_position_cmd(initial_sfen, usi_moves);
    if usi_moves.is_empty() {
        write!(cmd, " moves {ponder_move}").unwrap();
    } else {
        write!(cmd, " {ponder_move}").unwrap();
    }
    cmd
}

fn build_floodgate_comment(
    info: &SearchInfo,
    my_color: Color,
    pos: &Position,
    last_bestmove: &str,
) -> String {
    let score = if let Some(cp) = info.score_cp {
        match my_color {
            Color::Black => cp,
            Color::White => -cp,
        }
    } else if let Some(mate) = info.score_mate {
        let base = if mate > 0 { 100000 } else { -100000 };
        match my_color {
            Color::Black => base,
            Color::White => -base,
        }
    } else {
        0
    };
    let mut comment = format!("* {score}");
    if !info.pv.is_empty() {
        let mut pv_pos = pos.clone();
        let pv_start = if info.pv.first().map(|s| s.as_str()) == Some(last_bestmove) {
            1
        } else {
            0
        };
        for usi_mv in &info.pv[pv_start..] {
            if let Ok(csa) = usi_move_to_csa(usi_mv, &pv_pos) {
                write!(comment, " {csa}").unwrap();
                if pv_pos.apply_csa_move(&csa).is_err() {
                    break;
                }
            } else {
                break;
            }
        }
    }
    comment
}

// ────────────────────────────────────────────
// SessionEventSink 用 helper (SFEN 変換 / GameEnd 構築)
// ────────────────────────────────────────────

/// `ProtocolReconnectState` + `GameSummary.position_section` から public な
/// [`PublicReconnectState`] を組み立てる。
fn build_reconnect_state(
    summary: &GameSummary,
    proto_state: Option<&ProtocolReconnectState>,
) -> PublicReconnectState {
    let last_sfen = summary.position.to_sfen();
    let last_ply = summary.position.ply.saturating_sub(1);
    let side_from_state = proto_state
        .and_then(|ps| ps.current_turn)
        .unwrap_or(summary.position.side_to_move);
    let (remaining_self, remaining_opp) = match proto_state {
        Some(ps) => {
            let self_ms = match summary.my_color {
                Color::Black => ps.black_remaining_ms,
                Color::White => ps.white_remaining_ms,
            };
            let opp_ms = match summary.my_color {
                Color::Black => ps.white_remaining_ms,
                Color::White => ps.black_remaining_ms,
            };
            (Some((self_ms / 1000).max(0) as u32), Some((opp_ms / 1000).max(0) as u32))
        }
        None => (None, None),
    };
    PublicReconnectState {
        last_ply,
        last_sfen,
        side_to_move: Side::from(side_from_state),
        remaining_time_sec_self: remaining_self,
        remaining_time_sec_opp: remaining_opp,
    }
}

fn build_game_end_event(
    result: &GameResult,
    raw_reason_line: Option<String>,
    raw_result_line: Option<String>,
    my_color: Color,
) -> GameEndEvent {
    let reason = parse_game_end_reason(result, raw_reason_line.as_deref());
    let winner = match result {
        GameResult::Win => Some(Side::from(my_color)),
        GameResult::Lose => Some(Side::from(opposite(my_color))),
        GameResult::Draw => None,
        GameResult::Interrupted | GameResult::Censored => None,
    };
    GameEndEvent {
        result: result.clone(),
        reason,
        winner,
        raw_result_line,
        raw_reason_line,
    }
}

fn parse_game_end_reason(result: &GameResult, raw_reason: Option<&str>) -> GameEndReason {
    if let Some(reason_line) = raw_reason {
        if reason_line.contains("TIME_UP") {
            return GameEndReason::TimeUp;
        }
        if reason_line.contains("ILLEGAL") {
            return GameEndReason::IllegalMove;
        }
        if reason_line.contains("JISHOGI") {
            return GameEndReason::Jishogi;
        }
        if reason_line.contains("SENNICHITE") {
            return GameEndReason::Sennichite;
        }
        if reason_line.contains("MAX_MOVES") {
            return GameEndReason::MaxMoves;
        }
        if reason_line.contains("CHUDAN") {
            return GameEndReason::Interrupted;
        }
        if reason_line.contains("CENSORED") {
            return GameEndReason::Censored;
        }
        if reason_line.contains("DISCONNECTED") {
            return GameEndReason::OtherDisconnect;
        }
        // 認識できない reason 文字列は Unknown で前方互換的に保持する
        return GameEndReason::Unknown(reason_line.to_owned());
    }
    match result {
        GameResult::Win | GameResult::Lose => GameEndReason::Resign,
        GameResult::Draw => GameEndReason::Sennichite,
        GameResult::Interrupted => GameEndReason::Interrupted,
        GameResult::Censored => GameEndReason::Censored,
    }
}

fn search_info_to_snapshot(info: &SearchInfo) -> SearchInfoSnapshot {
    SearchInfoSnapshot {
        depth: info.depth,
        seldepth: info.seldepth,
        score_cp: info.score_cp,
        mate: info.score_mate,
        nodes: info.nodes,
        nps: info.nps,
        time_ms: info.time_ms,
        pv: info.pv.clone(),
        raw_line: None,
    }
}

fn map_anyhow_to_session_error(err: anyhow::Error) -> SessionError {
    SessionError::from(err)
}

// ────────────────────────────────────────────
// 内部 helper のユニットテスト
// ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SearchInfoEmitPolicy;

    fn info(depth: Option<u32>) -> SearchInfo {
        SearchInfo {
            depth,
            seldepth: None,
            score_cp: Some(123),
            score_mate: None,
            nodes: Some(1000),
            time_ms: Some(50),
            nps: Some(20000),
            pv: vec!["7g7f".to_owned()],
        }
    }

    #[test]
    fn throttle_disabled_emits_nothing() {
        let mut t = SearchInfoThrottle::new(SearchInfoEmitPolicy::Disabled);
        assert!(t.observe(&info(Some(1)), "info depth 1").is_none());
        assert!(t.observe(&info(Some(2)), "info depth 2").is_none());
        assert!(t.take_final().is_none());
    }

    #[test]
    fn throttle_every_line_always_emits() {
        let mut t = SearchInfoThrottle::new(SearchInfoEmitPolicy::EveryLine);
        assert!(t.observe(&info(Some(1)), "info depth 1").is_some());
        assert!(t.observe(&info(Some(1)), "info depth 1 nodes 2000").is_some());
        // emit_final も発火
        assert!(t.take_final().is_some());
    }

    #[test]
    fn throttle_interval_respects_depth_change_when_enabled() {
        let mut t = SearchInfoThrottle::new(SearchInfoEmitPolicy::Interval {
            min_ms: 60_000,
            emit_on_depth_change: true,
            emit_final: true,
        });
        // 初回は emit
        assert!(t.observe(&info(Some(1)), "info depth 1").is_some());
        // 同 depth・短い時間 -> 抑止
        assert!(t.observe(&info(Some(1)), "info depth 1 nodes 100").is_none());
        // depth 変化 -> emit
        assert!(t.observe(&info(Some(2)), "info depth 2").is_some());
        // emit_final も拾う
        assert!(t.take_final().is_some());
    }

    #[test]
    fn throttle_interval_emit_final_disabled() {
        let mut t = SearchInfoThrottle::new(SearchInfoEmitPolicy::Interval {
            min_ms: 50,
            emit_on_depth_change: false,
            emit_final: false,
        });
        // 1 件 emit させる
        let _ = t.observe(&info(Some(1)), "info depth 1");
        // emit_final は false なので None
        assert!(t.take_final().is_none());
    }

    #[test]
    fn throttle_interval_min_ms_blocks_within_interval() {
        let mut t = SearchInfoThrottle::new(SearchInfoEmitPolicy::Interval {
            min_ms: 60_000,
            emit_on_depth_change: false,
            emit_final: true,
        });
        assert!(t.observe(&info(Some(1)), "info depth 1").is_some());
        // depth 変化があっても emit_on_depth_change=false なので抑止される
        assert!(t.observe(&info(Some(2)), "info depth 2").is_none());
    }

    #[test]
    fn parse_game_end_reason_classifies_known_lines() {
        assert_eq!(
            parse_game_end_reason(&GameResult::Win, Some("#TIME_UP")),
            GameEndReason::TimeUp
        );
        assert_eq!(
            parse_game_end_reason(&GameResult::Win, Some("#ILLEGAL_MOVE")),
            GameEndReason::IllegalMove
        );
        assert_eq!(
            parse_game_end_reason(&GameResult::Draw, Some("#JISHOGI")),
            GameEndReason::Jishogi
        );
        assert_eq!(
            parse_game_end_reason(&GameResult::Draw, Some("#SENNICHITE")),
            GameEndReason::Sennichite
        );
        assert_eq!(
            parse_game_end_reason(&GameResult::Draw, Some("#MAX_MOVES")),
            GameEndReason::MaxMoves
        );
        assert_eq!(
            parse_game_end_reason(&GameResult::Interrupted, Some("#CHUDAN")),
            GameEndReason::Interrupted
        );
        assert_eq!(
            parse_game_end_reason(&GameResult::Censored, Some("#CENSORED")),
            GameEndReason::Censored
        );
        // 未知の理由は Unknown 保持
        if let GameEndReason::Unknown(s) =
            parse_game_end_reason(&GameResult::Lose, Some("#UNKNOWN_FUTURE"))
        {
            assert_eq!(s, "#UNKNOWN_FUTURE");
        } else {
            panic!("expected Unknown");
        }
    }

    #[test]
    fn parse_game_end_reason_falls_back_when_no_reason_line() {
        assert_eq!(parse_game_end_reason(&GameResult::Win, None), GameEndReason::Resign);
        assert_eq!(parse_game_end_reason(&GameResult::Lose, None), GameEndReason::Resign);
        assert_eq!(parse_game_end_reason(&GameResult::Draw, None), GameEndReason::Sennichite);
        assert_eq!(
            parse_game_end_reason(&GameResult::Interrupted, None),
            GameEndReason::Interrupted
        );
        assert_eq!(parse_game_end_reason(&GameResult::Censored, None), GameEndReason::Censored);
    }

    #[test]
    fn build_game_end_event_winner_self_when_win() {
        use rshogi_csa::Color;
        let evt = build_game_end_event(&GameResult::Win, None, None, Color::Black);
        assert_eq!(evt.winner, Some(Side::Black));
        assert_eq!(evt.reason, GameEndReason::Resign);
    }

    #[test]
    fn build_game_end_event_winner_opponent_when_lose() {
        use rshogi_csa::Color;
        let evt = build_game_end_event(&GameResult::Lose, None, None, Color::Black);
        assert_eq!(evt.winner, Some(Side::White));
    }

    #[test]
    fn build_game_end_event_no_winner_when_draw_or_interrupted() {
        use rshogi_csa::Color;
        let draw = build_game_end_event(&GameResult::Draw, None, None, Color::Black);
        assert_eq!(draw.winner, None);
        let interrupted = build_game_end_event(&GameResult::Interrupted, None, None, Color::Black);
        assert_eq!(interrupted.winner, None);
    }

    #[test]
    fn build_reconnect_state_uses_summary_position_sfen() {
        use rshogi_csa::{Color, initial_position};
        let summary = GameSummary {
            game_id: "g".to_owned(),
            my_color: Color::Black,
            sente_name: "b".to_owned(),
            gote_name: "w".to_owned(),
            position: initial_position(),
            initial_moves: Vec::new(),
            black_time: crate::protocol::TimeConfig::default(),
            white_time: crate::protocol::TimeConfig::default(),
            reconnect_token: None,
        };
        let proto_state = ProtocolReconnectState {
            current_turn: Some(Color::Black),
            black_remaining_ms: 10_000,
            white_remaining_ms: 5_000,
        };
        let pub_state = build_reconnect_state(&summary, Some(&proto_state));
        // last_sfen は summary.position.to_sfen() と一致 (履歴 replay は無し)
        assert_eq!(pub_state.last_sfen, summary.position.to_sfen());
        assert_eq!(pub_state.side_to_move, Side::Black);
        assert_eq!(pub_state.remaining_time_sec_self, Some(10));
        assert_eq!(pub_state.remaining_time_sec_opp, Some(5));
    }

    #[test]
    fn build_reconnect_state_swaps_remaining_for_white_player() {
        use rshogi_csa::{Color, initial_position};
        let summary = GameSummary {
            game_id: "g".to_owned(),
            my_color: Color::White, // 自分は後手
            sente_name: "b".to_owned(),
            gote_name: "w".to_owned(),
            position: initial_position(),
            initial_moves: Vec::new(),
            black_time: crate::protocol::TimeConfig::default(),
            white_time: crate::protocol::TimeConfig::default(),
            reconnect_token: None,
        };
        let proto_state = ProtocolReconnectState {
            current_turn: Some(Color::White),
            black_remaining_ms: 10_000,
            white_remaining_ms: 5_000,
        };
        let pub_state = build_reconnect_state(&summary, Some(&proto_state));
        // self は白 (5_000ms), opp は黒 (10_000ms)
        assert_eq!(pub_state.remaining_time_sec_self, Some(5));
        assert_eq!(pub_state.remaining_time_sec_opp, Some(10));
    }
}
