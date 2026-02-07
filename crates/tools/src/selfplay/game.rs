use anyhow::Result;
use rshogi_core::movegen::is_legal_with_pass;
use rshogi_core::types::{Color, Move};

use super::engine::EngineProcess;
use super::position::{build_position, ParsedPosition};
use super::time_control::TimeControl;
use super::types::{EvalLog, GameOutcome, InfoCallback, SearchRequest};

/// ゲーム設定
pub struct GameConfig {
    pub max_moves: u32,
    pub timeout_margin_ms: u64,
    /// パス権利の初期値 (先手, 後手)。None の場合はパス権なし。
    pub pass_rights: Option<(u8, u8)>,
}

/// 1手ごとに呼ばれるイベント
pub struct MoveEvent {
    pub ply: u32,
    pub side: Color,
    pub sfen_before: String,
    pub move_usi: String,
    /// エンジンが返した生の指し手文字列（合法手に正規化される前）
    pub raw_move_usi: Option<String>,
    pub elapsed_ms: u64,
    pub think_limit_ms: u64,
    pub timed_out: bool,
    pub eval: Option<EvalLog>,
    pub engine_label: String,
}

/// 対局結果
pub struct GameResult {
    pub outcome: GameOutcome,
    pub reason: String,
    pub plies: u32,
}

/// 1局を実行する。
///
/// - `black`, `white`: エンジンプロセス（事前に spawn 済み）
/// - `start_pos`: 開始局面
/// - `tc`: 時間管理（コピーして内部で更新）
/// - `config`: ゲーム設定
/// - `game_id`: ゲーム番号
/// - `on_move`: 1手ごとに呼ばれるコールバック
/// - `info_cb`: info行受信時のコールバック（None ならスキップ）
pub fn run_game(
    black: &mut EngineProcess,
    white: &mut EngineProcess,
    start_pos: &ParsedPosition,
    tc: TimeControl,
    config: &GameConfig,
    game_id: u32,
    on_move: &mut dyn FnMut(&MoveEvent),
    mut info_cb: Option<Box<InfoCallback<'_>>>,
) -> Result<GameResult> {
    let pass_black = config.pass_rights.map(|(b, _)| b);
    let pass_white = config.pass_rights.map(|(_, w)| w);
    let mut pos = build_position(start_pos, pass_black, pass_white)?;
    let mut tc = tc;
    let mut outcome = GameOutcome::InProgress;
    let mut outcome_reason = "max_moves".to_string();
    let mut plies_played = 0u32;

    let pass_rights_enabled = config.pass_rights.is_some();

    for ply_idx in 0..config.max_moves {
        plies_played = ply_idx + 1;
        let side = pos.side_to_move();
        let engine = if side == Color::Black {
            &mut *black
        } else {
            &mut *white
        };
        let engine_label = engine.label.clone();
        let sfen_before = pos.to_sfen();
        let think_limit_ms = tc.think_limit_ms(side);
        let pass_rights = if pass_rights_enabled {
            Some((pos.pass_rights(Color::Black), pos.pass_rights(Color::White)))
        } else {
            None
        };
        let req = SearchRequest {
            sfen: &sfen_before,
            time_args: tc.time_args(),
            think_limit_ms,
            timeout_margin_ms: config.timeout_margin_ms,
            game_id,
            ply: plies_played,
            side,
            engine_label: engine_label.clone(),
            pass_rights,
        };
        let cb = info_cb.as_mut().map(|b| b.as_mut() as &mut dyn FnMut(&str, &SearchRequest<'_>));
        let search = engine.search(&req, cb)?;

        let timed_out = search.timed_out;
        let mut move_usi = search.bestmove.clone().unwrap_or_else(|| "none".to_string());
        let mut raw_move_usi = None;
        let mut terminal = false;
        let elapsed_ms = search.elapsed_ms;
        let eval_log = search.eval.clone();

        if timed_out {
            outcome = if side == Color::Black {
                GameOutcome::WhiteWin
            } else {
                GameOutcome::BlackWin
            };
            outcome_reason = "timeout".to_string();
            terminal = true;
            if search.bestmove.is_none() {
                move_usi = "timeout".to_string();
            }
        } else if let Some(ref mv_str) = search.bestmove {
            raw_move_usi = Some(mv_str.clone());
            match mv_str.as_str() {
                "resign" => {
                    move_usi = mv_str.clone();
                    outcome = if side == Color::Black {
                        GameOutcome::WhiteWin
                    } else {
                        GameOutcome::BlackWin
                    };
                    outcome_reason = "resign".to_string();
                    terminal = true;
                }
                "win" => {
                    move_usi = mv_str.clone();
                    outcome = if side == Color::Black {
                        GameOutcome::BlackWin
                    } else {
                        GameOutcome::WhiteWin
                    };
                    outcome_reason = "win".to_string();
                    terminal = true;
                }
                _ => match Move::from_usi(mv_str) {
                    Some(mv) if is_legal_with_pass(&pos, mv) => {
                        let gives_check = if mv.is_pass() {
                            false
                        } else {
                            pos.gives_check(mv)
                        };
                        pos.do_move(mv, gives_check);
                        tc.update_after_move(side, search.elapsed_ms);
                        move_usi = mv_str.clone();
                        raw_move_usi = None;
                    }
                    _ => {
                        outcome = if side == Color::Black {
                            GameOutcome::WhiteWin
                        } else {
                            GameOutcome::BlackWin
                        };
                        outcome_reason = "illegal_move".to_string();
                        terminal = true;
                        move_usi = "illegal".to_string();
                    }
                },
            }
        } else {
            outcome = if side == Color::Black {
                GameOutcome::WhiteWin
            } else {
                GameOutcome::BlackWin
            };
            outcome_reason = "no_bestmove".to_string();
            terminal = true;
        }

        let event = MoveEvent {
            ply: plies_played,
            side,
            sfen_before,
            move_usi,
            raw_move_usi,
            elapsed_ms,
            think_limit_ms,
            timed_out,
            eval: eval_log,
            engine_label,
        };
        on_move(&event);

        if terminal || outcome != GameOutcome::InProgress {
            break;
        }
    }

    if outcome == GameOutcome::InProgress {
        outcome = GameOutcome::Draw;
        outcome_reason = "max_moves".to_string();
    }

    Ok(GameResult {
        outcome,
        reason: outcome_reason,
        plies: plies_played,
    })
}
