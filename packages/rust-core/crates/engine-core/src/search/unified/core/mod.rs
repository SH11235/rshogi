//! Core search logic for unified searcher
//!
//! Implements alpha-beta search with iterative deepening

mod debug_macros;
mod log_rate_limiter;
mod move_ordering;
mod node;
mod null_move;
mod pv;
mod pv_validation;
mod quiescence;
mod root_search;
mod time_control;

pub use pv::PVTable;
pub use pv_validation::pv_local_sanity;
pub use quiescence::quiescence_search;
pub use root_search::search_root_multipv;
pub use root_search::search_root_with_window;

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        common::mate_score,
        constants::MAX_PLY,
        unified::{tt_operations::TTOperations, UnifiedSearcher},
    },
    shogi::Position,
};
#[cfg(feature = "diagnostics")]
use std::cell::Cell;
use std::sync::atomic::Ordering;
#[cfg(feature = "diagnostics")]
thread_local! {
    static AB_SHORT_CIRCUIT_LOGGED_ONCE: Cell<bool> = const { Cell::new(false) };
}

/// Compute repetition penalty based on static evaluation from the side-to-move perspective.
///
/// - If side to move is ahead (static_eval >= 50cp), return a negative penalty to discourage
///   waiting-move loops (at least -16cp, saturating at -128cp).
/// - If side to move is not ahead, return 0 (draw acceptance for the worse/level side).
#[inline]
pub(super) fn repetition_penalty(static_eval: i32) -> i32 {
    // Penalize only the side that is ahead to discourage waiting-move loops.
    // Example shape: at least -16cp, up to -128cp; otherwise 0 for draw acceptance.
    if static_eval >= 50 {
        -(16 + static_eval / 16).min(128)
    } else {
        0
    }
}

#[cfg(test)]
mod tests_repetition_penalty {
    use super::repetition_penalty;

    #[test]
    fn test_repetition_penalty_shape() {
        assert_eq!(repetition_penalty(0), 0);
        assert_eq!(repetition_penalty(49), 0);
        assert!(repetition_penalty(50) <= -16);
        // Large advantage saturates at -128cp
        assert!(repetition_penalty(16 * 128) <= -128);
        assert!(repetition_penalty(16 * 128) >= -1024); // sanity bound (not too extreme)
    }
}

#[cfg(test)]
mod tests_repetition_alpha_beta {
    use super::alpha_beta;
    use crate::{
        evaluation::evaluate::MaterialEvaluator,
        search::unified::UnifiedSearcher,
        shogi::{board::Piece, board::PieceType, board::Square, Color, Position},
    };

    #[test]
    fn test_alpha_beta_returns_penalty_on_repetition() {
        // Minimal position with kings and an extra rook for side to move
        let mut pos = Position::empty();
        let bk = Piece::new(PieceType::King, Color::Black);
        let wk = Piece::new(PieceType::King, Color::White);
        // Place kings
        pos.board.put_piece(Square::from_usi_chars('5', 'i').unwrap(), bk);
        pos.board.put_piece(Square::from_usi_chars('5', 'a').unwrap(), wk);
        // Advantage for Black: add a rook
        pos.board.put_piece(
            Square::from_usi_chars('9', 'i').unwrap(),
            Piece::new(PieceType::Rook, Color::Black),
        );
        pos.side_to_move = Color::Black;

        // Compute hash and set repetition history: 3 previous occurrences
        pos.hash = pos.compute_hash();
        pos.zobrist_hash = pos.hash;
        let h = pos.zobrist_hash;
        // Four entries to pass the early length guard and ensure >=3 matches
        pos.history = vec![h, h, h, h];

        // Build a searcher and call alpha-beta
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
        let score = alpha_beta(&mut searcher, &mut pos, 2, -10000, 10000, 0);

        // Since Black is ahead, repetition should return a negative penalty
        assert!(score < 0, "Repetition should be penalized when ahead, got {score}");
    }

    #[test]
    fn test_alpha_beta_repetition_when_behind_returns_zero() {
        // Side to move is behind: give opponent an extra rook
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
        // Opponent (White) has rook
        pos.board.put_piece(
            Square::from_usi_chars('1', 'b').unwrap(),
            Piece::new(PieceType::Rook, Color::White),
        );
        pos.side_to_move = Color::Black; // Black is behind

        // Hash & repetition history
        pos.hash = pos.compute_hash();
        pos.zobrist_hash = pos.hash;
        let h = pos.zobrist_hash;
        pos.history = vec![h, h, h, h];

        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
        let score = alpha_beta(&mut searcher, &mut pos, 2, -10000, 10000, 0);

        // Behind -> draw acceptance => 0 (within clamp)
        assert_eq!(score, 0, "Repetition when behind should return 0, got {score}");
    }
}

