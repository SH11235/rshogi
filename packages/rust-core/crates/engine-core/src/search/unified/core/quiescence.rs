//! Quiescence search implementation
//!
//! Handles capture sequences to avoid horizon effects

use crate::{
    evaluation::evaluate::Evaluator,
    movegen::MoveGenerator,
    search::{
        common::mate_score,
        constants::{MAX_PLY, MAX_QUIESCE_DEPTH, SEARCH_INF},
        unified::{
            core::log_rate_limiter::QUIESCE_DEPTH_LIMITER,
            pruning::{delta_pruning_margin, likely_could_give_check, should_skip_see_pruning},
            UnifiedSearcher,
        },
        SearchStack, SearchStats, MAX_QPLY, QUIESCE_CHECK_EVAL_PENALTY,
    },
    search_debug,
    shogi::{Move, MoveList, Position},
    usi::{move_to_usi, position_to_sfen},
    PieceType,
};
use once_cell::sync::Lazy;
use smallvec::SmallVec;
use std::cell::Cell;
use std::sync::atomic::Ordering;

// Import victim_score from move_ordering module
use super::move_ordering::victim_score;

// Conservative initial values for checking moves in quiescence search
// Drops
const MAX_QS_CHECK_DROPS: usize = 4; // Maximum number of checking drops to search in QS
const QS_CHECK_ENABLE_MARGIN: i32 = 80; // Small margin to prune clearly losing drops
const CHECK_DROP_NEAR_KING_DIST: u8 = 2; // Manhattan distance limit for drops near king
                                         // Non-capture checks
const MAX_QS_NONCAP_CHECKS: usize = 4; // Maximum number of non-capture checking moves
const QS_NONCAP_QPLY_CAP: u8 = 4; // Limit relative depth for non-capture checks in QS
                                  // Non-capture promotion checks
const MAX_QS_PROMO_CHECKS: usize = 4; // Maximum number of non-capture promotions that give check
const QS_PROMO_QPLY_CAP: u8 = 4; // Limit relative depth for promotion checks in QS
                                 // Total budget across all check-like categories to prevent spikes
const MAX_QS_TOTAL_CHECKS: usize = 6;

// Runtime/compile-time toggle for enabling checking moves in quiescence search.
// Priority (high → low):
// 1) Compile-time features: `qs_checks_force_off` / `qs_checks_force_on`
// 2) Runtime env var: `SHOGI_QS_DISABLE_CHECKS` ("1" disables checks)
static QS_CHECKS_ENABLED: Lazy<bool> = Lazy::new(|| {
    // Mutual exclusion guard (both features at once is invalid)
    #[cfg(all(feature = "qs_checks_force_off", feature = "qs_checks_force_on"))]
    compile_error!("qs_checks_force_off and qs_checks_force_on are mutually exclusive");

    // Compile-time overrides
    #[cfg(feature = "qs_checks_force_off")]
    {
        false
    }
    #[cfg(all(not(feature = "qs_checks_force_off"), feature = "qs_checks_force_on"))]
    {
        true
    }
    #[cfg(all(
        not(feature = "qs_checks_force_off"),
        not(feature = "qs_checks_force_on")
    ))]
    {
        // Runtime default via env var
        !std::env::var("SHOGI_QS_DISABLE_CHECKS").map(|v| v == "1").unwrap_or(false)
    }
});

// Lightweight, time-based poll state for quiescence search.
// We keep a thread-local last-poll timestamp (ms since search start) to avoid
// relying solely on node-count based polling, which can be sparse on heavy paths.
thread_local! {
    static QS_LAST_LIGHT_POLL_MS: Cell<u64> = const { Cell::new(0) };
}

