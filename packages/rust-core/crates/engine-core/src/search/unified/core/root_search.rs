//! Root search implementation
//!
//! Handles the search from the root position with aspiration windows

use crate::{
    evaluation::evaluate::Evaluator,
    movegen::MoveGenerator,
    search::{
        common::mate_score,
        constants::SEARCH_INF,
        types::{Bound, RootLine},
        unified::{tt_operations::TTOperations, UnifiedSearcher},
    },
    shogi::{Move, Position},
};
use smallvec::SmallVec;

/// Search from root position with aspiration window
pub fn search_root_with_window<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING>,
    pos: &mut Position,
    depth: u8,
    initial_alpha: i32,
    initial_beta: i32,
    previous_pv: &[Move],
) -> (i32, Vec<Move>)
where
    E: Evaluator + Send + Sync + 'static,
{
    // Hard-limit short-circuit at root: exit immediately if past hard or planned end
    if let Some(tm) = &searcher.time_manager {
        let elapsed_ms = tm.elapsed_ms();
        let hard = tm.hard_limit_ms();
        if hard > 0 && hard < u64::MAX && elapsed_ms >= hard {
            // Mark time-based stop to help upper layers salvage PV and attach StopInfo consistently
            searcher.context.mark_time_stopped();
            searcher.context.stop();
            return (initial_alpha, Vec::new());
        }
        let planned = tm.scheduled_end_ms();
        if planned > 0 && planned < u64::MAX && elapsed_ms >= planned {
            searcher.context.mark_time_stopped();
            searcher.context.stop();
            return (initial_alpha, Vec::new());
        }
    }
    // Complete any remaining garbage collection from previous search
    #[cfg(feature = "hashfull_filter")]
    if USE_TT {
        if let Some(ref tt) = searcher.tt {
            // Limit GC iterations to prevent potential infinite loops
            const MAX_GC_ITERATIONS: usize = 8;
            const MAX_GC_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);
            let gc_start = std::time::Instant::now();
            let mut gc_iterations = 0;
            while tt.should_trigger_gc()
                && gc_iterations < MAX_GC_ITERATIONS
                && gc_start.elapsed() < MAX_GC_BUDGET
            {
                tt.perform_incremental_gc(512); // Larger batch size before search starts
                gc_iterations += 1;
            }
        }
    }

    let mut alpha = initial_alpha;
    let beta = initial_beta;
    let mut best_score = -SEARCH_INF;
    let mut pv = Vec::new();

    // Generate all legal moves
    let move_gen = MoveGenerator::new();
    let moves = match move_gen.generate_all(pos) {
        Ok(moves) => moves,
        Err(_) => {
            // King not found - should not happen in valid position
            return (-SEARCH_INF, pv);
        }
    };

    // Diagnostics: log root in_check and attacker count once per root call
    #[cfg(any(feature = "pv_debug_logs", feature = "tt_metrics"))]
    {
        use crate::shogi::board::Color;
        let stm: Color = pos.side_to_move;
        let in_check = pos.is_in_check();
        let atk_count = if let Some(ksq) = pos.board.king_square(stm) {
            pos.get_attackers_to(ksq, stm.opposite()).count_ones()
        } else {
            0
        };
        log::debug!(
            "diag root_check side={:?} in_check={} atk_count={} hash={:#016x} depth={} legal_moves={}",
            stm,
            in_check,
            atk_count,
            pos.zobrist_hash,
            depth,
            moves.len()
        );
    }

    if moves.is_empty() {
        // No legal moves - checkmate or stalemate
        if pos.is_in_check() {
            // Root position is checkmate - update mate distance
            searcher.context.update_mate_distance(0);
            return (mate_score(0, false), pv); // Getting mated at root
        } else {
            return (0, pv); // Stalemate
        }
    }

    // Get TT move for root position if available
    let tt_move = if USE_TT {
        searcher
            .tt
            .as_ref()
            .and_then(|tt| tt.probe_entry(pos.zobrist_hash, pos.side_to_move))
            .and_then(|e| e.get_move())
    } else {
        None
    };

    // Order moves - avoid Vec to SmallVec conversion
    let (ordered_slice, _owned_moves);
    if USE_TT || USE_PRUNING {
        let moves_vec = searcher.ordering.order_moves_at_root(pos, &moves, tt_move, previous_pv);
        _owned_moves = Some(moves_vec);
        let vec_ref = _owned_moves.as_ref().unwrap();
        ordered_slice = &vec_ref[..];
    } else {
        ordered_slice = moves.as_slice();
        _owned_moves = None;
    };

    // Note: Prefetching is done selectively inside the move loop below

    // Search each move
    for (move_idx, &mv) in ordered_slice.iter().enumerate() {
        // Lightweight polling at root for near-hard/stop responsiveness
        if searcher.context.should_stop() {
            break;
        }
        if let Some(tm) = &searcher.time_manager {
            let elapsed_ms = tm.elapsed_ms();
            let hard = tm.hard_limit_ms();
            if hard > 0 && hard < u64::MAX && elapsed_ms >= hard {
                searcher.context.stop();
                break;
            }
            let planned = tm.scheduled_end_ms();
            if planned > 0 && planned < u64::MAX && elapsed_ms >= planned {
                searcher.context.stop();
                break;
            }
        }
        // In debug builds, validate that all moves from move generator are legal
        // (this should always be true, so we just assert it)
        #[cfg(debug_assertions)]
        {
            // Since moves come from generate_all(), they should all be legal
            // TT move is not injected into the list, so this check is purely defensive
            debug_assert!(
                pos.is_pseudo_legal(mv),
                "Move from generate_all() should be pseudo-legal: {} in position {}",
                crate::usi::move_to_usi(&mv),
                crate::usi::position_to_sfen(pos)
            );
        }

        // Make move (pair evaluator hooks with move using guard)
        let eval_arc = searcher.evaluator.clone();
        let mut eval_guard = crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
        let undo_info = pos.do_move(mv);

        #[cfg(feature = "diagnostics")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static ROOT_TRACE_PRIMARY: AtomicU64 = AtomicU64::new(0);
            let seq = ROOT_TRACE_PRIMARY.fetch_add(1, Ordering::Relaxed);
            if seq < 64 {
                eprintln!(
                    "[TT_TRACE] root_after_do_move#{seq} mv={} side={:?} depth={} hash={:016x}",
                    crate::usi::move_to_usi(&mv),
                    pos.side_to_move,
                    depth,
                    pos.zobrist_hash
                );
            }
        }

        // Note: Node counting is done in alpha_beta() to avoid double counting

        // Save child hash for PV owner validation
        // This is the Zobrist hash of the child position after making the move
        let _child_hash = pos.zobrist_hash;

        // Prefetch TT entry for the new position (root moves are always important)
        if USE_TT && !searcher.is_prefetch_disabled() {
            searcher.prefetch_tt(pos.zobrist_hash, pos.side_to_move);
        }

        // Search with null window for moves after the first
        let score = if move_idx == 0 {
            let s = -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha, 1);
            if s >= beta {
                crate::search::SearchStats::bump(&mut searcher.stats.root_fail_high_count, 1);
            }
            s
        } else {
            // Late Move Reduction (if enabled)
            let reduction = if USE_PRUNING && depth >= 3 && move_idx >= 4 && !pos.is_in_check() {
                // Use more sophisticated reduction based on depth and move count
                crate::search::unified::pruning::lmr_reduction(depth, move_idx as u32)
            } else {
                0
            };

            let reduced_depth = (depth - 1).saturating_sub(reduction);
            let score = -super::alpha_beta(searcher, pos, reduced_depth, -alpha - 1, -alpha, 1);
            if score >= beta {
                crate::search::SearchStats::bump(&mut searcher.stats.root_fail_high_count, 1);
            }

            // Re-search if score beats alpha
            if score > alpha && score < beta {
                -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha, 1)
            } else {
                score
            }
        };

        // Undo move (guarantee evaluator hook pairing)
        pos.undo_move(mv, undo_info);
        eval_guard.undo();

        // Check stop flag immediately after alpha-beta search
        if searcher.context.should_stop() {
            // Skip TT storage when stopping - adds overhead
            break;
        }

        // Process events (including ponder hit) every move at root
        searcher.context.process_events(&searcher.time_manager);

        // Time management is handled by should_stop() during search

        // Also check time manager at root (but ensure at least one move is fully searched)
        if move_idx > 0
            && searcher.context.check_time_limit(searcher.stats.nodes, &searcher.time_manager)
        {
            // Skip TT storage when stopping - adds overhead
            break;
        }

        // Check for timeout
        if searcher.context.should_stop() {
            // Skip TT storage when stopping - adds overhead
            break;
        }

        // Update best move
        if score > best_score {
            best_score = score;

            // Check if this is a mate score and update mate distance
            if crate::search::common::is_mate_score(score) {
                if let Some(distance) = crate::search::common::extract_mate_distance(score) {
                    searcher.context.update_mate_distance(distance);
                }
            }

            pv.clear();
            pv.push(mv);

            // Get PV from recursive search (stack-based)
            let child_pv: &[crate::shogi::Move] =
                if crate::search::types::SearchStack::is_valid_ply(1) {
                    &searcher.search_stack[1].pv_line
                } else {
                    &[]
                };

            // Debug logging for PV construction at root
            #[cfg(debug_assertions)]
            if cfg!(feature = "pv_debug_logs") {
                eprintln!(
                    "[ROOT PV] depth={depth}, move_idx={move_idx}, best_move={}, score={score}",
                    crate::usi::move_to_usi(&mv)
                );
                if !child_pv.is_empty() {
                    eprintln!(
                        "  Child PV (ply=1): {}",
                        child_pv.iter().map(crate::usi::move_to_usi).collect::<Vec<_>>().join(" ")
                    );
                } else {
                    eprintln!("  Child PV (ply=1): <empty>");
                }
            }

            // Extend with child PV (stack-based). In debug, optionally validate.
            #[cfg(not(debug_assertions))]
            {
                pv.extend_from_slice(child_pv);
            }
            #[cfg(debug_assertions)]
            {
                if cfg!(feature = "pv_debug_logs") {
                    // Validate child PV moves before extending
                    let mut valid_child_pv = Vec::new();
                    let mut temp_pos = pos.clone();
                    let undo_info = temp_pos.do_move(mv);
                    let mut undo_infos = Vec::new();
                    for &child_mv in child_pv {
                        if temp_pos.is_pseudo_legal(child_mv) {
                            valid_child_pv.push(child_mv);
                            let child_undo = temp_pos.do_move(child_mv);
                            undo_infos.push((child_mv, child_undo));
                        } else {
                            eprintln!("[WARNING] Invalid move in child PV at root, truncating");
                            eprintln!("  Move: {}", crate::usi::move_to_usi(&child_mv));
                            eprintln!("  Position: {}", crate::usi::position_to_sfen(&temp_pos));
                            break;
                        }
                    }
                    for (child_mv, child_undo) in undo_infos.iter().rev() {
                        temp_pos.undo_move(*child_mv, child_undo.clone());
                    }
                    temp_pos.undo_move(mv, undo_info);
                    pv.extend_from_slice(&valid_child_pv);
                } else {
                    pv.extend_from_slice(child_pv);
                }
            }

            // Notify time manager that PV head changed to refresh PV stability window
            if let Some(ref tm) = searcher.time_manager {
                tm.on_pv_change(depth as u32);
            }
            // Count PV head changes
            searcher.stats.pv_changed = Some(searcher.stats.pv_changed.unwrap_or(0) + 1);

            if score > alpha {
                alpha = score;
            }
        }
    }

    // Validate PV before returning
    if !pv.is_empty() {
        // Debug logging for final PV construction
        #[cfg(debug_assertions)]
        if cfg!(feature = "pv_debug_logs") {
            eprintln!(
                "[ROOT FINAL PV] depth={depth}, score={best_score}, pv_len={}, pv={}",
                pv.len(),
                pv.iter().map(crate::usi::move_to_usi).collect::<Vec<_>>().join(" ")
            );

            // Check for suspicious moves in PV
            if pv.len() >= 8 {
                if let Some(&mv) = pv.get(7) {
                    eprintln!("  PV[7] (8th move) = {}", crate::usi::move_to_usi(&mv));
                }
            }
        }

        // Use centralized PV trimming function
        // This ensures all PVs are validated consistently
        let original_len = pv.len();
        pv = super::pv::trim_legal_pv(pos.clone(), &pv);

        // Record trimming statistics
        crate::search::SearchStats::bump(&mut searcher.stats.pv_trim_checks, 1);
        if pv.len() < original_len {
            crate::search::SearchStats::bump(&mut searcher.stats.pv_trim_cuts, 1);
        }

        // Full validation only in debug builds
        #[cfg(debug_assertions)]
        {
            // First check occupancy invariants (doesn't rely on move generator)
            super::pv_validation::pv_local_sanity(pos, &pv);
            // Then check legal moves
            super::pv_validation::assert_pv_legal(pos, &pv);
        }
    }

    (best_score, pv)
}

