mod guards;

use std::sync::OnceLock;

use crate::search::history::{ButterflyHistory, CounterMoveHistory};
use crate::search::params as dynp;
use crate::search::types::SearchStack;

pub(crate) use guards::{EvalMoveGuard, EvalNullGuard};

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

#[derive(Default)]
pub(crate) struct Heuristics {
    pub(crate) history: ButterflyHistory,
    pub(crate) counter: CounterMoveHistory,
    pub(crate) lmr_trials: u64,
}

pub(crate) struct LateMoveReductionParams<'heur, 'stack> {
    pub heur: &'heur mut Heuristics,
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

pub(crate) fn late_move_reduction(params: LateMoveReductionParams<'_, '_>) -> i32 {
    if params.depth < 3 || params.moveno < 3 || !params.is_quiet || params.is_good_capture {
        return 0;
    }
    params.heur.lmr_trials = params.heur.lmr_trials.saturating_add(1);
    let depth_ln = ln_depth(params.depth);
    let moveno_ln = ln_moveno(params.moveno);
    let rd = ((depth_ln * moveno_ln) / dynp::lmr_k_coeff()).floor() as i32;
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
