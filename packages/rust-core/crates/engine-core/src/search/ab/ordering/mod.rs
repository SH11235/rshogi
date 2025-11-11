pub mod constants;
mod guards;
mod move_picker;

use std::fmt;
use std::sync::OnceLock;

use crate::search::history::{
    ButterflyHistory, CaptureHistory, ContinuationHistory, ContinuationKey, CounterMoveHistory,
    PawnHistory,
};
use crate::search::params as dynp;
use crate::search::types::SearchStack;
use crate::shogi::board::Piece;
use crate::shogi::piece_constants::SEE_PIECE_VALUES;
use crate::shogi::{Move, PieceType};
use crate::Position;
pub(crate) use guards::{EvalMoveGuard, EvalNullGuard};
#[cfg(any(test, feature = "bench-move-picker"))]
pub use move_picker::MovePicker;
#[cfg(not(any(test, feature = "bench-move-picker")))]
pub(crate) use move_picker::MovePicker;

const MOVENO_LOG_TABLE_SIZE: usize = 512;

static DEPTH_LOG_TABLE: OnceLock<Vec<f32>> = OnceLock::new();
static MOVENO_LOG_TABLE: OnceLock<Vec<f32>> = OnceLock::new();

#[inline]
fn ln_depth(depth: i32) -> f32 {
    if depth <= 0 {
        return 0.0;
    }
    let idx = depth as usize;
    let table = DEPTH_LOG_TABLE.get_or_init(|| {
        let size = crate::search::constants::MAX_PLY + 2;
        let mut values = vec![0.0f32; size];
        for (i, value) in values.iter_mut().enumerate().skip(1) {
            *value = (i as f32).ln();
        }
        values
    });
    if idx < table.len() {
        table[idx]
    } else {
        (idx as f32).ln()
    }
}

#[inline]
fn ln_moveno(moveno: usize) -> f32 {
    if moveno == 0 {
        return 0.0;
    }
    let table = MOVENO_LOG_TABLE.get_or_init(|| {
        let mut values = vec![0.0f32; MOVENO_LOG_TABLE_SIZE];
        for (i, value) in values.iter_mut().enumerate().skip(1) {
            *value = (i as f32).ln();
        }
        values
    });
    if moveno < table.len() {
        table[moveno]
    } else {
        (moveno as f32).ln()
    }
}

#[derive(Clone, Default)]
pub struct Heuristics {
    pub(crate) history: ButterflyHistory,
    pub(crate) counter: CounterMoveHistory,
    pub(crate) continuation: ContinuationHistory,
    pub(crate) capture: CaptureHistory,
    pub(crate) pawn_history: PawnHistory,
    pub(crate) lmr_trials: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HeuristicsSummary {
    pub quiet_max: i16,
    pub continuation_max: i16,
    pub capture_max: i16,
    pub pawn_history_max: i16,
    pub counter_filled: u32,
    pub lmr_trials: u64,
}

impl Heuristics {
    pub fn age_all(&mut self) {
        self.history.age_scores();
        self.continuation.age_scores();
        self.capture.age_scores();
        self.pawn_history.age_scores();
        // Counter movesは age 概念が薄いためリセットのみ行う場合は別途検討
    }

    pub fn clear_all(&mut self) {
        self.history.clear();
        self.counter.clear();
        self.continuation.clear();
        self.capture.clear();
        self.pawn_history.clear();
        self.lmr_trials = 0;
    }

    pub fn merge_from(&mut self, other: &Self) {
        self.history.merge_from(&other.history);
        self.counter.merge_from(&other.counter);
        self.continuation.merge_from(&other.continuation);
        self.capture.merge_from(&other.capture);
        self.pawn_history.merge_from(&other.pawn_history);
        self.lmr_trials = self.lmr_trials.saturating_add(other.lmr_trials);
    }

    pub fn summary(&self) -> HeuristicsSummary {
        HeuristicsSummary {
            quiet_max: self.history.max_abs(),
            continuation_max: self.continuation.max_abs(),
            capture_max: self.capture.max_abs(),
            pawn_history_max: self.pawn_history.max_abs(),
            counter_filled: self.counter.filled_count(),
            lmr_trials: self.lmr_trials,
        }
    }