/// Experimental: Root search that returns MultiPV lines (skeleton)
///
/// For now, this function produces a single line equivalent to the current
/// search_root_with_window result. Future revisions will enumerate all root
/// moves and refine the top-K lines to Exact.
pub fn search_root_multipv<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING>,
    pos: &mut Position,
    depth: u8,
    initial_alpha: i32,
    initial_beta: i32,
    previous_pv: &[Move],
    k: u8,
) -> SmallVec<[RootLine; 4]>
where
    E: Evaluator + Send + Sync + 'static,
{
    // Prepare move list
    let move_gen = MoveGenerator::new();
    let moves = match move_gen.generate_all(pos) {
        Ok(moves) => moves,
        Err(_) => return SmallVec::new(),
    };

    if moves.is_empty() {
        return SmallVec::new();
    }

    // Order root moves using TT and previous PV
    let tt_move = if USE_TT {
        searcher
            .tt
            .as_ref()
            .and_then(|tt| tt.probe_entry(pos.zobrist_hash, pos.side_to_move))
            .and_then(|e| e.get_move())
    } else {
        None
    };

    let ordered: SmallVec<[Move; 128]> = if USE_TT || USE_PRUNING {
        let v = searcher.ordering.order_moves_at_root(pos, &moves, tt_move, previous_pv);
        let mut sv = SmallVec::with_capacity(v.len());
        sv.extend_from_slice(&v);
        sv
    } else {
        let mut sv = SmallVec::with_capacity(moves.len());
        sv.extend_from_slice(moves.as_slice());
        sv
    };

    // Collect candidates: (mv, score, bound, pv)
    struct Cand {
        mv: Move,
        score: i32,
        bound: Bound,
        pv: SmallVec<[Move; 32]>,
    }

    let mut alpha = initial_alpha;
    let beta = initial_beta;
    let mut cands: SmallVec<[Cand; 64]> = SmallVec::new();

    for (i, &mv) in ordered.iter().enumerate() {
        if searcher.context.should_stop() {
            break;
        }

        // Make move with evaluator hooks
        let eval_arc = searcher.evaluator.clone();
        let mut eval_guard = crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
        let undo = pos.do_move(mv);

        #[cfg(feature = "diagnostics")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static ROOT_MOVE_TRACE: AtomicU64 = AtomicU64::new(0);
            let seq = ROOT_MOVE_TRACE.fetch_add(1, Ordering::Relaxed);
            if seq < 32 {
                eprintln!(
                    "[TT_TRACE] root_after_do_move#{seq} mv={} side={:?} depth={} hash={:016x}",
                    crate::usi::move_to_usi(&mv),
                    pos.side_to_move,
                    depth,
                    pos.zobrist_hash
                );
            }
        }

        // Optional prefetch
        if USE_TT && !searcher.is_prefetch_disabled() {
            searcher.prefetch_tt(pos.zobrist_hash, pos.side_to_move);
        }

        // Search pattern like PVS at root
        let (score, bound) = if i == 0 {
            let alpha0 = initial_alpha; // capture initial alpha for first move bound classification
            let s = -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha0, 1);
            (
                s,
                if s <= alpha0 {
                    Bound::UpperBound
                } else if s >= beta {
                    Bound::LowerBound
                } else {
                    Bound::Exact
                },
            )
        } else {
            let reduction = if USE_PRUNING && depth >= 3 && i >= 4 && !pos.is_in_check() {
                let r = crate::search::unified::pruning::lmr_reduction(depth, i as u32);
                let cap = crate::search::unified::pruning::lmr_cap_for_profile(
                    searcher.teacher_profile(),
                );
                r.min(cap)
            } else {
                0
            };
            let rd = (depth - 1).saturating_sub(reduction);
            let s0 = -super::alpha_beta(searcher, pos, rd, -alpha - 1, -alpha, 1);
            if s0 >= beta {
                crate::search::SearchStats::bump(&mut searcher.stats.root_fail_high_count, 1);
            }
            if (s0 > alpha && s0 < beta) || (reduction > 0 && s0 >= beta) {
                let s1 = -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha, 1);
                (s1, Bound::Exact)
            } else if s0 <= alpha {
                (s0, Bound::UpperBound)
            } else {
                (s0, Bound::LowerBound)
            }
        };

        // Snapshot child PV from stack (ply=1)
        let child_pv: &[Move] = if crate::search::types::SearchStack::is_valid_ply(1) {
            &searcher.search_stack[1].pv_line
        } else {
            &[]
        };
        let mut pv_snap: SmallVec<[Move; 32]> = SmallVec::new();
        pv_snap.push(mv);
        if !child_pv.is_empty() {
            for &m in child_pv.iter() {
                pv_snap.push(m);
                if pv_snap.len() >= 32 {
                    break;
                }
            }
        }

        // Undo move
        pos.undo_move(mv, undo);
        eval_guard.undo();

        // Update alpha and store candidate
        if score > alpha {
            alpha = score;
        }
        cands.push(Cand {
            mv,
            score,
            bound,
            pv: pv_snap,
        });

        // Periodic event/time checks
        searcher.context.process_events(&searcher.time_manager);
        if searcher.context.check_time_limit(searcher.stats.nodes, &searcher.time_manager) {
            // Ensure we evaluate at least one candidate before breaking
            if !cands.is_empty() {
                break;
            }
        }
    }

    // Select top-K candidates efficiently (partial sort)
    fn bound_pri(b: Bound) -> i32 {
        match b {
            Bound::Exact => 2,
            Bound::LowerBound => 1,
            Bound::UpperBound => 0,
        }
    }
    let k_usize = k.max(1) as usize;
    if cands.len() > k_usize {
        let (left, _pivot, _right) = cands.select_nth_unstable_by(k_usize - 1, |a, b| {
            b.score.cmp(&a.score).then_with(|| bound_pri(b.bound).cmp(&bound_pri(a.bound)))
        });
        left.sort_unstable_by(|a, b| {
            b.score.cmp(&a.score).then_with(|| bound_pri(b.bound).cmp(&bound_pri(a.bound)))
        });
    } else {
        cands.sort_unstable_by(|a, b| {
            b.score.cmp(&a.score).then_with(|| bound_pri(b.bound).cmp(&bound_pri(a.bound)))
        });
    }
    // Note: no full re-sort here; top-K slice already sorted above

    // Build lines for top-K, re-searching non-Exact with full window when budget allows
    let take = k_usize.min(cands.len());
    let mut lines: SmallVec<[RootLine; 4]> = SmallVec::new();
    if take == 0 {
        // Fallback: if no candidates (extreme budget pressure), produce at least one line
        let gen = MoveGenerator::new();
        if let Ok(moves) = gen.generate_all(pos) {
            if let Some(&mv) = moves.as_slice().first() {
                let eval_arc = searcher.evaluator.clone();
                let mut eval_guard =
                    crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
                let undo = pos.do_move(mv);
                let s = -super::alpha_beta(
                    searcher,
                    pos,
                    depth.saturating_sub(1),
                    -SEARCH_INF,
                    SEARCH_INF,
                    1,
                );
                let child_pv: &[Move] = if crate::search::types::SearchStack::is_valid_ply(1) {
                    &searcher.search_stack[1].pv_line
                } else {
                    &[]
                };
                let mut pv = SmallVec::<[Move; 32]>::new();
                pv.push(mv);
                for &m in child_pv.iter() {
                    if pv.len() >= 32 {
                        break;
                    }
                    pv.push(m);
                }
                pos.undo_move(mv, undo);
                eval_guard.undo();

                let line = RootLine {
                    multipv_index: 1,
                    root_move: mv,
                    score_internal: s,
                    score_cp: if crate::search::common::is_mate_score(s) {
                        s
                    } else {
                        s.clamp(-1200, 1200)
                    },
                    bound: Bound::Exact,
                    depth: depth as u32,
                    seldepth: searcher.stats.seldepth,
                    pv,
                    nodes: None,
                    time_ms: None,
                    exact_exhausted: true,
                    exhaust_reason: Some("fallback".to_string()),
                    mate_distance: if crate::search::common::is_mate_score(s) {
                        crate::search::common::extract_mate_distance(s).map(|d| d as i32)
                    } else {
                        None
                    },
                };
                lines.push(line);
                return lines;
            }
        }
        return lines;
    }
    for idx in 0..take {
        let mut score = cands[idx].score;
        let mut bound = cands[idx].bound;
        let mv = cands[idx].mv;
        let mut pv = cands[idx].pv.clone();
        let mut exact_exhausted = false;
        let mut exhaust_reason: Option<String> = None;

        // Budget guard: re-search only if we have sufficient remaining budget
        if bound != Bound::Exact && has_budget_for_exactify(searcher) {
            let eval_arc = searcher.evaluator.clone();
            let mut eval_guard = crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
            let undo = pos.do_move(mv);

            // Re-search with full window for robustness
            let s2 = -super::alpha_beta(searcher, pos, depth - 1, -SEARCH_INF, SEARCH_INF, 1);

            // Capture PV again after re-search
            let child_pv: &[Move] = if crate::search::types::SearchStack::is_valid_ply(1) {
                &searcher.search_stack[1].pv_line
            } else {
                &[]
            };
            pv.clear();
            pv.push(mv);
            if !child_pv.is_empty() {
                for &m in child_pv.iter() {
                    pv.push(m);
                    if pv.len() >= 32 {
                        break;
                    }
                }
            }

            pos.undo_move(mv, undo);
            eval_guard.undo();

            score = s2;
            bound = Bound::Exact; // Full-window re-search yields exact root score
        } else if bound != Bound::Exact {
            // Could not exactify due to budget/time; mark exhaustion
            exact_exhausted = true;
            exhaust_reason = Some(if searcher.context.was_time_stopped() {
                "timeout".to_string()
            } else if searcher.context.should_stop() {
                "user_stop".to_string()
            } else {
                "budget".to_string()
            });
        }

        // Prepare dual representation and mate distance
        let score_internal = score;
        let mate_distance = if crate::search::common::is_mate_score(score_internal) {
            crate::search::common::extract_mate_distance(score_internal).map(|d| d as i32)
        } else {
            None
        };
        let clamp_cp = |s: i32| -> i32 {
            if crate::search::common::is_mate_score(s) {
                s // keep internal mate-like score for clarity; cp is still clipped below
            } else {
                s.clamp(-1200, 1200)
            }
        };
        let score_cp = clamp_cp(score_internal);

        let line = RootLine {
            multipv_index: (idx as u8) + 1,
            root_move: mv,
            score_internal,
            score_cp,
            bound,
            depth: depth as u32,
            seldepth: searcher.stats.seldepth,
            pv,
            nodes: None,
            time_ms: None,
            exact_exhausted,
            exhaust_reason,
            mate_distance,
        };
        lines.push(line);
    }

    lines
}

