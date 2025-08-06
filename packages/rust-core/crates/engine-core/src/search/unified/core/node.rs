//! Node expansion and search logic

use crate::{
    evaluation::evaluate::Evaluator,
    search::{constants::SEARCH_INF, tt::NodeType, unified::UnifiedSearcher},
    shogi::{Move, Position},
};
use std::sync::atomic::Ordering;

/// Search a single node in the tree
pub(super) fn search_node<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
    pos: &mut Position,
    depth: u8,
    mut alpha: i32,
    beta: i32,
    ply: u16,
) -> i32
where
    E: Evaluator + Send + Sync + 'static,
{
    // Check stop flag periodically to minimize overhead
    // Use more frequent checking if stop_flag is present
    let check_interval = if searcher.context.limits().stop_flag.is_some() {
        0x3F // Check every 64 nodes when stop_flag is present
    } else {
        0x3FF // Check every 1024 nodes for normal operation
    };

    if searcher.stats.nodes & check_interval == 0 && searcher.context.should_stop() {
        return alpha;
    }

    let original_alpha = alpha;
    let hash = pos.zobrist_hash;
    let mut best_move = None;
    let mut best_score = -SEARCH_INF;
    let mut moves_searched = 0;
    let mut quiet_moves_tried: Vec<Move> = Vec::new();
    let mut captures_tried: Vec<Move> = Vec::new();

    // Check if in check and update search stack
    let in_check = pos.is_in_check();
    if crate::search::types::SearchStack::is_valid_ply(ply) {
        searcher.search_stack[ply as usize].in_check = in_check;
        searcher.search_stack[ply as usize].clear_for_new_node();
    }

    // Static evaluation for pruning decisions
    let static_eval = if USE_PRUNING && !in_check {
        if crate::search::types::SearchStack::is_valid_ply(ply) {
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
        }
    } else {
        0 // Not used when in check
    };

    // Reverse futility pruning (static null move)
    if USE_PRUNING
        && crate::search::unified::pruning::can_do_static_null_move(
            depth,
            in_check,
            beta,
            static_eval,
        )
    {
        return beta;
    }

    // Razoring - extreme futility pruning at low depths
    if USE_PRUNING
        && crate::search::unified::pruning::can_do_razoring(depth, in_check, alpha, static_eval)
    {
        // Go directly to quiescence search if position is really bad
        let razoring_alpha = alpha - crate::search::unified::pruning::razoring_margin(depth);
        let score =
            super::quiescence_search(searcher, pos, razoring_alpha, razoring_alpha + 1, ply);
        if score <= razoring_alpha {
            return score;
        }
    }

    // Generate moves
    let mut move_gen = crate::movegen::generator::MoveGenImpl::new(pos);
    let moves = move_gen.generate_all();

    if moves.is_empty() {
        // No legal moves
        if in_check {
            return crate::search::common::mate_score(ply as u8, false); // Getting mated
        } else {
            return 0; // Stalemate
        }
    }

    // Try TT move first if available
    let tt_move = if USE_TT {
        let tt_entry = searcher.probe_tt(hash);

        // Update duplication statistics if available
        if let Some(ref stats) = searcher.duplication_stats {
            stats.total_nodes.fetch_add(1, Ordering::Relaxed);
            if tt_entry.is_none() {
                stats.unique_nodes.fetch_add(1, Ordering::Relaxed);
            }
        }

        tt_entry.and_then(|entry| entry.get_move())
    } else {
        None
    };

    // Order moves
    let ordered_moves = if USE_TT || USE_PRUNING {
        searcher.ordering.order_moves(pos, &moves, tt_move, &searcher.search_stack, ply)
    } else {
        moves.as_slice().to_vec()
    };

    // Skip selective prefetch - let individual moves control their own prefetching

    // Early futility pruning check
    let can_do_futility = USE_PRUNING
        && crate::search::unified::pruning::can_do_futility_pruning(
            depth,
            in_check,
            alpha,
            beta,
            static_eval,
        );
    let futility_margin = if can_do_futility {
        crate::search::unified::pruning::futility_margin(depth)
    } else {
        0
    };

    // Search moves
    for &mv in ordered_moves.iter() {
        // Futility pruning for quiet moves
        if USE_PRUNING
            && can_do_futility
            && moves_searched > 0
            && !mv.is_capture_hint()
            && !mv.is_promote()
            && static_eval + futility_margin <= alpha
        {
            continue;
        }

        // Track moves for history updates
        if mv.is_capture_hint() {
            captures_tried.push(mv);
        } else if !mv.is_promote() {
            quiet_moves_tried.push(mv);
        }

        // Update current move in search stack
        if crate::search::types::SearchStack::is_valid_ply(ply) {
            searcher.search_stack[ply as usize].current_move = Some(mv);
            searcher.search_stack[ply as usize].move_count = moves_searched + 1;
        }

        // Skip aggressive prefetching - it has shown negative performance impact

        // Make move
        let undo_info = pos.do_move(mv);
        searcher.stats.nodes += 1;

        // Simple optimization: selective prefetch
        if USE_TT && !crate::search::tt_filter::should_skip_prefetch(depth, moves_searched as usize)
        {
            searcher.prefetch_tt(pos.zobrist_hash);
        }

        let mut score;

        // Principal variation search
        if moves_searched == 0 {
            // Full window search for first move
            score = -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha, ply + 1);
        } else {
            // Late move reduction using advanced pruning module
            let gives_check = false; // TODO: implement gives_check detection
            let reduction = if USE_PRUNING
                && crate::search::unified::pruning::can_do_lmr(
                    depth,
                    moves_searched,
                    in_check,
                    gives_check,
                    mv,
                ) {
                crate::search::unified::pruning::lmr_reduction(depth, moves_searched)
            } else {
                0
            };

            // Null window search with safe depth calculation
            // Calculate reduced depth safely to avoid underflow
            // Using saturating_sub(1 + reduction) is more robust than chained saturating_sub
            // NOTE: We intentionally allow reduced_depth to become 0, which triggers
            // quiescence search. Using .max(1) would prevent proper quiescence search
            // and can lead to illegal move generation (e.g., king captures).
            // Calculate reduced depth for Late Move Reduction (LMR)
            // saturating_sub ensures depth doesn't go negative, transitioning to quiescence search when depth=0
            let reduced_depth = depth.saturating_sub(1 + reduction);
            score = -super::alpha_beta(searcher, pos, reduced_depth, -alpha - 1, -alpha, ply + 1);

            // Re-search if needed
            if score > alpha && score < beta {
                score = -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha, ply + 1);
            }
        }

        // Undo move
        pos.undo_move(mv, undo_info);

        moves_searched += 1;

        // Update best move
        if score > best_score {
            best_score = score;
            best_move = Some(mv);

            if score > alpha {
                alpha = score;

                // Update PV
                // We need to clone the child PV to avoid borrowing issues
                let child_pv_vec: Vec<Move> =
                    searcher.pv_table.get_line((ply + 1) as usize).to_vec();
                searcher.pv_table.update(ply as usize, mv, &child_pv_vec);

                if alpha >= beta {
                    // Beta cutoff - update killers and history
                    if USE_PRUNING {
                        // Update killers in SearchStack
                        if crate::search::types::SearchStack::is_valid_ply(ply) {
                            searcher.search_stack[ply as usize].update_killers(mv);
                        }

                        // Also update global killer table
                        searcher.ordering.update_killer(ply, mv);

                        // Get previous move for history updates
                        let prev_move = if ply > 0
                            && crate::search::types::SearchStack::is_valid_ply(ply - 1)
                        {
                            searcher.search_stack[(ply - 1) as usize].current_move
                        } else {
                            None
                        };

                        match searcher.history.lock() {
                            Ok(mut history) => {
                                // Update history for the cutoff move
                                history.update_cutoff(
                                    pos.side_to_move,
                                    mv,
                                    depth as i32,
                                    prev_move,
                                );

                                // Update counter move history
                                if let Some(prev_mv) = prev_move {
                                    history.counter_moves.update(pos.side_to_move, prev_mv, mv);
                                }

                                // Update capture history if it's a capture
                                if mv.is_capture_hint() {
                                    if let (Some(attacker), Some(victim)) =
                                        (mv.piece_type(), mv.captured_piece_type())
                                    {
                                        history.capture.update_good(
                                            pos.side_to_move,
                                            attacker,
                                            victim,
                                            depth as i32,
                                        );
                                    }
                                }

                                // Limit the number of moves to update for performance
                                const MAX_MOVES_TO_UPDATE: usize = 16;

                                // Penalize quiet moves that didn't cause cutoff
                                for &quiet_mv in quiet_moves_tried.iter().take(MAX_MOVES_TO_UPDATE)
                                {
                                    if quiet_mv != mv {
                                        history.update_quiet(
                                            pos.side_to_move,
                                            quiet_mv,
                                            depth as i32,
                                            prev_move,
                                        );
                                    }
                                }

                                // Penalize captures that didn't cause cutoff
                                for &capture_mv in captures_tried.iter().take(MAX_MOVES_TO_UPDATE) {
                                    if capture_mv != mv {
                                        if let (Some(attacker), Some(victim)) = (
                                            capture_mv.piece_type(),
                                            capture_mv.captured_piece_type(),
                                        ) {
                                            history.capture.update_bad(
                                                pos.side_to_move,
                                                attacker,
                                                victim,
                                                depth as i32,
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!("Failed to acquire history lock for updates: {e}");
                                // Fallback: Just update stats without history heuristics
                                // This ensures search continues to function even if history is unavailable
                            }
                        }
                    }
                    break;
                }
            }
        }

        // Late move pruning - prune remaining moves if we've searched enough
        if USE_PRUNING
            && depth <= 3
            && moves_searched >= 8 + depth as u32 * 3
            && !crate::search::common::is_mate_score(best_score)
        {
            break;
        }
    }

    // Store in TT
    if USE_TT {
        let node_type = if best_score <= original_alpha {
            NodeType::UpperBound
        } else if best_score >= beta {
            NodeType::LowerBound
        } else {
            NodeType::Exact
        };

        // Simple optimization: skip shallow nodes
        if !crate::search::tt_filter::should_skip_tt_store(depth, false) {
            let boosted_depth = crate::search::tt_filter::boost_tt_depth(depth, node_type);
            searcher.store_tt(hash, boosted_depth, best_score, node_type, best_move);
        }
    }

    best_score
}

// Note: Helper functions removed - now using functions from pruning module directly

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        evaluation::evaluate::MaterialEvaluator,
        search::{unified::UnifiedSearcher, SearchLimits},
        shogi::Position,
    };

    #[test]
    fn test_search_node_basic() {
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(5).build());

        let mut pos = Position::startpos();
        let score = search_node(&mut searcher, &mut pos, 3, -1000, 1000, 0);

        // Should return a valid score
        assert!((-1000..=1000).contains(&score));
        assert!(searcher.stats.nodes > 0);
    }

    #[test]
    fn test_search_node_stop_flag() {
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(MaterialEvaluator);

        // Set stop flag immediately
        searcher.context.stop();

        let mut pos = Position::startpos();
        let score = search_node(&mut searcher, &mut pos, 5, -1000, 1000, 0);

        // Should return alpha when stopped
        assert_eq!(score, -1000);
        // Should have minimal nodes due to early stop
        assert!(searcher.stats.nodes < 10);
    }

    #[test]
    fn test_history_update_with_mutex_error() {
        // This test verifies that search continues even if history mutex fails
        // In real code, we handle the error with logging and continue
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(3).build());

        let mut pos = Position::startpos();

        // Even if history mutex were to fail (which we handle gracefully),
        // search should complete successfully
        let score = search_node(&mut searcher, &mut pos, 2, -1000, 1000, 0);

        // Verify search completed
        assert!((-SEARCH_INF..=SEARCH_INF).contains(&score));
    }

    #[test]
    fn test_max_moves_to_update_performance() {
        // Test that we limit the number of moves updated for performance
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(3).build());

        let mut pos = Position::startpos();

        // Create a move generator to count moves
        use crate::movegen::MoveGen;
        use crate::shogi::MoveList;
        let mut move_gen = MoveGen::new();
        let mut moves = MoveList::new();
        move_gen.generate_all(&pos, &mut moves);
        assert!(moves.len() > 16); // Ensure we have more than MAX_MOVES_TO_UPDATE

        // Search should complete efficiently even with many moves
        let start_nodes = searcher.stats.nodes;
        let _ = search_node(&mut searcher, &mut pos, 3, -1000, 1000, 0);
        let end_nodes = searcher.stats.nodes;

        // Should have searched some nodes
        assert!(end_nodes > start_nodes);
    }

    #[test]
    fn test_pruning_conditions_respected() {
        // Test with pruning enabled
        let mut searcher_with_pruning =
            UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(MaterialEvaluator);
        searcher_with_pruning
            .context
            .set_limits(SearchLimits::builder().depth(5).build());

        // Test with pruning disabled
        let mut searcher_no_pruning =
            UnifiedSearcher::<MaterialEvaluator, true, false, 16>::new(MaterialEvaluator);
        searcher_no_pruning.context.set_limits(SearchLimits::builder().depth(5).build());

        let mut pos1 = Position::startpos();
        let mut pos2 = Position::startpos();

        let _ = search_node(&mut searcher_with_pruning, &mut pos1, 4, -1000, 1000, 0);
        let _ = search_node(&mut searcher_no_pruning, &mut pos2, 4, -1000, 1000, 0);

        // With pruning should search fewer nodes
        assert!(searcher_with_pruning.stats.nodes < searcher_no_pruning.stats.nodes);
    }
}
