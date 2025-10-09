use std::time::Instant;

use smallvec::SmallVec;

use crate::search::types::SearchStack;
use crate::search::SearchLimits;
use crate::Position;

use super::driver::ClassicBackend;
use super::ordering::{EvalMoveGuard, Heuristics};
use super::pvs::{ABArgs, SearchContext};

impl<E: crate::evaluation::evaluate::Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(crate) fn reconstruct_root_pv_from_tt(
        &self,
        root: &Position,
        depth: i32,
        first: crate::shogi::Move,
    ) -> Option<SmallVec<[crate::shogi::Move; 32]>> {
        let tt = self.tt.as_ref()?;
        if depth <= 0 {
            return None;
        }

        let mut pos = root.clone();
        let max_depth = depth.clamp(0, crate::search::constants::MAX_PLY as i32) as u8;
        let mut pv_vec = tt.reconstruct_pv_from_tt(&mut pos, max_depth);
        if pv_vec.is_empty() {
            return None;
        }

        if !pv_vec[0].equals_without_piece_type(&first) {
            return None;
        }

        pv_vec.truncate(32);
        Some(SmallVec::from_vec(pv_vec))
    }

    pub(crate) fn extract_pv(
        &self,
        root: &Position,
        depth: i32,
        first: crate::shogi::Move,
        limits: &SearchLimits,
        nodes: &mut u64,
    ) -> SmallVec<[crate::shogi::Move; 32]> {
        let mut pv: SmallVec<[crate::shogi::Move; 32]> = SmallVec::new();
        let mut pos = root.clone();
        let mut d = depth;
        let mut seldepth_dummy = 0u32;
        let mut stack = vec![SearchStack::default(); crate::search::constants::MAX_PLY + 1];
        let mut heur = Heuristics::default();
        let mut _tt_hits: u64 = 0;
        let mut _beta_cuts: u64 = 0;
        let mut _lmr_counter: u64 = 0;

        let mut first_used = false;
        let t0 = Instant::now();
        let mut guard_stack: SmallVec<[EvalMoveGuard<'_, E>; 32]> = SmallVec::new();
        while d > 0 {
            if ClassicBackend::<E>::should_stop(limits) {
                break;
            }
            if let Some(limit) = limits.time_limit() {
                if t0.elapsed() >= limit {
                    break;
                }
            }
            let mv = if !first_used {
                first
            } else {
                let mut qnodes = 0_u64;
                let qnodes_limit =
                    limits.qnodes_limit.unwrap_or(crate::search::constants::DEFAULT_QNODES_LIMIT);
                #[cfg(feature = "diagnostics")]
                let mut abdada_busy_detected: u64 = 0;
                #[cfg(feature = "diagnostics")]
                let mut abdada_busy_set: u64 = 0;

                let mut ctx = SearchContext {
                    limits,
                    start_time: &t0,
                    nodes,
                    seldepth: &mut seldepth_dummy,
                    qnodes: &mut qnodes,
                    qnodes_limit,
                    #[cfg(feature = "diagnostics")]
                    abdada_busy_detected: &mut abdada_busy_detected,
                    #[cfg(feature = "diagnostics")]
                    abdada_busy_set: &mut abdada_busy_set,
                };
                let (_sc, mv_opt) = self.alphabeta(
                    ABArgs {
                        pos: &pos,
                        depth: d,
                        alpha: i32::MIN / 2,
                        beta: i32::MAX / 2,
                        ply: 0,
                        is_pv: true,
                        stack: &mut stack,
                        heur: &mut heur,
                        tt_hits: &mut _tt_hits,
                        beta_cuts: &mut _beta_cuts,
                        lmr_counter: &mut _lmr_counter,
                    },
                    &mut ctx,
                );
                match mv_opt {
                    Some(m) => m,
                    None => break,
                }
            };
            first_used = true;
            pv.push(mv);
            let guard = EvalMoveGuard::new(self.evaluator.as_ref(), &pos, mv);
            guard_stack.push(guard);
            pos.do_move(mv);
            d -= 1;
        }
        while let Some(_guard) = guard_stack.pop() {}
        pv
    }
}
