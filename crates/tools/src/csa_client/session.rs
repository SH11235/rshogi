//! CSA対局セッション管理
//!
//! 1回の対局（ログイン〜対局〜終局）を管理する。

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::common::csa::{Color, Position, csa_move_to_usi, usi_move_to_csa};

use super::config::CsaClientConfig;
use super::engine::{BestMoveResult, SearchInfo, UsiEngine};
use super::protocol::{CsaConnection, GameResult, GameSummary, RecvEvent};
use super::record::GameRecord;

/// 対局中の時間管理（先後別に byoyomi/increment を保持）
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
        // フィッシャー: 初期持ち時間に初回インクリメントを加算（shogihome 準拠）
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

/// 対局セッションの可変状態をまとめた構造体。
/// ヘルパーメソッドへの引数を減らすために使用。
struct SessionState<'a> {
    conn: &'a mut CsaConnection,
    engine: &'a mut UsiEngine,
    config: &'a CsaClientConfig,
    shutdown: &'a AtomicBool,
    pos: Position,
    usi_moves: Vec<String>,
    clock: Clock,
    record: GameRecord,
    ponder_state: Option<PonderState>,
    my_color: Color,
    initial_sfen: String,
}

impl SessionState<'_> {
    /// 探索結果の bestmove を送信し、エコーを待ち、ponder を開始する。
    /// resign/win の場合は対局終了結果を返す。
    fn send_bestmove_and_wait_echo(
        &mut self,
        result: &BestMoveResult,
        info: &SearchInfo,
        turn_start: Instant,
    ) -> Result<Option<(GameResult, GameRecord)>> {
        // resign / win 判定
        if result.bestmove == "resign" {
            self.conn.send_resign()?;
            self.record.set_result("resign");
            let (game_result, _) = wait_game_end(self.conn)?;
            self.engine.gameover(&gameover_str(&game_result))?;
            return Ok(Some((game_result, self.record.clone())));
        }
        if result.bestmove == "win" {
            self.conn.send_win()?;
            self.record.set_result("win_declaration");
            let (game_result, _) = wait_game_end(self.conn)?;
            self.engine.gameover(&gameover_str(&game_result))?;
            return Ok(Some((game_result, self.record.clone())));
        }

        // USI手 → CSA手
        let csa_move = usi_move_to_csa(&result.bestmove, &self.pos)?;

        // floodgate コメント
        let comment = if self.config.server.floodgate {
            Some(build_floodgate_comment(info, self.my_color, &self.pos, &result.bestmove))
        } else {
            None
        };

        self.conn.send_move_with_comment(&csa_move, comment.as_deref())?;

        // 盤面更新
        self.pos.apply_csa_move(&csa_move)?;
        self.usi_moves.push(result.bestmove.clone());
        self.record.add_move(&csa_move, 0, Some(info), self.my_color);

        // ponder 開始
        if self.config.game.ponder
            && let Some(ref ponder_mv) = result.ponder_move
        {
            let my_estimated_ms = turn_start.elapsed().as_millis() as i64;
            let ponder_pos_cmd =
                build_position_cmd_with_ponder(&self.initial_sfen, &self.usi_moves, ponder_mv);
            let ponder_go = format!(
                "go ponder {}",
                self.clock.build_ponder_go_args(
                    self.config.time.margin_msec,
                    self.my_color,
                    my_estimated_ms
                )
            );
            self.engine.go_ponder(&ponder_pos_cmd, &ponder_go)?;
            self.ponder_state = Some(PonderState {
                expected_usi: ponder_mv.clone(),
            });
        }

        // サーバーからのエコー（自分の手）を受信
        loop {
            match self.conn.recv_move()? {
                Some(RecvEvent::Move(sm)) => {
                    self.clock.consume(self.my_color, sm.time_sec);
                    self.record.update_last_time(sm.time_sec);
                    return Ok(None); // 正常: 外側ループへ戻る
                }
                Some(RecvEvent::GameEnd(result, msg, reason)) => {
                    log::info!("[CSA] 対局終了(エコー待ち中): {msg}");
                    cleanup_ponder(self.engine, &mut self.ponder_state)?;
                    self.record.set_result(&record_result_with_reason(&result, &reason));
                    self.engine.gameover(&gameover_str(&result))?;
                    return Ok(Some((result, self.record.clone())));
                }
                None => {
                    self.conn
                        .maybe_send_keepalive(self.config.server.keepalive.ping_interval_sec)?;
                    if self.shutdown.load(Ordering::SeqCst) {
                        let result = resign_and_wait(
                            self.conn,
                            self.engine,
                            &mut self.ponder_state,
                            &mut self.record,
                        )?;
                        return Ok(Some((result, self.record.clone())));
                    }
                }
            }
        }
    }
}

