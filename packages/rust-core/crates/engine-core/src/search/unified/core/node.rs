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
    let original_alpha = alpha;
    let hash = pos.zobrist_hash;
    let mut best_move = None;
    let mut best_score = -SEARCH_INF;
    let mut moves_searched = 0;

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
            // Calculate reduced depth, ensuring it doesn't go below 0
            let reduced_depth = depth.saturating_sub(1).saturating_sub(reduction);
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
                        if let Ok(mut history) = searcher.history.lock() {
                            history.update_cutoff(pos.side_to_move, mv, depth as i32, None);
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