#[inline(always)]
fn qs_should_light_poll<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &UnifiedSearcher<E, USE_TT, USE_PRUNING>,
) -> bool
where
    E: Evaluator + Send + Sync + 'static,
{
    if let Some(tm) = &searcher.time_manager {
        let elapsed_ms = searcher.context.elapsed().as_millis() as u64;
        let hard = tm.hard_limit_ms();
        // Near-hard window: always poll when within 50ms of hard deadline
        let near_hard = hard < u64::MAX && elapsed_ms.saturating_add(50) >= hard;
        // Lightweight periodic poll: ~every 12ms
        let periodic = QS_LAST_LIGHT_POLL_MS.with(|c| {
            let last = c.get();
            let due = elapsed_ms.saturating_sub(last) >= 12;
            if due {
                c.set(elapsed_ms);
            }
            due
        });
        return near_hard || periodic;
    }
    false
}

#[inline(always)]
fn qs_light_time_poll<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING>,
) -> bool
where
    E: Evaluator + Send + Sync + 'static,
{
    if qs_should_light_poll(searcher) {
        // Process events and check time limit (unified path).
        searcher.context.process_events(&searcher.time_manager);
        if searcher.context.check_time_limit(searcher.stats.nodes, &searcher.time_manager)
            || searcher.context.should_stop()
        {
            return true;
        }
    }
    false
}

