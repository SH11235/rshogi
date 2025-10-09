use std::time::{Duration, Instant};

use smallvec::SmallVec;

use crate::search::types::SearchStack;
use crate::search::SearchLimits;
use crate::Position;

use super::driver::ClassicBackend;
use super::ordering::{EvalMoveGuard, Heuristics};
use super::pvs::{ABArgs, SearchContext};

impl<E: crate::evaluation::evaluate::Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    /// Maximum PV length for UI/logging (keep in sync with TT reconstruction)
    const PV_MAX_LEN: usize = 32;
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

        pv_vec.truncate(Self::PV_MAX_LEN);
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
        let mut stack_buf = super::driver::take_stack_cache();
        let stack: &mut [SearchStack] = &mut stack_buf[..];
        let mut heur = Heuristics::default();
        let mut _tt_hits: u64 = 0;
        let mut _beta_cuts: u64 = 0;
        let mut _lmr_counter: u64 = 0;

        let mut first_used = false;
        let t0 = Instant::now();
        // Compute a conservative wall-clock micro budget (cap) and respect TM/fallback/time_limit
        let cap_ms: u64 = std::env::var("SHOGI_PVEXTRACT_CAP_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v > 0 && v <= 20)
            .unwrap_or(6);
        let mut deadline: Option<Instant> = Some(t0 + Duration::from_millis(cap_ms));
        if let Some(tm) = limits.time_manager.as_ref() {
            let elapsed = tm.elapsed_ms();
            let mut rem = u64::MAX;
            let soft = tm.soft_limit_ms();
            let hard = tm.hard_limit_ms();
            if soft != u64::MAX {
                rem = rem.min(soft.saturating_sub(elapsed));
            }
            if hard != u64::MAX {
                rem = rem.min(hard.saturating_sub(elapsed));
            }
            if rem != u64::MAX {
                let cand = t0 + Duration::from_millis(rem);
                deadline = Some(deadline.map(|d| d.min(cand)).unwrap_or(cand));
            }
        }
        if let Some(dl) = limits.fallback_deadlines {
            let elapsed = limits.start_time.elapsed().as_millis() as u64;
            let mut rem = u64::MAX;
            if dl.soft_limit_ms > 0 {
                rem = rem.min(dl.soft_limit_ms.saturating_sub(elapsed));
            }
            if dl.hard_limit_ms > 0 {
                rem = rem.min(dl.hard_limit_ms.saturating_sub(elapsed));
            }
            if rem != u64::MAX {
                let cand = t0 + Duration::from_millis(rem);
                deadline = Some(deadline.map(|d| d.min(cand)).unwrap_or(cand));
            }
        }
        if let Some(limit) = limits.time_limit() {
            let cand = t0 + limit;
            deadline = Some(deadline.map(|d| d.min(cand)).unwrap_or(cand));
        }
        // Keep evaluator in sync with `pos` while we descend the PV.
        let mut guard_stack: SmallVec<[EvalMoveGuard<'_, E>; 32]> = SmallVec::new();
        while d > 0 {
            // Keep PV length aligned with TT reconstruction (32 moves max)
            if pv.len() >= Self::PV_MAX_LEN {
                break;
            }
            if ClassicBackend::<E>::should_stop(limits) {
                break;
            }
            if let Some(dl) = deadline {
                if Instant::now() >= dl {
                    break;
                }
            }
            let mv = if !first_used {
                first
            } else {
                let mut qnodes = 0_u64;
                let step_depth = d.min(2);
                let qnodes_limit = Self::compute_qnodes_limit(limits, step_depth, 1);
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
                // Use the shallow step depth decided above
                let (_sc, mv_opt) = self.alphabeta(
                    ABArgs {
                        pos: &pos,
                        depth: step_depth,
                        alpha: i32::MIN / 2,
                        beta: i32::MAX / 2,
                        ply: 0,
                        is_pv: true,
                        stack,
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
            {
                let guard = EvalMoveGuard::new(self.evaluator.as_ref(), &pos, mv);
                pos.do_move(mv);
                guard_stack.push(guard);
            }
            d -= 1;
        }
        // Drop guards to return evaluator to the root position.
        drop(guard_stack);
        super::driver::return_stack_cache(stack_buf);
        pv
    }
}