    pub fn lmr_trials(&self) -> u64 {
        self.lmr_trials
    }
}

impl fmt::Debug for Heuristics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let summary = self.summary();
        f.debug_struct("Heuristics")
            .field("lmr_trials", &self.lmr_trials)
            .field("quiet_max", &summary.quiet_max)
            .field("continuation_max", &summary.continuation_max)
            .field("capture_max", &summary.capture_max)
            .field("pawn_history_max", &summary.pawn_history_max)
            .field("counter_filled", &summary.counter_filled)
            .finish()
    }
}

pub(crate) struct LateMoveReductionParams<'stack> {
    pub lmr_trials: &'stack mut u64,
    pub depth: i32,
    pub moveno: usize,
    pub is_quiet: bool,
    pub is_good_capture: bool,
    pub is_pv: bool,
    pub gives_check: bool,
    pub static_eval: i32,
    pub ply: u32,
    pub stack: &'stack [SearchStack],
}

pub(crate) fn late_move_reduction(params: LateMoveReductionParams<'_>) -> i32 {
    if params.depth < 3 || params.moveno < 3 || !params.is_quiet || params.is_good_capture {
        return 0;
    }
    *params.lmr_trials = params.lmr_trials.saturating_add(1);
    let depth_ln = ln_depth(params.depth);
    let moveno_ln = ln_moveno(params.moveno);
    // 深さ依存の係数（浅層では LMR を弱める）
    let rd = ((depth_ln * moveno_ln) / dynp::lmr_k_coeff_for_depth(params.depth)).floor() as i32;
    let mut r = rd.max(1);
    if params.is_pv {
        r -= 1;
    }
    if params.gives_check {
        r = 0;
    }
    let improving = if params.ply >= 2 {
        let idx = (params.ply - 2) as usize;
        if let Some(prev2) = params.stack[idx].static_eval {
            params.static_eval >= prev2 - 10
        } else {
            false
        }
    } else {
        false
    };
    if improving {
        r -= 1;
    }
    r.clamp(0, params.depth - 1)
}

fn infer_piece_type(pos: &Position, mv: Move, prefer_from: bool) -> Option<PieceType> {
    if let Some(pt) = mv.piece_type() {
        return Some(pt);
    }
    if prefer_from {
        if let Some(from) = mv.from() {
            if let Some(piece) = pos.board.piece_on(from) {
                return Some(piece.piece_type);
            }
        }
    }
    pos.board.piece_on(mv.to()).map(|p: Piece| p.piece_type)
}

fn infer_captured_piece(pos: &Position, mv: Move) -> Option<PieceType> {
    if let Some(pt) = mv.captured_piece_type() {
        return Some(pt);
    }
    pos.board.piece_on(mv.to()).map(|p: Piece| p.piece_type)
}

fn continuation_score(heur: &Heuristics, pos: &Position, mv: Move, prev_move: Option<Move>) -> i32 {
    if let Some(prev_mv) = prev_move {
        if let (Some(prev_piece), Some(curr_piece)) =
            (infer_piece_type(pos, prev_mv, false), infer_piece_type(pos, mv, true))
        {
            let key = ContinuationKey::new(
                pos.side_to_move,
                prev_piece as usize,
                prev_mv.to(),
                prev_mv.is_drop(),
                curr_piece as usize,
                mv.to(),
                mv.is_drop(),
            );
            return heur.continuation.get(key);
        }
    }
    0
}

/// YO準拠の statScore（履歴ベースの手の良さ指標）
pub(crate) fn stat_score(
    heur: &Heuristics,
    pos: &Position,
    mv: Move,
    prev_move: Option<Move>,
    prev2_move: Option<Move>,
    is_capture: bool,
) -> i32 {
    let stm = pos.side_to_move;
    if is_capture {
        if let (Some(attacker), Some(victim)) =
            (infer_piece_type(pos, mv, true), infer_captured_piece(pos, mv))
        {
            let mut score = heur.capture.get(stm, attacker, victim, mv.to());
            score += SEE_PIECE_VALUES[0][victim as usize];
            return score;
        }
        return 0;
    }

    let mut score = heur.history.get(stm, mv);
    if let Some(curr_piece) = infer_piece_type(pos, mv, true) {
        score += heur.pawn_history.get(stm, curr_piece, mv.to());
    }
    score += continuation_score(heur, pos, mv, prev_move);
    score += continuation_score(heur, pos, mv, prev2_move);
    score
}
