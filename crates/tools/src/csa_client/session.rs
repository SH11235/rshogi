//! CSA対局セッション管理
//!
//! 1回の対局（ログイン〜対局〜終局）を管理する。

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use crate::common::csa::{Color, Position, csa_move_to_usi, usi_move_to_csa};

use super::config::CsaClientConfig;
use super::engine::{SearchInfo, UsiEngine};
use super::protocol::{CsaConnection, GameResult, GameSummary, RecvEvent};
use super::record::GameRecord;

/// 対局中の時間管理
struct Clock {
    black_time_ms: i64,
    white_time_ms: i64,
    byoyomi_ms: i64,
    increment_ms: i64,
}

impl Clock {
    fn from_summary(summary: &GameSummary) -> Self {
        let total_ms = summary.total_time_sec as i64 * 1000;
        Self {
            black_time_ms: total_ms,
            white_time_ms: total_ms,
            byoyomi_ms: summary.byoyomi_sec as i64 * 1000,
            increment_ms: summary.increment_sec as i64 * 1000,
        }
    }

    fn consume(&mut self, color: Color, time_sec: u32) {
        let consumed_ms = time_sec as i64 * 1000;
        match color {
            Color::Black => {
                self.black_time_ms = (self.black_time_ms - consumed_ms + self.increment_ms).max(0);
            }
            Color::White => {
                self.white_time_ms = (self.white_time_ms - consumed_ms + self.increment_ms).max(0);
            }
        }
    }

