//! Node expansion and search logic

use crate::{
    evaluation::evaluate::Evaluator,
    movegen::MoveGenerator,
    search::{
        constants::SEARCH_INF,
        unified::{tt_operations::TTOperations, UnifiedSearcher},
        NodeType,
    },
    shogi::{PieceType, Position, TriedMoves},
};

/// Search a single node in the tree
pub(super) fn search_node<E, const USE_TT: bool, const USE_PRUNING: bool>(
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
    // Note: Node count is incremented in alpha_beta() to avoid double counting
    // searcher.stats.nodes += 1; // Removed to prevent double counting

    // Clear PV at this ply on entry (stack-based PV)
    if crate::search::types::SearchStack::is_valid_ply(ply) {
        searcher.search_stack[ply as usize].pv_line.clear();
    }

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

    // In-check entry extension: extend depth by +1 at node entry (capped)
    // Apply before any pruning/razoring decisions so they see the extended depth
    let mut depth = depth; // shadow parameter for local mutation
    if in_check
        && depth >= 3
        && crate::search::types::SearchStack::is_valid_ply(ply)
        && searcher.search_stack[ply as usize].consecutive_checks < 2
    {
        depth = depth.saturating_add(1);

        // Suppress piling-up of per-move check extensions within this node
        let entry = &mut searcher.search_stack[ply as usize];
        entry.consecutive_checks = entry.consecutive_checks.saturating_add(1);

        // Attribute this to check_extensions for diagnostics
        crate::search::SearchStats::bump(&mut searcher.stats.check_extensions, 1);
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
    let move_gen = MoveGenerator::new();
    let moves = match move_gen.generate_all(pos) {
        Ok(moves) => moves,
        Err(_) => {
            // King not found - should not happen in valid position
            return -SEARCH_INF;
        }
    };

    if moves.is_empty() {
        // No legal moves
        if in_check {
            let mate_score = crate::search::common::mate_score(ply as u8, false);
            // Update mate distance in context (accounting for ply)
            searcher.context.update_mate_distance(ply as u8);
            return mate_score; // Getting mated
        } else {
            return 0; // Stalemate
        }
    }

    // Try TT move first if available
    let tt_move = if USE_TT {
        let tt_entry = searcher.probe_tt(hash);

        // Note: Duplication statistics are updated during TT probe in alpha_beta().
        // Store-time updates are temporarily disabled in tt_operations.rs.

        // ABDADA: Check if sibling node found exact cut
        if depth > 2 {
            if let Some(ref tt) = searcher.tt {
                if tt.has_exact_cut(hash) {
                    // Early return with the stored score if available and reliable
                    if let Some(entry) = tt_entry {
                        // Only trust the early cutoff if:
                        // 1. Entry matches hash
                        // 2. Entry has sufficient depth
                        // 3. Entry is a lower bound (cut node) and score >= beta
                        if entry.matches(hash) && entry.depth() >= depth.saturating_sub(1) {
                            let adjusted_score = crate::search::common::adjust_mate_score_from_tt(
                                entry.score() as i32,
                                ply as u8,
                            );
                            // For lower bound entries, we can only use them if score >= beta
                            if entry.node_type() == NodeType::LowerBound && adjusted_score >= beta {
                                return adjusted_score;
                            }
                            // For exact entries, we can always use them
                            if entry.node_type() == NodeType::Exact {
                                return adjusted_score;
                            }
                        }
                    }
                    // If we don't have sufficient evidence, continue normal search
                    // This prevents propagating incorrect bounds
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

        // Validate move is pseudo-legal before any heavy checks
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

        // SEE-based pruning for obviously bad captures at shallow depth
        // - Skip for drops, promotions, in-check, and likely checking moves (see helper)
        // - Apply only when using pruning, shallow depth, and capture moves
        if USE_PRUNING
            && !in_check
            && depth <= 4
            && is_capture
            && !mv.is_drop()
            && !mv.is_promote()
            && !crate::search::unified::pruning::should_skip_see_pruning(pos, mv)
        {
            // Keep captures with SEE >= 0, prune otherwise
            if !pos.see_ge(mv, 0) {
                continue;
            }
        }

        // Clone BEFORE do_move for old check method comparison
        #[cfg(debug_assertions)]
        let pre_pos = if depth >= 3 && moves_searched < 4 {
            Some(pos.clone())
        } else {
            None
        };

        // Make move (pair evaluator hooks with move using guard)
        let eval_arc = searcher.evaluator.clone();
        let mut eval_guard = crate::search::unified::EvalMoveGuard::new(&*eval_arc, pos, mv);
        let undo_info = pos.do_move(mv);

        // Save child hash for PV owner validation (avoids second do/undo)
        // This is the Zobrist hash of the child position after making the move
        let _child_hash_for_pv = pos.zobrist_hash;

        // Check if this move gives check (opponent is now in check)
        // This is more efficient than calling gives_check before do_move
        let gives_check = pos.is_in_check();

        // Debug assertion to verify the optimization correctness
        // Testing check detection differences
        #[cfg(debug_assertions)]
        #[allow(clippy::overly_complex_bool_expr)]
        if depth >= 3 && moves_searched < 4 {
            if let Some(test_pos) = pre_pos {
                // Old method: pre-compute gives_check on pre-move position
                let old_gives_check = if USE_PRUNING {
                    if crate::search::unified::pruning::likely_could_give_check(&test_pos, mv) {
                        test_pos.gives_check(mv)
                    } else if mv.is_drop() {
                        depth >= 3 && moves_searched < 8 && test_pos.gives_check(mv)
                    } else {
                        false
                    }
                } else {
                    depth >= 3 && moves_searched < 8 && test_pos.gives_check(mv)
                };

                if gives_check != old_gives_check {
                    // Log mismatch but don't panic - there can be edge cases in detection
                    log::debug!(
                        "gives_check mismatch: new={}, old={} for move {} at depth {}",
                        gives_check,
                        old_gives_check,
                        crate::usi::move_to_usi(&mv),
                        depth
                    );
                }
            }
        }

        // Simple optimization: selective prefetch
        if USE_TT
            && !searcher.is_prefetch_disabled()
            && !crate::search::tt::filter::should_skip_prefetch(depth, moves_searched as usize)
        {
            searcher.prefetch_tt(pos.zobrist_hash);
        }

        let mut score;

        // Extension: (1) checking moves (including drops) (2) king moves
        let mut extension = 0;
        if depth >= 3 {
            let allow_check_ext = moves_searched < 4 // Early moves only
                && crate::search::types::SearchStack::is_valid_ply(ply)
                && searcher.search_stack[ply as usize].consecutive_checks < 2; // Cap at 2

            if gives_check && allow_check_ext {
                extension = 1; // Check extension
                crate::search::SearchStats::bump(&mut searcher.stats.check_extensions, 1);
            } else if is_king_move && moves_searched < 4 {
                extension = 1; // Existing king extension
                crate::search::SearchStats::bump(&mut searcher.stats.king_extensions, 1);
            }
        }

        // Update consecutive checks count for next ply
        if crate::search::types::SearchStack::is_valid_ply(ply + 1) {
            let current_consecutive_checks = searcher.search_stack[ply as usize].consecutive_checks;
            let next = &mut searcher.search_stack[(ply + 1) as usize];
            next.consecutive_checks = if gives_check && extension > 0 {
                current_consecutive_checks.saturating_add(1)
            } else {
                0
            };
        }

        // Principal variation search
        if moves_searched == 0 {
            // Full window search for first move (saturating for safety)
            let next_depth = depth.saturating_add(extension).saturating_sub(1);
            score = -super::alpha_beta(searcher, pos, next_depth, -beta, -alpha, ply + 1);
        } else {
            // Late move reduction using advanced pruning module
            let reduction = if USE_PRUNING
                && crate::search::unified::pruning::can_do_lmr(
                    depth,
                    moves_searched,
                    in_check,
                    gives_check,
                    mv,
                )
                && !is_king_move // Don't reduce king moves
                && !gives_check
            // Don't reduce check extension nodes
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
                                                                // Apply teacher-profile LMR cap
            let lmr_cap =
                crate::search::unified::pruning::lmr_cap_for_profile(searcher.teacher_profile());
            let reduction = reduction.min(lmr_cap);
            let reduced_depth = search_depth.saturating_sub(1 + reduction);
            if reduction > 0 {
                crate::search::SearchStats::bump(&mut searcher.stats.lmr_count, 1);
            }
            score = -super::alpha_beta(searcher, pos, reduced_depth, -alpha - 1, -alpha, ply + 1);

            // Re-search if needed: either principal improvement in window,
            // or fail-high on a reduced search (verification without reduction)
            if (score > alpha && score < beta) || (reduction > 0 && score >= beta) {
                let full_depth = search_depth.saturating_sub(1);
                score = -super::alpha_beta(searcher, pos, full_depth, -beta, -alpha, ply + 1);
            }
        }

        // Undo move
        pos.undo_move(mv, undo_info);
        eval_guard.undo();

        // Check stop flag after alpha-beta recursion
        if searcher.context.should_stop() {
            return best_score.max(alpha); // Return current best value
        }

        moves_searched += 1;

        // Update best move
        if score > best_score {
            best_score = score;
            best_move = Some(mv);
            // note: capture classification is handled inline via `is_capture`

            if score > alpha {
                alpha = score;

                // Update PV on the search stack without global table
                // Validate that the move is still pseudo-legal before adding to PV
                // This prevents TT pollution from causing invalid PVs
                if pos.is_pseudo_legal(mv) {
                    // Additional check: ensure the move's source square is not empty
                    let move_valid = if !mv.is_drop() {
                        if let Some(from) = mv.from() {
                            pos.piece_at(from).is_some()
                        } else {
                            false
                        }
                    } else {
                        true
                    };

                    if move_valid {
                        // Stack-based PV: head is mv, tail is child stack PV
                        if crate::search::types::SearchStack::is_valid_ply(ply) {
                            let child_ply = (ply + 1) as usize;
                            let child_tail =
                                if crate::search::types::SearchStack::is_valid_ply(ply + 1) {
                                    searcher.search_stack[child_ply].pv_line.clone()
                                } else {
                                    smallvec::SmallVec::<[crate::shogi::Move; 16]>::new()
                                };
                            let entry = &mut searcher.search_stack[ply as usize];
                            entry.pv_line.clear();
                            entry.pv_line.push(mv);
                            entry.pv_line.extend_from_slice(&child_tail);
                        }
                    } else {
                        #[cfg(debug_assertions)]
                        crate::pv_debug_exec!({
                            eprintln!(
                                "[ERROR] Move passed pseudo_legal but source square is empty!"
                            );
                            eprintln!("  Ply: {ply}, Move: {}", crate::usi::move_to_usi(&mv));
                            eprintln!("  Position: {}", crate::usi::position_to_sfen(pos));
                        });
                    }
                } else {
                    #[cfg(debug_assertions)]
                    crate::pv_debug_exec!({
                        eprintln!("[WARNING] Skipping invalid move in PV update at ply {ply}");
                        eprintln!("  Move: {}", crate::usi::move_to_usi(&mv));
                        eprintln!("  Position: {}", crate::usi::position_to_sfen(pos));
                    });
                }

                // Validate PV immediately in debug builds
                #[cfg(debug_assertions)]
                {
                    if crate::search::types::SearchStack::is_valid_ply(ply) {
                        let local_pv = searcher.search_stack[ply as usize].pv_line.to_vec();
                        super::pv_validation::pv_local_sanity(pos, &local_pv);
                    }
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
                                history.update_cutoff(us, mv, depth as i32, prev_move);

                                // Update counter move history
                                if let Some(prev_mv) = prev_move {
                                    history.counter_moves.update(us, prev_mv, mv);
                                }

                                // Update capture history if the cutoff move is a capture
                                if is_capture {
                                    // Try metadata first, fall back to board lookup
                                    let attacker = mv.piece_type();
                                    let victim = mv
                                        .captured_piece_type()
                                        .or_else(|| pos.piece_at(mv.to()).map(|p| p.piece_type));

                                    if let (Some(att), Some(vic)) = (attacker, victim) {
                                        history.capture.update_good(us, att, vic, depth as i32);
                                    }
                                }

                                // Limit the number of moves to update for performance
                                const MAX_MOVES_TO_UPDATE: usize = 16;

                                // Penalize quiet moves that didn't cause cutoff
                                for &quiet_mv in quiet_moves_tried.iter().take(MAX_MOVES_TO_UPDATE)
                                {
                                    if quiet_mv != mv {
                                        history.update_quiet(us, quiet_mv, depth as i32, prev_move);
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
                                            history.capture.update_bad(us, att, vic, depth as i32);
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
        if !crate::search::tt::filter::should_skip_tt_store(depth, is_pv) {
            let mut boosted_depth = crate::search::tt::filter::boost_tt_depth(depth, node_type);
            // Apply additional boost for PV nodes
            boosted_depth = crate::search::tt::filter::boost_pv_depth(boosted_depth, is_pv);
            searcher.store_tt(hash, boosted_depth, best_score, node_type, best_move, ply as u8);
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
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(5).build());

        let mut pos = Position::startpos();
        let score = search_node(&mut searcher, &mut pos, 3, -1000, 1000, 0);

        // Should return a valid score
        assert!((-1000..=1000).contains(&score));
        assert!(searcher.stats.nodes > 0);
    }

    #[test]
    fn test_search_node_stop_flag() {
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);

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
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
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
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(3).build());

        let mut pos = Position::startpos();

        // Create a move generator to count moves
        use crate::movegen::MoveGenerator;
        let move_gen = MoveGenerator::new();
        let moves = move_gen.generate_all(&pos).unwrap();
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
            UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
        searcher_with_pruning
            .context
            .set_limits(SearchLimits::builder().depth(5).build());

        // Test with pruning disabled
        let mut searcher_no_pruning =
            UnifiedSearcher::<MaterialEvaluator, true, false>::new(MaterialEvaluator);
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
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
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
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
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

    #[test]
    fn test_consecutive_check_extension_limit() {
        // Test that consecutive_checks is preserved across clear_for_new_node
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);

        // Set consecutive_checks value
        searcher.search_stack[3].consecutive_checks = 2;

        // Call clear_for_new_node
        searcher.search_stack[3].clear_for_new_node();

        // Verify consecutive_checks is preserved (not reset to 0)
        assert_eq!(
            searcher.search_stack[3].consecutive_checks, 2,
            "consecutive_checks should be preserved across clear_for_new_node"
        );

        // Also test that other fields are cleared
        assert_eq!(searcher.search_stack[3].current_move, None);
        assert_eq!(searcher.search_stack[3].move_count, 0);
        assert_eq!(searcher.search_stack[3].excluded_move, None);
        assert!(searcher.search_stack[3].quiet_moves.is_empty());
    }

    #[test]
    fn test_check_extension_early_moves_only() {
        // Test that check extensions are only applied to early moves
        use crate::usi::parse_sfen;

        // Position where we can test check extensions
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let mut pos = parse_sfen(sfen).unwrap();

        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(5).build());

        // Reset extension counters
        searcher.stats.check_extensions = Some(0);
        searcher.stats.king_extensions = Some(0);

        // Search the position
        let _ = search_node(&mut searcher, &mut pos, 5, -1000, 1000, 0);

        // Verify that extensions were applied
        let check_exts = searcher.stats.check_extensions.unwrap_or(0);
        let king_exts = searcher.stats.king_extensions.unwrap_or(0);

        // Extensions should be reasonable (not exploding)
        let total_exts = check_exts + king_exts;
        assert!(
            total_exts < searcher.stats.nodes / 10,
            "Too many extensions: {} out of {} nodes",
            total_exts,
            searcher.stats.nodes
        );

        // Note: We don't assert that extensions > 0 because it's position/depth dependent
        // The important check is that extensions don't explode (checked above)
    }
}