/// Alpha-beta search with pruning
pub(super) fn alpha_beta<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING>,
    pos: &mut Position,
    depth: u8,
    mut alpha: i32,
    beta: i32,
    ply: u16,
) -> i32
where
    E: Evaluator + Send + Sync + 'static,
{
    node::reset_near_deadline_flags();

    // Increment node count here (not in search_node to avoid double counting)
    searcher.stats.nodes += 1;

    // Hard-limit short-circuit: if we are past the hard deadline, exit immediately.
    // This complements node-based/event-mask checks and the lightweight polls,
    // and guarantees termination even on paths where node progress is sparse.
    if let Some(tm) = &searcher.time_manager {
        let hard = tm.hard_limit_ms();
        if hard > 0 && hard < u64::MAX {
            let elapsed_ms = tm.elapsed_ms();
            if elapsed_ms >= hard {
                // Mark time-based stop so outer layers (root/ID) exit immediately
                #[cfg(feature = "diagnostics")]
                AB_SHORT_CIRCUIT_LOGGED_ONCE.with(|flag| {
                    if !flag.get() {
                        log::info!(
                            "diag short_circuit ab reason=hard elapsed={}ms hard={}ms",
                            elapsed_ms,
                            hard
                        );
                        flag.set(true);
                    }
                });
                searcher.context.mark_time_stopped();
                searcher.context.stop();
                return alpha;
            }
        }
    }

    // Fast-path: if a planned rounded stop is set and we've reached it, exit immediately
    if let Some(tm) = &searcher.time_manager {
        let planned = tm.scheduled_end_ms();
        if planned > 0 && planned < u64::MAX {
            let elapsed_ms = tm.elapsed_ms();
            if elapsed_ms >= planned {
                // Planned rounded stop reached – signal stop for consistent unwind
                #[cfg(feature = "diagnostics")]
                AB_SHORT_CIRCUIT_LOGGED_ONCE.with(|flag| {
                    if !flag.get() {
                        log::info!(
                            "diag short_circuit ab reason=planned elapsed={}ms planned={}ms",
                            elapsed_ms,
                            planned
                        );
                        flag.set(true);
                    }
                });
                searcher.context.mark_time_stopped();
                searcher.context.stop();
                return alpha;
            }
        }
    }

    // Early stop check
    if searcher.context.should_stop() {
        return alpha;
    }

    // Get adaptive polling mask based on time control and stop conditions
    // This unified mask handles all event checking including stop_flag polling
    let event_mask = time_control::get_event_poll_mask(searcher);

    // Process events based on adaptive mask (handle mask=0 for immediate check)
    if event_mask == 0 || (searcher.stats.nodes & event_mask) == 0 {
        // Add debug logging for ponder mode
        if matches!(
            &searcher.context.limits().time_control,
            crate::time_management::TimeControl::Ponder(_)
        ) && searcher.stats.nodes.is_multiple_of(10000)
        {
            log::debug!(
                "Ponder search: checking events at {} nodes (mask: {:#x})",
                searcher.stats.nodes,
                event_mask
            );
        }

        searcher.context.process_events(&searcher.time_manager);

        // Check both time limit and stop flag using unified method
        if searcher.context.check_time_limit(searcher.stats.nodes, &searcher.time_manager)
            || searcher.context.should_stop()
        {
            return alpha;
        }
    }

    // Check for transposition table garbage collection
    #[cfg(feature = "hashfull_filter")]
    if USE_TT {
        const GC_CHECK_INTERVAL: u64 = 0xFFF; // Check every 4096 nodes
        if (searcher.stats.nodes & GC_CHECK_INTERVAL) == 0 {
            if let Some(ref tt) = searcher.tt {
                if tt.should_trigger_gc() {
                    // Adjust batch size based on search depth
                    let gc_batch_size = if depth < 10 { 128 } else { 256 };
                    tt.perform_incremental_gc(gc_batch_size);
                }
            }
        }
    }

    // Absolute depth limit to prevent stack overflow
    if ply >= MAX_PLY as u16 {
        // Use rate limiter to avoid log spam during deep searches
        if log::log_enabled!(log::Level::Warn)
            && log_rate_limiter::QUIESCE_DEPTH_LIMITER.should_log()
        {
            log::warn!("Hit absolute ply limit {ply} in alpha-beta search");
        }
        let eval = searcher.evaluator.evaluate(pos);
        return eval;
    }

    // Mate distance pruning
    if USE_PRUNING {
        alpha = alpha.max(mate_score(ply as u8, false)); // Getting mated
                                                         // Rebind local beta for mate-distance pruning (do not mutate param)
        let beta = beta.min(mate_score((ply + 1) as u8, true)); // Giving mate
        if alpha >= beta {
            return alpha;
        }
    }

    // Path-dependent repetition handling BEFORE TT probing
    if pos.is_repetition() {
        // Use stack-cached static eval if available to avoid double evaluation
        let static_eval = if crate::search::types::SearchStack::is_valid_ply(ply) {
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

        let score = repetition_penalty(static_eval).clamp(alpha, beta);
        return score;
    }

    // Probe transposition table
    let hash = pos.zobrist_hash;
    if USE_TT {
        if let Some(tt_entry) = searcher.probe_tt(hash) {
            // Verify key match before using entry
            if tt_entry.matches(hash) {
                // Count TT hit for observability
                crate::search::SearchStats::bump(&mut searcher.stats.tt_hits, 1);
                // Track duplication stats
                if let Some(ref stats) = searcher.duplication_stats {
                    // TT hit doesn't necessarily mean duplication
                    // A position can be in TT from a different path without being a duplicate
                    // For accurate duplicate detection, we'd need to track visited positions in current search
                    // For now, we only count as duplicate if depth is sufficient (heuristic)
                    let is_duplicate = tt_entry.depth() >= depth;
                    stats.add_node(true, is_duplicate); // is_tt_hit=true, is_duplicate based on depth
                }

                if tt_entry.depth() >= depth {
                    let tt_score = tt_entry.score();
                    // Adjust mate scores from TT (stored relative to root) to current ply
                    let adjusted_score = crate::search::common::adjust_mate_score_from_tt(
                        tt_score as i32,
                        ply as u8,
                    );

                    // If TT entry contains a mate score, update mate distance
                    if crate::search::common::is_mate_score(adjusted_score) {
                        if let Some(distance) =
                            crate::search::common::extract_mate_distance(adjusted_score)
                        {
                            // Adjust distance for current ply
                            let root_distance = distance.saturating_add(ply as u8);
                            searcher.context.update_mate_distance(root_distance);
                        }
                    }

                    match tt_entry.node_type() {
                        // EXACT entries contain the true score for this position
                        crate::search::NodeType::Exact => return adjusted_score,
                        // LOWERBOUND means the true score is >= tt_score
                        // We can use it for cutoff if it's >= beta
                        crate::search::NodeType::LowerBound => {
                            if adjusted_score >= beta {
                                return adjusted_score;
                            }
                            // We can also improve alpha since we know score >= tt_score
                            alpha = alpha.max(adjusted_score);
                        }
                        // UPPERBOUND means the true score is <= tt_score
                        // We can use it for cutoff if it's <= alpha
                        crate::search::NodeType::UpperBound => {
                            if adjusted_score <= alpha {
                                return adjusted_score;
                            }
                        }
                    }
                }
            } else {
                // Key mismatch - treat as no TT hit
                if let Some(ref stats) = searcher.duplication_stats {
                    stats.add_node(false, false);
                }
            }
        } else {
            // No TT hit - this is a unique node
            if let Some(ref stats) = searcher.duplication_stats {
                stats.add_node(false, false);
            }
        }
    }

    // Quiescence search at leaf nodes
    if depth == 0 {
        // Check if immediate evaluation is requested
        if searcher.context.limits().immediate_eval_at_depth_zero {
            // Return static evaluation immediately
            if pos.is_in_check() {
                // In check: return alpha (same as quiescence search behavior)
                return alpha;
            } else {
                // Not in check: return clamped evaluation
                let eval = searcher.evaluator.evaluate(pos);
                return eval.clamp(alpha, beta);
            }
        }

        // Check QNodes budget before entering quiescence search
        if let Some(limit) = searcher.context.limits().qnodes_limit {
            let exceeded = if let Some(ref counter) = searcher.context.limits().qnodes_counter {
                // Parallel search: check shared counter
                counter.load(Ordering::Relaxed) >= limit
            } else {
                // Single-threaded: check local counter
                searcher.stats.qnodes >= limit
            };

            if exceeded {
                // Return evaluation consistent with quiescence search behavior
                if pos.is_in_check() {
                    // In check: return alpha (same as quiescence search)
                    return alpha;
                } else {
                    // Not in check: return clamped evaluation
                    let eval = searcher.evaluator.evaluate(pos);
                    return eval.clamp(alpha, beta);
                }
            }
        }

        return quiescence::quiescence_search(searcher, pos, alpha, beta, ply, 0);
    }

    // Null move pruning
    if USE_PRUNING {
        // Get static eval for null move decision
        let static_eval = if crate::search::types::SearchStack::is_valid_ply(ply) {
            let stack_entry = &mut searcher.search_stack[ply as usize];
            if let Some(cached_eval) = stack_entry.static_eval {
                cached_eval
            } else {
                let eval = searcher.evaluator.evaluate(pos);
                stack_entry.static_eval = Some(eval);
                eval
            }
        } else {
            searcher.evaluator.evaluate(pos)
        };

        if crate::search::unified::pruning::can_do_null_move(
            pos,
            depth,
            pos.is_in_check(),
            beta,
            static_eval,
        ) {
            if let Some(score) = null_move::try_null_move(searcher, pos, depth, beta, ply) {
                return score;
            }
        }
    }

    // Regular alpha-beta search
    node::search_node(searcher, pos, depth, alpha, beta, ply)
}

#[cfg(test)]
mod tests;