/// 1回の対局セッションを実行する
pub fn run_game_session(
    conn: &mut CsaConnection,
    engine: &mut UsiEngine,
    config: &CsaClientConfig,
    shutdown: &AtomicBool,
) -> Result<(GameResult, GameRecord)> {
    let summary = conn.recv_game_summary(config.server.keepalive.ping_interval_sec)?;
    conn.agree_and_wait_start(&summary.game_id)?;
    engine.new_game()?;

    let mut s = SessionState {
        pos: summary.position.clone(),
        initial_sfen: summary.position.to_sfen(),
        usi_moves: Vec::new(),
        clock: Clock::from_summary(&summary),
        record: GameRecord::new(&summary),
        ponder_state: None,
        my_color: summary.my_color,
        conn,
        engine,
        config,
        shutdown,
    };

    // 途中局面の手順を適用
    let mut move_color = summary.position.side_to_move;
    for cm in &summary.initial_moves {
        let usi = csa_move_to_usi(&cm.mv, &s.pos)?;
        s.pos.apply_csa_move(&cm.mv)?;
        s.usi_moves.push(usi);
        if let Some(t) = cm.time_sec {
            s.clock.consume(move_color, t);
        }
        s.record.add_move(&cm.mv, cm.time_sec.unwrap_or(0), None, move_color);
        move_color = opposite(move_color);
    }

    // 対局メインループ
    loop {
        if s.pos.side_to_move == s.my_color {
            // 自手番: 探索して指す
            let turn_start = Instant::now();
            let position_cmd = build_position_cmd(&s.initial_sfen, &s.usi_moves);
            let go_cmd =
                format!("go {}", s.clock.build_go_args(s.config.time.margin_msec, s.my_color));
            let (result, info) = s.engine.go(&position_cmd, &go_cmd, s.shutdown)?;

            if let Some(end) = s.send_bestmove_and_wait_echo(&result, &info, turn_start)? {
                return Ok(end);
            }
        }

        // 相手の手番: サーバーから指し手を待つ
        loop {
            match s.conn.recv_move()? {
                Some(RecvEvent::Move(sm)) => {
                    if let Some(ps) = s.ponder_state.take() {
                        let opponent_usi = csa_move_to_usi(&sm.mv, &s.pos)?;
                        if opponent_usi == ps.expected_usi {
                            // ponderhit
                            log::debug!("[PONDER] ponderhit: {}", opponent_usi);
                            s.pos.apply_csa_move(&sm.mv)?;
                            s.usi_moves.push(opponent_usi);
                            s.clock.consume(opposite(s.my_color), sm.time_sec);
                            s.record.add_move(&sm.mv, sm.time_sec, None, opposite(s.my_color));

                            let ponderhit_start = Instant::now();
                            let (result, info) = s.engine.ponderhit(s.shutdown)?;

                            if let Some(end) =
                                s.send_bestmove_and_wait_echo(&result, &info, ponderhit_start)?
                            {
                                return Ok(end);
                            }
                            break; // 外側ループへ
                        } else {
                            // ponder 外れ
                            log::debug!(
                                "[PONDER] miss: expected={} actual={}",
                                ps.expected_usi,
                                opponent_usi
                            );
                            s.engine.stop_and_wait()?;
                            s.pos.apply_csa_move(&sm.mv)?;
                            s.usi_moves.push(opponent_usi);
                            s.clock.consume(opposite(s.my_color), sm.time_sec);
                            s.record.add_move(&sm.mv, sm.time_sec, None, opposite(s.my_color));
                            break;
                        }
                    } else {
                        // ponder なし
                        let opponent_usi = csa_move_to_usi(&sm.mv, &s.pos)?;
                        s.pos.apply_csa_move(&sm.mv)?;
                        s.usi_moves.push(opponent_usi);
                        s.clock.consume(opposite(s.my_color), sm.time_sec);
                        s.record.add_move(&sm.mv, sm.time_sec, None, opposite(s.my_color));
                        break;
                    }
                }
                Some(RecvEvent::GameEnd(result, msg, reason)) => {
                    log::info!("[CSA] 対局終了: {msg}");
                    cleanup_ponder(s.engine, &mut s.ponder_state)?;
                    s.record.set_result(&record_result_with_reason(&result, &reason));
                    s.engine.gameover(&gameover_str(&result))?;
                    return Ok((result, s.record));
                }
                None => {
                    s.conn.maybe_send_keepalive(s.config.server.keepalive.ping_interval_sec)?;
                    if s.shutdown.load(Ordering::SeqCst) {
                        let result =
                            resign_and_wait(s.conn, s.engine, &mut s.ponder_state, &mut s.record)?;
                        return Ok((result, s.record));
                    }
                }
            }
        }
    }
}

