mod guards;

use crate::search::history::{ButterflyHistory, CounterMoveHistory};
use crate::search::params as dynp;
use crate::search::types::SearchStack;

pub(crate) use guards::{EvalMoveGuard, EvalNullGuard};

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
    let rd = ((depth as f32).ln() * (moveno as f32).ln() / dynp::lmr_k_coeff()).floor() as i32;
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