    /// USI go コマンドの時間引数を構築する
    fn build_go_args(&self, margin_msec: u64) -> String {
        let btime = self.black_time_ms.max(0);
        let wtime = self.white_time_ms.max(0);

        if self.increment_ms > 0 {
            // フィッシャー
            format!(
                "btime {} wtime {} binc {} winc {}",
                btime, wtime, self.increment_ms, self.increment_ms
            )
        } else if self.byoyomi_ms > 0 {
            let byoyomi = (self.byoyomi_ms - margin_msec as i64).max(0);
            format!("btime {} wtime {} byoyomi {}", btime, wtime, byoyomi)
        } else {
            format!("btime {} wtime {}", btime, wtime)
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
    // 対局情報受信
    let summary = conn.recv_game_summary()?;
    conn.agree_and_wait_start(&summary.game_id)?;

    engine.new_game()?;

    // 内部盤面の構築
    let mut pos = summary.position.clone();
    let initial_sfen = pos.to_sfen();
    let mut usi_moves: Vec<String> = Vec::new();
    let mut clock = Clock::from_summary(&summary);

    // 途中局面の手順を適用（手番は初期局面の side_to_move から追跡）
    let mut move_color = summary.position.side_to_move;
    for cm in &summary.initial_moves {
        let usi = csa_move_to_usi(&cm.mv, &pos)?;
        pos.apply_csa_move(&cm.mv)?;
        usi_moves.push(usi);
        if let Some(t) = cm.time_sec {
            clock.consume(move_color, t);
        }
        move_color = opposite(move_color);
    }

    // 棋譜記録
    let mut record = GameRecord::new(&summary);
    for cm in &summary.initial_moves {
        record.add_move(&cm.mv, cm.time_sec.unwrap_or(0), None);
    }

    let my_color = summary.my_color;
    let mut ponder_state: Option<PonderState> = None;

    // 自分の手番かどうか判定
    let is_my_turn = |side: Color| side == my_color;

    // 対局メインループ
    loop {
        if is_my_turn(pos.side_to_move) {
            // 自手番: 探索して指す
            let position_cmd = build_position_cmd(&initial_sfen, &usi_moves);
            let go_cmd = format!("go {}", clock.build_go_args(config.time.margin_msec));

            let (result, info) = engine.go(&position_cmd, &go_cmd)?;

            if result.bestmove == "resign" {
                conn.send_resign()?;
                record.set_result("resign");
                // 終局を待つ
                let game_result = wait_game_end(conn)?;
                return Ok((game_result, record));
            }
            if result.bestmove == "win" {
                conn.send_win()?;
                record.set_result("win_declaration");
                let game_result = wait_game_end(conn)?;
                return Ok((game_result, record));
            }

            // USI手 → CSA手
            let csa_move = usi_move_to_csa(&result.bestmove, &pos)?;

            // floodgate コメント
            let comment = if config.server.floodgate {
                Some(build_floodgate_comment(&info, my_color, &pos, &usi_moves))
            } else {
                None
            };

            conn.send_move_with_comment(&csa_move, comment.as_deref())?;

            // 盤面更新
            pos.apply_csa_move(&csa_move)?;
            usi_moves.push(result.bestmove.clone());
            record.add_move(&csa_move, 0, Some(&info)); // 消費時間はサーバーエコーで確定

            // ponder 開始
            if config.game.ponder
                && let Some(ref ponder_mv) = result.ponder_move
            {
                let ponder_pos_cmd =
                    build_position_cmd_with_ponder(&initial_sfen, &usi_moves, ponder_mv);
                let ponder_go =
                    format!("go ponder {}", clock.build_go_args(config.time.margin_msec));
                engine.go_ponder(&ponder_pos_cmd, &ponder_go)?;
                ponder_state = Some(PonderState {
                    expected_usi: ponder_mv.clone(),
                });
            }

            // サーバーからのエコー（自分の手）を受信
            loop {
                match conn.recv_move()? {
                    Some(RecvEvent::Move(sm)) => {
                        // 自分の手のエコー: 消費時間を更新
                        clock.consume(my_color, sm.time_sec);
                        record.update_last_time(sm.time_sec);
                        break;
                    }
                    Some(RecvEvent::GameEnd(result, msg)) => {
                        log::info!("[CSA] 対局終了(エコー待ち中): {msg}");
                        cleanup_ponder(engine, &mut ponder_state)?;
                        engine.gameover(&gameover_str(&result))?;
                        return Ok((result, record));
                    }
                    None => {
                        conn.maybe_send_keepalive(config.server.keepalive.ping_interval_sec)?;
                        if shutdown.load(Ordering::SeqCst) {
                            let result =
                                resign_and_wait(conn, engine, &mut ponder_state, &mut record)?;
                            return Ok((result, record));
                        }
                    }
                }
            }
        }

        // 相手の手番: サーバーから指し手を待つ
        loop {
            match conn.recv_move()? {
                Some(RecvEvent::Move(sm)) => {
                    // ponder 中の場合
                    if let Some(ps) = ponder_state.take() {
                        let opponent_usi = csa_move_to_usi(&sm.mv, &pos)?;
                        if opponent_usi == ps.expected_usi {
                            // ponderhit
                            log::debug!("[PONDER] ponderhit: {}", opponent_usi);
                            // 盤面更新（相手の手）
                            pos.apply_csa_move(&sm.mv)?;
                            usi_moves.push(opponent_usi);
                            clock.consume(opposite(my_color), sm.time_sec);
                            record.add_move(&sm.mv, sm.time_sec, None);

                            // ponderhit → bestmove を待つ
                            let (result, info) = engine.ponderhit()?;

                            if result.bestmove == "resign" {
                                conn.send_resign()?;
                                record.set_result("resign");
                                let game_result = wait_game_end(conn)?;
                                return Ok((game_result, record));
                            }
                            if result.bestmove == "win" {
                                conn.send_win()?;
                                record.set_result("win_declaration");
                                let game_result = wait_game_end(conn)?;
                                return Ok((game_result, record));
                            }

                            let csa_move = usi_move_to_csa(&result.bestmove, &pos)?;
                            let comment = if config.server.floodgate {
                                Some(build_floodgate_comment(&info, my_color, &pos, &usi_moves))
                            } else {
                                None
                            };
                            conn.send_move_with_comment(&csa_move, comment.as_deref())?;

                            pos.apply_csa_move(&csa_move)?;
                            usi_moves.push(result.bestmove.clone());
                            record.add_move(&csa_move, 0, Some(&info));

                            // 次の ponder
                            if config.game.ponder
                                && let Some(ref ponder_mv) = result.ponder_move
                            {
                                let ponder_pos_cmd = build_position_cmd_with_ponder(
                                    &initial_sfen,
                                    &usi_moves,
                                    ponder_mv,
                                );
                                let ponder_go = format!(
                                    "go ponder {}",
                                    clock.build_go_args(config.time.margin_msec)
                                );
                                engine.go_ponder(&ponder_pos_cmd, &ponder_go)?;
                                ponder_state = Some(PonderState {
                                    expected_usi: ponder_mv.clone(),
                                });
                            }

                            // 自手エコーを受信
                            loop {
                                match conn.recv_move()? {
                                    Some(RecvEvent::Move(echo)) => {
                                        clock.consume(my_color, echo.time_sec);
                                        record.update_last_time(echo.time_sec);
                                        break;
                                    }
                                    Some(RecvEvent::GameEnd(result, msg)) => {
                                        log::info!("[CSA] 対局終了(エコー待ち中): {msg}");
                                        cleanup_ponder(engine, &mut ponder_state)?;
                                        engine.gameover(&gameover_str(&result))?;
                                        return Ok((result, record));
                                    }
                                    None => {
                                        conn.maybe_send_keepalive(
                                            config.server.keepalive.ping_interval_sec,
                                        )?;
                                        if shutdown.load(Ordering::SeqCst) {
                                            let result = resign_and_wait(
                                                conn,
                                                engine,
                                                &mut ponder_state,
                                                &mut record,
                                            )?;
                                            return Ok((result, record));
                                        }
                                    }
                                }
                            }
                            // ponderhit + 自手送信が完了、外側ループに戻る
                            break;
                        } else {
                            // ponder 外れ
                            log::debug!(
                                "[PONDER] miss: expected={} actual={}",
                                ps.expected_usi,
                                opponent_usi
                            );
                            engine.stop_and_wait()?;
                            // 相手の手を反映
                            pos.apply_csa_move(&sm.mv)?;
                            usi_moves.push(opponent_usi);
                            clock.consume(opposite(my_color), sm.time_sec);
                            record.add_move(&sm.mv, sm.time_sec, None);
                            break; // 外側ループに戻り、自手番の処理へ
                        }
                    } else {
                        // ponder なし: 通常の相手手受信
                        let opponent_usi = csa_move_to_usi(&sm.mv, &pos)?;
                        pos.apply_csa_move(&sm.mv)?;
                        usi_moves.push(opponent_usi);
                        clock.consume(opposite(my_color), sm.time_sec);
                        record.add_move(&sm.mv, sm.time_sec, None);
                        break; // 外側ループに戻り、自手番の処理へ
                    }
                }
                Some(RecvEvent::GameEnd(result, msg)) => {
                    log::info!("[CSA] 対局終了: {msg}");
                    cleanup_ponder(engine, &mut ponder_state)?;
                    engine.gameover(&gameover_str(&result))?;
                    return Ok((result, record));
                }
                None => {
                    // タイムアウト: keep-alive チェック
                    conn.maybe_send_keepalive(config.server.keepalive.ping_interval_sec)?;
                    if shutdown.load(Ordering::SeqCst) {
                        let result = resign_and_wait(conn, engine, &mut ponder_state, &mut record)?;
                        return Ok((result, record));
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

/// シャットダウン時に投了して対局終了を待つ
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
    let result = wait_game_end(conn)?;
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

/// Floodgate 形式のコメント行を構築する。
/// 形式: `'* <評価値> <PV手順（CSA形式）>`
fn build_floodgate_comment(
    info: &SearchInfo,
    my_color: Color,
    pos: &Position,
    _usi_moves: &[String],
) -> String {
    // 評価値（先手視点に正規化）
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

    // PV を CSA に変換（ベストエフォート）
    if !info.pv.is_empty() {
        // PV変換用に盤面を複製
        let mut pv_pos = pos.clone();
        // 現在の手順を再現（posは既にcsa_moveで更新済みの状態だが、
        // PVの変換には現在のposを使う）
        for usi_mv in &info.pv {
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

/// 対局終了を待つ
fn wait_game_end(conn: &mut CsaConnection) -> Result<GameResult> {
    loop {
        match conn.recv_move()? {
            Some(RecvEvent::GameEnd(result, msg)) => {
                log::info!("[CSA] 対局終了: {msg}");
                return Ok(result);
            }
            Some(RecvEvent::Move(_)) => {
                // 終局直前のエコー手は無視
            }
            None => {
                // タイムアウト: 少し待つ
            }
        }
    }
}
