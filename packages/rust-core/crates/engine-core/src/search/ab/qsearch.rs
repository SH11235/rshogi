use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::mate_score;
use crate::search::params::{
    qs_checks_enabled, QS_MARGIN_CAPTURE, QS_MAX_QUIET_CHECKS, QS_PROMOTE_BONUS,
};
use crate::Position;

use super::driver::ClassicBackend;
use super::ordering::EvalMoveGuard;
use super::pvs::SearchContext;

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(crate) fn qsearch(
        &self,
        pos: &Position,
        mut alpha: i32,
        beta: i32,
        ctx: &mut SearchContext,
        ply: u32,
    ) -> i32 {
        if (ply as u16) >= crate::search::constants::MAX_QUIESCE_DEPTH {
            return alpha;
        }
        if ctx.time_up() || Self::should_stop(ctx.limits) {
            return alpha;
        }
        ctx.tick(ply);

        if pos.is_in_check() {
            let mg = MoveGenerator::new();
            let Ok(list) = mg.generate_all(pos) else {
                return self.evaluator.evaluate(pos);
            };
            let mut has_legal = false;
            for mv in list.as_slice().iter().copied() {
                if !pos.is_legal_move(mv) {
                    continue;
                }
                has_legal = true;
                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    -self.qsearch(&child, -beta, -alpha, ctx, ply + 1)
                };
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
            }
            if !has_legal {
                return mate_score(ply as u8, false);
            }
            return alpha;
        }

        let stand_pat = self.evaluator.evaluate(pos);
        if stand_pat >= beta {
            return stand_pat;
        }
        if stand_pat > alpha {
            alpha = stand_pat;
        }

        let mg = MoveGenerator::new();
        let Ok(captures) = mg.generate_captures(pos) else {
            return alpha;
        };

        let mut caps: Vec<(crate::shogi::Move, i32)> =
            captures.as_slice().iter().copied().map(|m| (m, pos.see(m))).collect();
        caps.sort_unstable_by(|a, b| b.1.cmp(&a.1));

        for (mv, _see) in caps.iter().copied().filter(|&(_, s)| s >= 0) {
            let captured_val = mv
                .captured_piece_type()
                .map(|pt| crate::shogi::piece_constants::SEE_PIECE_VALUES[0][pt as usize])
                .unwrap_or(0);
            let best_gain = stand_pat + captured_val + QS_PROMOTE_BONUS + QS_MARGIN_CAPTURE;
            if best_gain <= alpha {
                continue;
            }

            let mut child = pos.clone();
            let sc = {
                let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                child.do_move(mv);
                -self.qsearch(&child, -beta, -alpha, ctx, ply + 1)
            };
            if sc >= beta {
                return sc;
            }
            if sc > alpha {
                alpha = sc;
            }
        }

        if qs_checks_enabled() {
            let Ok(quiet) = mg.generate_quiet(pos) else {
                return alpha;
            };
            let mut tried_checks = 0usize;
            for mv in quiet.as_slice().iter().copied() {
                if tried_checks >= QS_MAX_QUIET_CHECKS {
                    break;
                }
                if pos.gives_check(mv) {
                    tried_checks += 1;
                    let mut child = pos.clone();
                    let sc = {
                        let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                        child.do_move(mv);
                        -self.qsearch(&child, -beta, -alpha, ctx, ply + 1)
                    };
                    if sc >= beta {
                        return sc;
                    }
                    if sc > alpha {
                        alpha = sc;
                    }
                }
            }
        }

        for (mv, _see) in caps.into_iter().filter(|&(_, s)| s < 0) {
            let captured_val = mv
                .captured_piece_type()
                .map(|pt| crate::shogi::piece_constants::SEE_PIECE_VALUES[0][pt as usize])
                .unwrap_or(0);
            if captured_val < 500 && !pos.gives_check(mv) {
                continue;
            }
            let mut child = pos.clone();
            let sc = {
                let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                child.do_move(mv);
                -self.qsearch(&child, -beta, -alpha, ctx, ply + 1)
            };
            if sc >= beta {
                return sc;
            }
            if sc > alpha {
                alpha = sc;
            }
        }

        alpha
    }
}
