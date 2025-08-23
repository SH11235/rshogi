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
use std::sync::atomic::Ordering;

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
    // Increment node count here (not in search_node to avoid double counting)
    searcher.stats.nodes += 1;

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
        ) && searcher.stats.nodes % 10000 == 0
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
                    tt.incremental_gc(gc_batch_size);
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

    // Check for draw
    if pos.is_draw() {
        return 0;
    }

    // Probe transposition table
    let hash = pos.zobrist_hash;
    if USE_TT {
        if let Some(tt_entry) = searcher.probe_tt(hash) {
            // Verify key match before using entry
            if tt_entry.matches(hash) {
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