/// Quiescence search to resolve tactical exchanges and avoid horizon effects
///
/// This function searches capture moves (and check evasions when in check) to ensure
/// the evaluation is based on a quiet position. It prevents the search from stopping
/// in the middle of a capture sequence which could lead to incorrect evaluations.
///
/// # Parameters
/// - `searcher`: The search context containing evaluation, TT, and statistics
/// - `pos`: Current position to search
/// - `alpha`: Lower bound of the search window
/// - `beta`: Upper bound of the search window
/// - `ply`: Absolute depth from root position (for mate distance calculation)
/// - `qply`: Relative quiescence depth (incremented from 0 at qsearch entry)
///   Used to limit qsearch depth independently of main search depth
///   Not applied in check positions to ensure proper check evasion
///
/// # Returns
/// The evaluation score of the position after resolving captures
///
/// # Note on Transposition Table Usage
/// This implementation currently does not use the transposition table (TT).
/// If TT support is added in the future, ensure that mate scores are properly
/// adjusted using `adjust_mate_score_for_tt` when storing and
/// `adjust_mate_score_from_tt` when retrieving, similar to the implementation
/// in `alpha_beta` and `search_node` functions.
///
/// # Token Return Policy
/// For qnodes budget tracking, tokens are only returned at function entry when
/// the budget is exceeded. Tokens are NOT returned during the search loops to
/// provide more accurate statistics about nodes actually searched. This policy
/// ensures that the node count reflects actual work performed rather than
/// prematurely aborted attempts.
pub fn quiescence_search<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING>,
    pos: &mut Position,
    mut alpha: i32,
    beta: i32,
    ply: u16,
    qply: u8,
) -> i32
where
    E: Evaluator + Send + Sync + 'static,
{
    searcher.stats.nodes += 1;
    searcher.stats.qnodes += 1;

    // Extract qnodes limit and shared counter at function start for efficiency
    let qlimit = searcher.context.limits().qnodes_limit;
    let qnodes_counter = searcher.context.limits().qnodes_counter.clone();

    // Increment shared counter if available (for accurate aggregation)
    // Returns the previous value before increment
    let prev_shared_qnodes =
        qnodes_counter.as_ref().map(|counter| counter.fetch_add(1, Ordering::AcqRel));

    // Early stop check
    if searcher.context.should_stop() {
        return alpha;
    }

    // Periodic time check (unified with alpha_beta and search_node)
    let time_check_mask = super::time_control::get_event_poll_mask(searcher);
    if time_check_mask == 0 || (searcher.stats.nodes & time_check_mask) == 0 {
        // Process events
        searcher.context.process_events(&searcher.time_manager);

        // Check time limit
        if searcher.context.check_time_limit(searcher.stats.nodes, &searcher.time_manager) {
            return alpha;
        }

        // Check if stop was triggered
        if searcher.context.should_stop() {
            return alpha;
        }
    }

    // Lightweight time-based polling to cover paths where node-based polling is sparse
    if qs_light_time_poll(searcher) {
        return alpha;
    }

    // Check if in check first - this determines our search strategy
    let in_check = pos.is_in_check();

    // Path-dependent repetition handling in quiescence as well
    if pos.is_repetition() {
        // Use stack-cached static eval if available to avoid double evaluation
        let static_eval = if SearchStack::is_valid_ply(ply) {
            let slot = &mut searcher.search_stack[ply as usize].static_eval;
            if let Some(v) = *slot {
                v
            } else {
                let v = searcher.evaluator.evaluate(pos);
                *slot = Some(v);
                v
            }
        } else {
            searcher.evaluator.evaluate(pos)
        };

        let score = super::repetition_penalty(static_eval).clamp(alpha, beta);
        return score;
    }

    // QNodes budget check (after in_check determination)
    if let Some(limit) = qlimit {
        // Check if we've exceeded the limit using previous value
        let exceeded = if let Some(prev) = prev_shared_qnodes {
            // Parallel search: use the previous value from shared counter
            prev >= limit
        } else {
            // Single-threaded search: use previous local counter value
            // Note: stats.qnodes was already incremented at function start
            let prev_local = searcher.stats.qnodes.saturating_sub(1);
            prev_local >= limit
        };

        if exceeded {
            log::trace!("qsearch budget exceeded at qnodes={}", searcher.stats.qnodes);

            // Token return policy: We only return tokens at function entry to minimize overshoot
            // In loops below, we don't return tokens because actual search work has already begun
            // This provides more accurate statistics about nodes actually searched
            if let Some(ref counter) = qnodes_counter {
                counter.fetch_sub(1, Ordering::AcqRel);
            }
            // Also revert local counter to maintain consistency
            searcher.stats.qnodes = searcher.stats.qnodes.saturating_sub(1);

            if in_check {
                // In check: cannot use stand pat, conservatively return alpha
                return alpha;
            } else {
                // Not in check: return current evaluation clamped to bounds
                let eval = searcher.evaluator.evaluate(pos);
                if eval >= beta {
                    return beta;
                }
                if eval > alpha {
                    return eval;
                }
                return alpha;
            }
        }
    }

    // Debug logging gate (read once)
    static QS_DEBUG: Lazy<bool> = Lazy::new(|| {
        // Enable with SHOGI_DEBUG_QS=1
        std::env::var("SHOGI_DEBUG_QS").map(|v| v == "1").unwrap_or(false)
    });

    // Primary limit: relative qsearch depth
    // This ensures consistent qsearch behavior regardless of main search depth
    // Note: In check positions, we skip this limit to ensure proper check evasion
    if !in_check {
        let qply_limit = MAX_QPLY;

        if qply >= qply_limit {
            // Debug-only logging (opt-in via SHOGI_DEBUG_QS=1) with rate limiting
            if *QS_DEBUG
                && log::log_enabled!(log::Level::Debug)
                && super::log_rate_limiter::QUIESCE_DEPTH_LIMITER.should_log()
            {
                log::debug!("Hit relative quiescence depth limit qply={qply} (limit={qply_limit})");
            }
            // Not in check, return stand pat evaluation with proper window handling
            let stand_pat = searcher.evaluator.evaluate(pos);
            if stand_pat >= beta {
                return beta;
            }
            if stand_pat > alpha {
                return stand_pat;
            }
            return alpha;
        }
    }

    // Secondary safeguard: absolute ply limit
    // This is a safety net to prevent stack overflow in extreme cases
    if ply >= MAX_PLY as u16 {
        // Debug-only logging (opt-in) with rate limiting
        if *QS_DEBUG
            && log::log_enabled!(log::Level::Debug)
            && super::log_rate_limiter::QUIESCE_DEPTH_LIMITER.should_log()
        {
            log::debug!("Hit absolute ply limit {ply} in quiescence search");
        }
        // In check at max depth is rare but we can't recurse further
        // Return pessimistic value rather than static eval which could be illegal
        return if in_check {
            alpha
        } else {
            searcher.evaluator.evaluate(pos)
        };
    }

    // Tertiary safeguard: absolute quiescence depth (relaxed to 96)
    // This prevents explosion in complex positions with many captures
    if ply >= MAX_QUIESCE_DEPTH {
        // Debug-only logging (opt-in) with rate limiting
        if *QS_DEBUG && log::log_enabled!(log::Level::Debug) && QUIESCE_DEPTH_LIMITER.should_log() {
            log::debug!("Hit absolute quiescence depth limit at ply {ply}");
        }
        // Hard stop at reasonable depth
        // Return evaluation-based value instead of fixed constants to avoid discontinuity
        return if in_check {
            // In check positions, return a value based on evaluation but slightly pessimistic
            alpha.max(searcher.evaluator.evaluate(pos) - QUIESCE_CHECK_EVAL_PENALTY)
        } else {
            // Not in check, return current evaluation
            searcher.evaluator.evaluate(pos)
        };
    }

    if in_check {
        // In check: must search all legal moves (no stand pat)
        // Generate all legal moves
        let move_gen = MoveGenerator::new();
        let moves = match move_gen.generate_all(pos) {
            Ok(moves) => moves,
            Err(_) => {
                // King not found - should not happen in valid position
                return -SEARCH_INF;
            }
        };

        // If no legal moves, it's checkmate
        if moves.is_empty() {
            return mate_score(ply as u8, false); // Getting mated
        }

        // Search all legal moves to find check evasion
        let mut best = -SEARCH_INF;
        for &mv in moves.iter() {
            // Check QNodes budget before each move (important for strict limit enforcement)
            if let Some(limit) = qlimit {
                let exceeded = if let Some(ref counter) = qnodes_counter {
                    // Check current value against limit
                    counter.load(Ordering::Acquire) >= limit
                } else {
                    // Single-threaded: check current local counter
                    searcher.stats.qnodes >= limit
                };

                if exceeded {
                    // If we haven't found any move yet, return alpha
                    // Otherwise return the best score found so far
                    // Note: We don't return tokens here as search has already progressed
                    return if best == -SEARCH_INF { alpha } else { best };
                }
            }

            // Validate move
            if !pos.is_pseudo_legal(mv) {
                continue;
            }

            // Make move (pair evaluator hooks with move using guard)
            let eval_arc = searcher.evaluator.clone();
            let mut eval_guard = crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
            let undo_info = pos.do_move(mv);

            // Check if still in check after the move
            let still_in_check = pos.is_in_check();
            if still_in_check {
                pos.undo_move(mv, undo_info);
                eval_guard.undo();
                continue; // Not a valid evasion
            }

            // Recursive search (increment both ply and qply)
            let score =
                -quiescence_search(searcher, pos, -beta, -alpha, ply + 1, qply.saturating_add(1));

            // Undo move
            pos.undo_move(mv, undo_info);
            eval_guard.undo();

            // Check stop flag
            if searcher.context.should_stop() {
                return alpha;
            }

            // Update bounds
            if score >= beta {
                return beta;
            }
            if score > best {
                best = score;
            }
            if score > alpha {
                alpha = score;
            }
        }

        return alpha.max(best);
    }

    // Not in check: normal quiescence search with captures only
    // Stand pat
    let stand_pat = searcher.evaluator.evaluate(pos);
    if stand_pat >= beta {
        return beta;
    }
    if alpha < stand_pat {
        alpha = stand_pat;
    }

    // Delta pruning margin
    let delta_margin = if USE_PRUNING {
        delta_pruning_margin()
    } else {
        0
    };

    // Generate all moves for quiescence (we'll filter captures manually)
    let move_gen = MoveGenerator::new();
    let all_moves = match move_gen.generate_all(pos) {
        Ok(moves) => moves,
        Err(_) => {
            // King not found - should not happen in valid position
            return stand_pat;
        }
    };

    // Filter to captures only - check board state directly instead of relying on metadata
    let us = pos.side_to_move;
    let mut moves = MoveList::new();
    for &mv in all_moves.iter() {
        if mv.is_drop() {
            continue; // Drops don't capture
        }
        // Check if destination square has enemy piece
        if let Some(piece) = pos.piece_at(mv.to()) {
            if piece.color == us.opposite() {
                moves.push(mv);
            }
        }
    }

    // Apply pruning optimizations if enabled
    if USE_PRUNING {
        // Apply SEE filtering first to remove bad captures (Phase 1 implementation)
        // This reduces the number of moves to sort, improving performance
        moves.as_mut_vec().retain(|mv| {
            // Check if this move should skip SEE pruning (drops, checks, etc.)
            if should_skip_see_pruning(pos, *mv) {
                return true;
            }

            // Keep captures with SEE >= 0
            pos.see_ge(*mv, 0)
        });

        // Order remaining captures by MVV-LVA - sort in place to avoid allocation
        moves.as_mut_vec().sort_by_cached_key(|&mv| {
            // MVV-LVA: prioritize capturing more valuable pieces
            // First try metadata, then fall back to board lookup
            if let Some(victim) = mv.captured_piece_type() {
                -victim_score(victim)
            } else if let Some(piece) = pos.piece_at(mv.to()) {
                -victim_score(piece.piece_type)
            } else {
                0 // Should not happen for captures
            }
        });
    }

    // Search captures
    for &mv in moves.iter() {
        // Check stop flag at the beginning of each capture move
        if searcher.context.should_stop() {
            return alpha; // Return current alpha value
        }

        // Lightweight time-based polling inside hot loop
        if qs_light_time_poll(searcher) {
            return alpha;
        }

        // Check QNodes budget before each capture move
        if let Some(limit) = qlimit {
            let exceeded = if let Some(ref counter) = qnodes_counter {
                counter.load(Ordering::Acquire) >= limit
            } else {
                searcher.stats.qnodes >= limit
            };

            if exceeded {
                // Note: We don't return tokens here as search has already progressed
                return alpha; // Return current best bound
            }
        }

        // Delta pruning - skip captures that can't improve position enough
        if USE_PRUNING && delta_margin > 0 {
            // Estimate material gain from capture
            let material_gain = if let Some(victim) = mv.captured_piece_type() {
                victim_score(victim)
            } else if let Some(piece) = pos.piece_at(mv.to()) {
                // Fallback to board lookup if metadata is missing
                victim_score(piece.piece_type)
            } else {
                0
            };

            // Skip if capture can't improve alpha
            if stand_pat + material_gain + delta_margin < alpha {
                continue;
            }
        }

        // Validate move before executing (important for safety)
        if !pos.is_pseudo_legal(mv) {
            search_debug!("WARNING: Skipping illegal move in quiescence search");
            search_debug!("Move: {}", move_to_usi(&mv));
            search_debug!("Position: {}", position_to_sfen(pos));
            continue;
        }

        // Make move (pair evaluator hooks with move using guard)
        let eval_arc = searcher.evaluator.clone();
        let mut eval_guard = crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
        let undo_info = pos.do_move(mv);

        // Skip prefetch in quiescence - adds overhead with minimal benefit
        // (Controlled by tt_filter module)

        // Recursive search (increment both ply and qply)
        let score =
            -quiescence_search(searcher, pos, -beta, -alpha, ply + 1, qply.saturating_add(1));

        // Undo move
        pos.undo_move(mv, undo_info);
        eval_guard.undo();

        // Stop check after recursion
        if searcher.context.should_stop() {
            return alpha;
        }

        // Update alpha
        if score >= beta {
            return beta; // Beta cutoff
        }
        if score > alpha {
            alpha = score;
        }
    }

    // Global checks budget across categories (noncap, promo, drops)
    let mut checks_budget: usize = MAX_QS_TOTAL_CHECKS;

    // === Add: non-drop, non-capture checking moves (limited) ===
    // Only enable with pruning profile to avoid blowing up basic search
    if USE_PRUNING
        && *QS_CHECKS_ENABLED
        && stand_pat + QS_CHECK_ENABLE_MARGIN >= alpha
        && qply < QS_NONCAP_QPLY_CAP
        && checks_budget > 0
    {
        let mut check_noncaptures: SmallVec<[Move; 16]> = all_moves
            .iter()
            .copied()
            .filter(|&mv| !mv.is_drop())
            .filter(|&mv| pos.piece_at(mv.to()).is_none()) // non-capture only
            .filter(|&mv| {
                // Cheap prefilter then accurate check
                likely_could_give_check(pos, mv) && pos.gives_check(mv)
            })
            .collect();

        // Order by proximity/alignment similar to drops
        check_noncaptures.sort_by_key(|&mv| -score_nocap_check_for_ordering(pos, mv));

        let per_cap = MAX_QS_NONCAP_CHECKS.min(checks_budget);
        if check_noncaptures.len() > per_cap {
            check_noncaptures.truncate(per_cap);
        }

        for &mv in check_noncaptures.iter() {
            if searcher.context.should_stop() {
                return alpha;
            }

            if qs_light_time_poll(searcher) {
                return alpha;
            }

            // Budget enforcement
            if let Some(limit) = qlimit {
                let exceeded = if let Some(ref counter) = qnodes_counter {
                    counter.load(Ordering::Acquire) >= limit
                } else {
                    searcher.stats.qnodes >= limit
                };
                if exceeded {
                    return alpha;
                }
            }

            if !pos.is_pseudo_legal(mv) {
                continue;
            }

            let eval_arc = searcher.evaluator.clone();
            let mut eval_guard = crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
            let undo = pos.do_move(mv);
            let score =
                -quiescence_search(searcher, pos, -beta, -alpha, ply + 1, qply.saturating_add(1));
            pos.undo_move(mv, undo);
            eval_guard.undo();

            // Stats: count searched non-capture checks
            SearchStats::bump(&mut searcher.stats.qs_noncapture_checks, 1);

            if searcher.context.should_stop() {
                return alpha;
            }
            if score >= beta {
                return beta;
            }
            if score > alpha {
                alpha = score;
            }
            // Spend total budget
            checks_budget = checks_budget.saturating_sub(1);
            if checks_budget == 0 {
                break;
            }
        }
    }

    // === Add: non-capture promotion moves that give check (limited) ===
    if USE_PRUNING
        && *QS_CHECKS_ENABLED
        && stand_pat + QS_CHECK_ENABLE_MARGIN >= alpha
        && qply < QS_PROMO_QPLY_CAP
        && checks_budget > 0
    {
        let mut promo_check_noncaptures: SmallVec<[Move; 16]> = all_moves
            .iter()
            .copied()
            .filter(|&mv| !mv.is_drop() && mv.is_promote())
            .filter(|&mv| pos.piece_at(mv.to()).is_none()) // non-capture only
            .filter(|&mv| {
                // Cheap prefilter to avoid expensive gives_check calls
                likely_could_give_check(pos, mv) && pos.gives_check(mv)
            })
            .collect();

        promo_check_noncaptures.sort_by_key(|&mv| -score_nocap_check_for_ordering(pos, mv));

        let per_cap = MAX_QS_PROMO_CHECKS.min(checks_budget);
        if promo_check_noncaptures.len() > per_cap {
            promo_check_noncaptures.truncate(per_cap);
        }

        for &mv in promo_check_noncaptures.iter() {
            if searcher.context.should_stop() {
                return alpha;
            }

            if qs_light_time_poll(searcher) {
                return alpha;
            }

            // Budget enforcement
            if let Some(limit) = qlimit {
                let exceeded = if let Some(ref counter) = qnodes_counter {
                    counter.load(Ordering::Acquire) >= limit
                } else {
                    searcher.stats.qnodes >= limit
                };
                if exceeded {
                    return alpha;
                }
            }

            if !pos.is_pseudo_legal(mv) {
                continue;
            }

            let eval_arc = searcher.evaluator.clone();
            let mut eval_guard = crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
            let undo = pos.do_move(mv);
            let score =
                -quiescence_search(searcher, pos, -beta, -alpha, ply + 1, qply.saturating_add(1));
            pos.undo_move(mv, undo);
            eval_guard.undo();

            // Stats: count searched non-capture promotions that give check
            SearchStats::bump(&mut searcher.stats.qs_promo_checks, 1);

            if searcher.context.should_stop() {
                return alpha;
            }
            if score >= beta {
                return beta;
            }
            if score > alpha {
                alpha = score;
            }
            checks_budget = checks_budget.saturating_sub(1);
            if checks_budget == 0 {
                break;
            }
        }
    }

    // === Add: checking drops (limited) ===
    // First, skip generation if stand pat is too far below alpha
    if *QS_CHECKS_ENABLED && stand_pat + QS_CHECK_ENABLE_MARGIN >= alpha && checks_budget > 0 {
        // all_moves already generated above (generate_all). Extract checking drops.
        let them = pos.side_to_move.opposite();
        let ksq_opt = pos.board.king_square(them);

        if let Some(ksq) = ksq_opt {
            // Extract checking drops (use SmallVec to avoid heap allocation for small lists)
            let mut check_drops: SmallVec<[Move; 16]> = all_moves
                .iter()
                .copied()
                .filter(|&mv| mv.is_drop())
                // Proximity and alignment filter to reduce gives_check calls
                // Exception: sliders (Rook, Bishop, Lance) have special alignment checks
                .filter(|&mv| {
                    let to = mv.to();
                    let dx = ksq.file().abs_diff(to.file());
                    let dy = ksq.rank().abs_diff(to.rank());

                    // drop_piece_type() returns PieceType directly for drop moves
                    match mv.drop_piece_type() {
                        PieceType::Rook => dx == 0 || dy == 0, // Must be aligned horizontally or vertically
                        PieceType::Bishop => dx == dy,         // Must be diagonally aligned
                        PieceType::Lance => dx == 0 && dy <= CHECK_DROP_NEAR_KING_DIST + 1, // Vertically aligned with distance limit
                        _ => dx + dy <= CHECK_DROP_NEAR_KING_DIST, // Near king only for others
                    }
                })
                // Final check: actually gives check (accurate)
                .filter(|&mv| pos.gives_check(mv))
                .collect();

            // Ordering: proximity and piece hints
            check_drops.sort_by_key(|&mv| -score_check_drop_for_ordering(pos, mv));

            // Limit number of drops by both category cap and total budget
            let per_cap = MAX_QS_CHECK_DROPS.min(checks_budget);
            if check_drops.len() > per_cap {
                check_drops.truncate(per_cap);
            }

            // Search checking drops
            for &mv in check_drops.iter() {
                if searcher.context.should_stop() {
                    return alpha;
                }

                if qs_light_time_poll(searcher) {
                    return alpha;
                }

                // Budget (qnodes) strict enforcement
                if let Some(limit) = qlimit {
                    let exceeded = if let Some(ref counter) = qnodes_counter {
                        counter.load(Ordering::Acquire) >= limit
                    } else {
                        searcher.stats.qnodes >= limit
                    };
                    if exceeded {
                        return alpha;
                    }
                }

                // Drops have zero capture value, already checked margin at outer level

                if !pos.is_pseudo_legal(mv) {
                    continue;
                }

                let eval_arc = searcher.evaluator.clone();
                let mut eval_guard =
                    crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
                let undo = pos.do_move(mv);
                let score = -quiescence_search(
                    searcher,
                    pos,
                    -beta,
                    -alpha,
                    ply + 1,
                    qply.saturating_add(1),
                );
                pos.undo_move(mv, undo);
                eval_guard.undo();
                // Count only after we actually searched it
                SearchStats::bump(&mut searcher.stats.qs_check_drops, 1);

                if searcher.context.should_stop() {
                    return alpha;
                }
                if score >= beta {
                    return beta;
                }
                if score > alpha {
                    alpha = score;
                }
                checks_budget = checks_budget.saturating_sub(1);
                if checks_budget == 0 {
                    break;
                }
            }
        }
    }

    alpha
}

