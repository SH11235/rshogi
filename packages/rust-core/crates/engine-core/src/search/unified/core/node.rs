//! Node expansion and search logic

use crate::{
    evaluation::evaluate::Evaluator,
    search::{constants::SEARCH_INF, tt::NodeType, unified::UnifiedSearcher},
    shogi::{Move, Position},
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
    // Check stop flag periodically (every 1024 nodes) to minimize overhead
    if searcher.stats.nodes & 0x3FF == 0 && searcher.context.should_stop() {
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
        searcher.probe_tt(hash).and_then(|entry| entry.get_move())
    } else {
        None
    };

    // Order moves
    let ordered_moves = if USE_TT || USE_PRUNING {
        searcher.ordering.order_moves(pos, &moves, tt_move, &searcher.search_stack, ply)
    } else {
        moves.as_slice().to_vec()
    };

    // Search moves
    for &mv in ordered_moves.iter() {
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

        // Make move
        let undo_info = pos.do_move(mv);
        searcher.stats.nodes += 1;

        let mut score;

        // Principal variation search
        if moves_searched == 0 {
            // Full window search for first move
            score = -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha, ply + 1);
        } else {
            // Late move reduction
            let reduction = if USE_PRUNING && should_reduce(depth, moves_searched, in_check, mv) {
                calculate_reduction(depth, moves_searched)
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

                        // Get previous move for history updates
                        let prev_move = if ply > 0
                            && crate::search::types::SearchStack::is_valid_ply(ply - 1)
                        {
                            searcher.search_stack[(ply - 1) as usize].current_move
                        } else {
                            None
                        };

                        if let Ok(mut history) = searcher.history.lock() {
                            // Update history for the cutoff move
                            history.update_cutoff(pos.side_to_move, mv, depth as i32, prev_move);

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

                            // Penalize quiet moves that didn't cause cutoff
                            for &quiet_mv in quiet_moves_tried.iter() {
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
                            for &capture_mv in captures_tried.iter() {
                                if capture_mv != mv {
                                    if let (Some(attacker), Some(victim)) =
                                        (capture_mv.piece_type(), capture_mv.captured_piece_type())
                                    {
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
                    }
                    break;
                }
            }
        }

        // Futility pruning
        if USE_PRUNING
            && can_prune(searcher, pos, depth, alpha, beta, moves_searched, in_check, ply)
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

        searcher.store_tt(hash, depth, best_score, node_type, best_move);
    }

    best_score
}

/// Check if we should apply late move reduction
fn should_reduce(depth: u8, moves_searched: u32, in_check: bool, mv: Move) -> bool {
    depth >= 3 && moves_searched >= 4 && !in_check && !mv.is_capture_hint() && !mv.is_promote()
}

/// Calculate reduction amount
fn calculate_reduction(depth: u8, moves_searched: u32) -> u8 {
    if moves_searched >= 6 && depth >= 4 {
        2
    } else {
        1
    }
}

/// Check if we can prune remaining moves
#[allow(clippy::too_many_arguments)]
fn can_prune<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
    pos: &Position,
    depth: u8,
    alpha: i32,
    beta: i32,
    moves_searched: u32,
    in_check: bool,
    ply: u16,
) -> bool
where
    E: Evaluator + Send + Sync + 'static,
{
    // Don't prune if:
    // - In check
    // - Too few moves searched
    // - At low depth
    // - Near mate scores
    if in_check || moves_searched < 8 || depth < 2 {
        return false;
    }

    if crate::search::common::is_mate_score(alpha) || crate::search::common::is_mate_score(beta) {
        return false;
    }

    // Futility margin
    let margin = 200 * depth as i32;

    // Get static eval - use cached value if available
    let eval = if crate::search::types::SearchStack::is_valid_ply(ply) {
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

    eval + margin < alpha
}