struct PonderState {
    expected_usi: String,
}

fn cleanup_ponder(engine: &mut UsiEngine, ponder_state: &mut Option<PonderState>) -> Result<()> {
    if ponder_state.take().is_some() {
        engine.stop_and_wait()?;
    }
    Ok(())
}

fn resign_and_wait(
    conn: &mut CsaConnection,
    engine: &mut UsiEngine,
    ponder_state: &mut Option<PonderState>,
    record: &mut GameRecord,
) -> Result<GameResult> {
    log::info!("シャットダウン: 投了して終了します...");
    cleanup_ponder(engine, ponder_state)?;
    conn.send_resign()?;
    record.set_result("resign");
    let (result, _) = wait_game_end(conn)?;
    engine.gameover(&gameover_str(&result))?;
    Ok(result)
}

fn opposite(color: Color) -> Color {
    match color {
        Color::Black => Color::White,
        Color::White => Color::Black,
    }
}

fn gameover_str(result: &GameResult) -> String {
    match result {
        GameResult::Win => "win".to_string(),
        GameResult::Lose => "lose".to_string(),
        GameResult::Draw => "draw".to_string(),
        _ => "draw".to_string(),
    }
}

fn record_result_with_reason(result: &GameResult, reason: &Option<String>) -> String {
    if let Some(r) = reason {
        if r.contains("TIME_UP") {
            return "time_up".to_string();
        }
        if r.contains("ILLEGAL") {
            return "illegal_move".to_string();
        }
        if r.contains("MAX_MOVES") {
            return "max_moves".to_string();
        }
        if r.contains("JISHOGI") {
            return "jishogi".to_string();
        }
        if r.contains("SENNICHITE") {
            return "sennichite".to_string();
        }
    }
    match result {
        GameResult::Win => "win".to_string(),
        GameResult::Lose => "lose".to_string(),
        GameResult::Draw => "sennichite".to_string(),
        GameResult::Interrupted => "interrupted".to_string(),
        GameResult::Censored => "interrupted".to_string(),
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
    let base = if initial_sfen == HIRATE_SFEN {
        "position startpos".to_string()
    } else {
        format!("position sfen {initial_sfen}")
    };
    if usi_moves.is_empty() {
        format!("{base} moves {ponder_move}")
    } else {
        format!("{base} moves {} {ponder_move}", usi_moves.join(" "))
    }
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

    let mut comment = format!("'* {score}");

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

fn wait_game_end(conn: &mut CsaConnection) -> Result<(GameResult, Option<String>)> {
    let start = Instant::now();
    const TIMEOUT: Duration = Duration::from_secs(30);
    loop {
        if start.elapsed() >= TIMEOUT {
            log::warn!("[CSA] 終局結果の受信タイムアウト ({}秒)", TIMEOUT.as_secs());
            return Ok((GameResult::Interrupted, None));
        }
        match conn.recv_move()? {
            Some(RecvEvent::GameEnd(result, msg, reason)) => {
                log::info!("[CSA] 対局終了: {msg}");
                return Ok((result, reason));
            }
            Some(RecvEvent::Move(_)) => {}
            None => {}
        }
    }
}