/// Score checking drops for move ordering in quiescence search
/// Prioritizes drops closer to the king and more valuable pieces
#[inline]
fn score_check_drop_for_ordering(pos: &Position, mv: Move) -> i32 {
    use crate::shogi::PieceType;
    let them = pos.side_to_move.opposite();
    let ksq = match pos.board.king_square(them) {
        Some(s) => s,
        None => return 0,
    };
    let to = mv.to();
    let dx = (ksq.file() as i32 - to.file() as i32).abs();
    let dy = (ksq.rank() as i32 - to.rank() as i32).abs();
    let prox = (10 - (dx + dy)).max(0); // Closer is better, clamped to 0
    let pt = mv.drop_piece_type();
    // Slightly favor attacking pieces (not too much to avoid instability)
    let atk_hint = match pt {
        PieceType::Pawn => 5,
        PieceType::Lance => 8,
        PieceType::Knight => 9,
        PieceType::Silver => 10,
        PieceType::Gold => 9,
        PieceType::Bishop => 11,
        PieceType::Rook => 12,
        PieceType::King => 0,
    };
    prox * 2 + atk_hint
}

/// Score non-capture checking moves for ordering in quiescence search
#[inline]
fn score_nocap_check_for_ordering(pos: &Position, mv: Move) -> i32 {
    let them = pos.side_to_move.opposite();
    let ksq = match pos.board.king_square(them) {
        Some(s) => s,
        None => return 0,
    };
    let to = mv.to();
    let dx = (ksq.file() as i32 - to.file() as i32).abs();
    let dy = (ksq.rank() as i32 - to.rank() as i32).abs();
    let prox = (10 - (dx + dy)).max(0);
    let atk_hint = match mv.piece_type() {
        Some(PieceType::Rook) => (dx == 0 || dy == 0) as i32 * 6 + 6,
        Some(PieceType::Bishop) => (dx == dy) as i32 * 6 + 5,
        Some(PieceType::Lance) => (dx == 0) as i32 * 4 + 3,
        Some(PieceType::Silver) => 3,
        Some(PieceType::Gold) => 3,
        Some(PieceType::Knight) => 3,
        Some(PieceType::Pawn) => 2,
        _ => 1,
    };
    prox * 2 + atk_hint
}