/// Determine if there is sufficient remaining budget to perform a full-window
/// re-search to exactify a non-Exact root line.
/// Heuristic: require at least ~8% of node or time budget remaining.
fn has_budget_for_exactify<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &UnifiedSearcher<E, USE_TT, USE_PRUNING>,
) -> bool
where
    E: Evaluator + Send + Sync + 'static,
{
    // Node-based budget
    if let Some(limit) = searcher.context.limits().node_limit() {
        let used = searcher.stats.nodes;
        if used >= limit {
            return false;
        }
        let remaining = limit - used;
        // Dynamic threshold with floor for smaller budgets
        let frac = if limit < 10_000 {
            0.02
        } else if limit < 50_000 {
            0.04
        } else {
            0.08
        };
        let mut need = ((limit as f64) * frac) as u64;
        if need < 200 {
            need = 200;
        }
        if remaining < need {
            return false;
        }
    }

    // Time-based budget via TimeManager
    if let Some(ref tm) = searcher.time_manager {
        let elapsed_ms = searcher.context.elapsed().as_millis() as u64;
        let total_ms = if tm.hard_limit_ms() > 0 {
            tm.hard_limit_ms()
        } else {
            tm.soft_limit_ms()
        };
        if total_ms > 0 {
            if elapsed_ms >= total_ms {
                return false;
            }
            let remaining = total_ms - elapsed_ms;
            let frac = if total_ms < 500 { 0.02 } else { 0.08 };
            let mut need = ((total_ms as f64) * frac) as u64;
            if need < 10 {
                need = 10;
            }
            if remaining < need {
                return false;
            }
        }
    }

    !searcher.context.should_stop()
}
