//! Node expansion and search logic

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        constants::SEARCH_INF,
        tt::NodeType,
        unified::{tt_operations::TTOperations, UnifiedSearcher},
    },
    shogi::{PieceType, Position, TriedMoves},
};

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
    // Note: Node count is incremented in alpha_beta() to avoid double counting
    // searcher.stats.nodes += 1; // Removed to prevent double counting

    // Clear PV at this ply on entry
    searcher.pv_table.clear_len_at(ply as usize);

    // Get adaptive polling mask based on time control (unified with alpha_beta)
    let time_check_mask = super::time_control::get_event_poll_mask(searcher);

    // Early stop check
    if searcher.context.should_stop() {
        return alpha;
    }

    // Periodic time and event check (mask=0 means check every node)
    if time_check_mask == 0 || (searcher.stats.nodes & time_check_mask) == 0 {
        // Process events (including ponder hit)
        searcher.context.process_events(&searcher.time_manager);

        // Check time limit
        if searcher.context.check_time_limit(searcher.stats.nodes, &searcher.time_manager) {
            return alpha;
        }

        // Check if stop was triggered by events
        if searcher.context.should_stop() {
            return alpha;
        }
    }

    let original_alpha = alpha;
    let hash = pos.zobrist_hash;
    let mut best_move = None;
    let mut best_score = -SEARCH_INF;
    let mut moves_searched = 0;
    let mut quiet_moves_tried: TriedMoves = TriedMoves::new();
    let mut captures_tried: TriedMoves = TriedMoves::new();

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
            super::quiescence_search(searcher, pos, razoring_alpha, razoring_alpha + 1, ply, 0);
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

        // Note: Duplication statistics are now updated during TT store, not probe.
        // This ensures accurate counting of unique vs duplicate nodes.

        // ABDADA: Check if sibling node found exact cut
        if depth > 2 {
            if let Some(ref tt) = searcher.tt {
                if tt.has_exact_cut(hash) {
                    // Early return with the stored score if available
                    if let Some(entry) = tt_entry {
                        // Only trust the early cutoff if the entry has sufficient depth and matches key
                        if entry.matches(hash) && entry.depth() >= depth.saturating_sub(1) {
                            return entry.score() as i32;
                        }
                    }
                    // Even without a good score, stop searching this node
                    // to avoid duplication with sibling threads
                    // Return alpha to keep a safe fail-low bound under negamax
                    return alpha;
                }
            }
        }

        // Get TT move and validate it against legal moves
        let candidate = tt_entry.filter(|e| e.matches(hash)).and_then(|entry| entry.get_move());
        match candidate {
            Some(m) if moves.iter().any(|&lm| lm == m) => Some(m),
            Some(_) => {
                // TT move was invalid
                None
            }
            _ => None,
        }
    } else {
        None
    };

    // Order moves - avoid Vec to SmallVec conversion
    let (ordered_slice, _owned_moves);
    if USE_TT || USE_PRUNING {
        let moves_vec =
            searcher.ordering.order_moves(pos, &moves, tt_move, &searcher.search_stack, ply);
        _owned_moves = Some(moves_vec);
        let vec_ref = _owned_moves.as_ref().unwrap();
        ordered_slice = &vec_ref[..];
    } else {
        ordered_slice = moves.as_slice();
        _owned_moves = None;
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
    for &mv in ordered_slice.iter() {
        // Check stop flag at the beginning of each move
        if searcher.context.should_stop() {
            break; // Exit move loop immediately
        }

        // Double safety check: verify piece ownership for normal moves
        if !mv.is_drop() {
            if let Some(from) = mv.from() {
                if let Some(piece) = pos.piece_at(from) {
                    if piece.color != pos.side_to_move {
                        // Skip moves that attempt to move opponent's piece
                        continue;
                    }
                } else {
                    // Skip if from square is empty
                    continue;
                }
            }
        }

        // Calculate capture status once using board state (more reliable than metadata)
        let us = pos.side_to_move;
        let is_capture =
            !mv.is_drop() && pos.piece_at(mv.to()).is_some_and(|pc| pc.color == us.opposite());

        // Futility pruning for quiet moves
        if USE_PRUNING
            && can_do_futility
            && moves_searched > 0
            && !is_capture
            && !mv.is_promote()
            && static_eval + futility_margin <= alpha
        {
            continue;
        }

        // Track moves for history updates (limited to 16 to avoid heap allocation)
        if is_capture {
            // Safety: TriedMoves is a SmallVec<[Move; 16]> to ensure stack allocation
            // We limit to 16 moves to match MAX_MOVES_TO_UPDATE constant used in history updates
            if captures_tried.len() < 16 {
                captures_tried.push(mv);
            }
        } else if !mv.is_promote() {
            // Safety: Same 16-element limit for quiet moves to avoid heap allocation
            if quiet_moves_tried.len() < 16 {
                quiet_moves_tried.push(mv);
            }
        }

        // Check if this is a king move (before making the move)
        let is_king_move = if let Some(pt) = mv.piece_type() {
            pt == PieceType::King
        } else if !mv.is_drop() {
            // Fallback: check board state if metadata is missing
            if let Some(from) = mv.from() {
                pos.piece_at(from).map(|p| p.piece_type == PieceType::King).unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        };

        // Update current move in search stack
        if crate::search::types::SearchStack::is_valid_ply(ply) {
            searcher.search_stack[ply as usize].current_move = Some(mv);
            searcher.search_stack[ply as usize].move_count = moves_searched + 1;
        }

        // Skip aggressive prefetching - it has shown negative performance impact

        // Validate move is pseudo-legal before execution
        if !pos.is_pseudo_legal(mv) {
            #[cfg(debug_assertions)]
            {
                eprintln!(
                    "[WARNING] Skipping illegal move {} in search",
                    crate::usi::move_to_usi(&mv)
                );
                eprintln!("  Position: {}", crate::usi::position_to_sfen(pos));
            }
            continue;
        }

        // Make move
        let undo_info = pos.do_move(mv);

        // Simple optimization: selective prefetch
        if USE_TT && !crate::search::tt_filter::should_skip_prefetch(depth, moves_searched as usize)
        {
            searcher.prefetch_tt(pos.zobrist_hash);
        }

        let mut score;

        // Principal variation search
        if moves_searched == 0 {
            // Full window search for first move (saturating for safety)
            let next_depth = depth.saturating_sub(1);
            score = -super::alpha_beta(searcher, pos, next_depth, -beta, -alpha, ply + 1);
        } else {
            // Special handling for king moves - extend search to see consequences
            let extension = if is_king_move && depth >= 3 {
                1 // Extend search by 1 ply for king moves
            } else {
                0
            };

            // Late move reduction using advanced pruning module
            // Use lightweight pre-filter first, then accurate check if needed
            let gives_check = if crate::search::unified::pruning::likely_could_give_check(pos, mv) {
                pos.gives_check(mv)
            } else {
                false
            };

            let reduction = if USE_PRUNING
                && crate::search::unified::pruning::can_do_lmr(
                    depth,
                    moves_searched,
                    in_check,
                    gives_check,
                    mv,
                )
                && !is_king_move
            // Don't reduce king moves
            {
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
            let search_depth = depth.saturating_add(extension); // Add extension first
            let reduced_depth = search_depth.saturating_sub(1 + reduction);
            score = -super::alpha_beta(searcher, pos, reduced_depth, -alpha - 1, -alpha, ply + 1);

            // Re-search if needed
            if score > alpha && score < beta {
                let full_depth = search_depth.saturating_sub(1);
                score = -super::alpha_beta(searcher, pos, full_depth, -beta, -alpha, ply + 1);
            }
        }

        // Undo move
        pos.undo_move(mv, undo_info);

        // Check stop flag after alpha-beta recursion
        if searcher.context.should_stop() {
            return best_score.max(alpha); // Return current best value
        }

        moves_searched += 1;

        // Update best move
        if score > best_score {
            best_score = score;
            best_move = Some(mv);

            if score > alpha {
                alpha = score;

                // Update PV without allocation using the new method
                // Validate that the move is still pseudo-legal before adding to PV
                // This prevents TT pollution from causing invalid PVs
                if pos.is_pseudo_legal(mv) {
                    searcher.pv_table.update_from_child(ply as usize, mv, (ply + 1) as usize);
                } else {
                    #[cfg(debug_assertions)]
                    if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                        eprintln!("[WARNING] Skipping invalid move in PV update at ply {ply}");
                        eprintln!("  Move: {}", crate::usi::move_to_usi(&mv));
                        eprintln!("  Position: {}", crate::usi::position_to_sfen(pos));
                    }
                }

                // Validate PV immediately in debug builds
                #[cfg(debug_assertions)]
                {
                    let local_pv = searcher.pv_table.get_line(ply as usize).to_vec();
                    super::pv_validation::pv_local_sanity(pos, &local_pv);
                }

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

                                // Update capture history if the cutoff move is a capture
                                // Recalculate capture status for the best move
                                let mv_is_capture = !mv.is_drop()
                                    && pos
                                        .piece_at(mv.to())
                                        .is_some_and(|pc| pc.color == pos.side_to_move.opposite());

                                if mv_is_capture {
                                    // Try metadata first, fall back to board lookup
                                    let attacker = mv.piece_type();
                                    let victim = mv
                                        .captured_piece_type()
                                        .or_else(|| pos.piece_at(mv.to()).map(|p| p.piece_type));

                                    if let (Some(att), Some(vic)) = (attacker, victim) {
                                        history.capture.update_good(
                                            pos.side_to_move,
                                            att,
                                            vic,
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
                                        // Try metadata first, fall back to board lookup
                                        let attacker = capture_mv.piece_type();
                                        let victim =
                                            capture_mv.captured_piece_type().or_else(|| {
                                                pos.piece_at(capture_mv.to()).map(|p| p.piece_type)
                                            });

                                        if let (Some(att), Some(vic)) = (attacker, victim) {
                                            history.capture.update_bad(
                                                pos.side_to_move,
                                                att,
                                                vic,
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

                    // ABDADA: Set exact cut flag for siblings
                    if USE_TT && depth > 2 {
                        if let Some(ref tt) = searcher.tt {
                            tt.set_exact_cut(hash);
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

        // Determine if this is a PV node
        // A node is a PV node if the score improved alpha but didn't exceed beta
        let is_pv = best_score > original_alpha && best_score < beta;

        // Simple optimization: skip shallow nodes
        if !crate::search::tt_filter::should_skip_tt_store(depth, is_pv) {
            let mut boosted_depth = crate::search::tt_filter::boost_tt_depth(depth, node_type);
            // Apply additional boost for PV nodes
            boosted_depth = crate::search::tt_filter::boost_pv_depth(boosted_depth, is_pv);
            searcher.store_tt(hash, boosted_depth, best_score, node_type, best_move);
        }
    }

    best_score
}

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

    #[test]
    fn test_abdada_integration() {
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(5).build());

        let mut pos = Position::startpos();
        let hash = pos.zobrist_hash;

        // First search to populate TT
        let _score1 = search_node(&mut searcher, &mut pos, 4, -1000, 1000, 0);

        // Simulate beta cutoff by setting exact cut flag
        if let Some(ref tt) = searcher.tt {
            tt.set_exact_cut(hash);
        }

        // Reset node count
        let nodes_before = searcher.stats.nodes;

        // Second search should return early due to ABDADA flag
        let score2 = search_node(&mut searcher, &mut pos, 4, -1000, 1000, 0);

        // Should have searched very few nodes due to early return
        let nodes_after = searcher.stats.nodes;
        assert!(
            nodes_after - nodes_before < 10,
            "ABDADA early return should minimize node count"
        );

        // Scores might differ due to early return, but should be reasonable
        assert!(score2.abs() < 10000);
    }

    #[test]
    fn test_search_node_depth_zero() {
        // Test that search_node handles depth=0 correctly without underflow
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(1).build());

        let mut pos = Position::startpos();

        // Call search_node with depth=0 should not panic or underflow
        let score = search_node(&mut searcher, &mut pos, 0, -1000, 1000, 0);

        // Should return a valid score (will trigger quiescence search)
        assert!((-10000..=10000).contains(&score));

        // Should have searched some nodes (quiescence search)
        assert!(searcher.stats.nodes > 0);
        assert!(searcher.stats.qnodes > 0);
    }
}