#[cfg(test)]
mod tests_repetition_qs {
    use super::quiescence_search;
    use crate::{
        evaluation::evaluate::MaterialEvaluator,
        search::unified::UnifiedSearcher,
        shogi::{board::Piece, board::PieceType, board::Square, Color, Position},
    };

    #[test]
    fn test_quiescence_returns_penalty_on_repetition() {
        let mut pos = Position::empty();
        // Kings
        pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        // Advantage: extra rook for side to move
        pos.board.put_piece(
            Square::from_usi_chars('9', 'i').unwrap(),
            Piece::new(PieceType::Rook, Color::Black),
        );
        pos.side_to_move = Color::Black;

        // Sync hash and history for repetition detection
        pos.hash = pos.compute_hash();
        pos.zobrist_hash = pos.hash;
        let h = pos.zobrist_hash;
        pos.history = vec![h, h, h, h];

        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);

        let alpha = -10000;
        let beta = 10000;
        let score = quiescence_search(&mut searcher, &mut pos, alpha, beta, 0, 0);

        assert!(
            score < 0,
            "QS repetition should return a negative penalty when ahead, got {score}"
        );
    }

    #[test]
    fn test_quiescence_repetition_when_behind_returns_zero() {
        let mut pos = Position::empty();
        // Kings
        pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        // Opponent advantage: white rook
        pos.board.put_piece(
            Square::from_usi_chars('1', 'b').unwrap(),
            Piece::new(PieceType::Rook, Color::White),
        );
        pos.side_to_move = Color::Black; // Black is behind

        pos.hash = pos.compute_hash();
        pos.zobrist_hash = pos.hash;
        let h = pos.zobrist_hash;
        pos.history = vec![h, h, h, h];

        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);

        let alpha = -10000;
        let beta = 10000;
        let score = quiescence_search(&mut searcher, &mut pos, alpha, beta, 0, 0);

        assert_eq!(score, 0, "QS repetition when behind should return 0, got {score}");
    }
}
