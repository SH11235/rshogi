//! Quiescence search implementation
//!
//! Handles capture sequences to avoid horizon effects

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        common::mate_score,
        constants::{MAX_PLY, SEARCH_INF},
        unified::UnifiedSearcher,
    },
    shogi::{Move, Position},
};
use smallvec::SmallVec;
use std::sync::atomic::Ordering;

// Import victim_score from move_ordering module
use super::move_ordering::victim_score;

// Conservative initial values for checking drops in quiescence search
const MAX_QS_CHECK_DROPS: usize = 4; // Maximum number of checking drops to search in QS
const CHECK_DROP_MARGIN: i32 = 80; // Small margin to prune clearly losing drops
const CHECK_DROP_NEAR_KING_DIST: u8 = 2; // Manhattan distance limit for drops near king

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

    // Check if in check first - this determines our search strategy
    let in_check = pos.is_in_check();

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

    // Primary limit: relative qsearch depth
    // This ensures consistent qsearch behavior regardless of main search depth
    // Note: In check positions, we skip this limit to ensure proper check evasion
    if !in_check {
        let qply_limit = crate::search::constants::MAX_QPLY;

        if qply >= qply_limit {
            // Use rate limiter to avoid log spam
            if log::log_enabled!(log::Level::Warn)
                && super::log_rate_limiter::QUIESCE_DEPTH_LIMITER.should_log()
            {
                log::warn!("Hit relative quiescence depth limit qply={qply} (limit={qply_limit})");
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
        // Use rate limiter for this warning too
        if log::log_enabled!(log::Level::Warn)
            && super::log_rate_limiter::QUIESCE_DEPTH_LIMITER.should_log()
        {
            log::warn!("Hit absolute ply limit {ply} in quiescence search");
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
    if ply >= crate::search::constants::MAX_QUIESCE_DEPTH {
        // Use rate limiter to avoid log spam
        if log::log_enabled!(log::Level::Warn)
            && super::log_rate_limiter::QUIESCE_DEPTH_LIMITER.should_log()
        {
            log::warn!("Hit absolute quiescence depth limit at ply {ply}");
        }
        // Hard stop at reasonable depth
        // Return evaluation-based value instead of fixed constants to avoid discontinuity
        return if in_check {
            // In check positions, return a value based on evaluation but slightly pessimistic
            alpha.max(
                searcher.evaluator.evaluate(pos)
                    - crate::search::constants::QUIESCE_CHECK_EVAL_PENALTY,
            )
        } else {
            // Not in check, return current evaluation
            searcher.evaluator.evaluate(pos)
        };
    }

    if in_check {
        // In check: must search all legal moves (no stand pat)
        // Generate all legal moves
        let mut move_gen_impl = crate::movegen::generator::MoveGenImpl::new(pos);
        let moves = move_gen_impl.generate_all();

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

            // Make move
            let undo_info = pos.do_move(mv);

            // Check if still in check after the move
            let still_in_check = pos.is_in_check();
            if still_in_check {
                pos.undo_move(mv, undo_info);
                continue; // Not a valid evasion
            }

            // Recursive search (increment both ply and qply)
            let score =
                -quiescence_search(searcher, pos, -beta, -alpha, ply + 1, qply.saturating_add(1));

            // Undo move
            pos.undo_move(mv, undo_info);

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
        crate::search::unified::pruning::delta_pruning_margin()
    } else {
        0
    };

    // Generate all moves for quiescence (we'll filter captures manually)
    let mut move_gen_impl = crate::movegen::generator::MoveGenImpl::new(pos);
    let all_moves = move_gen_impl.generate_all();

    // Filter to captures only - check board state directly instead of relying on metadata
    let us = pos.side_to_move;
    let mut moves = crate::shogi::MoveList::new();
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
        moves.as_mut_vec().retain(|&mv| {
            // Check if this move should skip SEE pruning (drops, checks, etc.)
            if crate::search::unified::pruning::should_skip_see_pruning(pos, mv) {
                return true;
            }

            // Keep captures with SEE >= 0
            pos.see_ge(mv, 0)
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
            crate::search_debug!("WARNING: Skipping illegal move in quiescence search");
            crate::search_debug!("Move: {}", crate::usi::move_to_usi(&mv));
            crate::search_debug!("Position: {}", crate::usi::position_to_sfen(pos));
            continue;
        }

        // Make move
        let undo_info = pos.do_move(mv);

        // Skip prefetch in quiescence - adds overhead with minimal benefit
        // (Controlled by tt_filter module)

        // Recursive search (increment both ply and qply)
        let score =
            -quiescence_search(searcher, pos, -beta, -alpha, ply + 1, qply.saturating_add(1));

        // Undo move
        pos.undo_move(mv, undo_info);

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

    // === Add: checking drops (limited) ===
    // First, skip generation if stand pat is too far below alpha
    if stand_pat + CHECK_DROP_MARGIN >= alpha {
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
                    use crate::shogi::PieceType;
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

            // Limit number of drops
            if check_drops.len() > MAX_QS_CHECK_DROPS {
                check_drops.truncate(MAX_QS_CHECK_DROPS);
            }

            // Search checking drops
            for &mv in check_drops.iter() {
                if searcher.context.should_stop() {
                    return alpha;
                }

                // Increment qs_check_drops counter
                crate::search::SearchStats::bump(&mut searcher.stats.qs_check_drops, 1);
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

                if searcher.context.should_stop() {
                    return alpha;
                }
                if score >= beta {
                    return beta;
                }
                if score > alpha {
                    alpha = score;
                }
            }
        }
    }

    alpha
}

/// Score checking drops for move ordering in quiescence search
/// Prioritizes drops closer to the king and more valuable pieces
#[inline]
fn score_check_drop_for_ordering(pos: &Position, mv: crate::shogi::Move) -> i32 {
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
