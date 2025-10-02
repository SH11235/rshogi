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
        for i in 1..size {
            values[i] = (i as f32).ln();
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
        for i in 1..MOVENO_LOG_TABLE_SIZE {
            values[i] = (i as f32).ln();
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

pub(crate) fn late_move_reduction(
    heur: &mut Heuristics,
    depth: i32,
    moveno: usize,
    is_quiet: bool,
    is_good_capture: bool,
    is_pv: bool,
    gives_check: bool,
    static_eval: i32,
    ply: u32,
    stack: &[SearchStack],
) -> i32 {
    if depth < 3 || moveno < 3 || !is_quiet || is_good_capture {
        return 0;
    }
    heur.lmr_trials = heur.lmr_trials.saturating_add(1);
    let depth_ln = ln_depth(depth);
    let moveno_ln = ln_moveno(moveno);
    let rd = ((depth_ln * moveno_ln) / dynp::lmr_k_coeff()).floor() as i32;
    let mut r = rd.max(1);
    if is_pv {
        r -= 1;
    }
    if gives_check {
        r = 0;
    }
    let improving = if ply >= 2 {
        if let Some(prev2) = stack[ply as usize - 2].static_eval {
            static_eval >= prev2 - 10
        } else {
            false
        }
    } else {
        false
    };
    if improving {
        r -= 1;
    }
    r.clamp(0, depth - 1)
}
