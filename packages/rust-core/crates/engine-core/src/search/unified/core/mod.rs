//! Core search logic for unified searcher
//!
//! Implements alpha-beta search with iterative deepening

mod node;
mod pv;

pub use pv::PVTable;

use crate::{
    evaluation::evaluate::Evaluator,
    search::{common::mate_score, constants::SEARCH_INF, unified::UnifiedSearcher},
    shogi::{Move, Position},
};

/// Search from root position with iterative deepening
pub(super) fn search_root<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
    pos: &mut Position,
    depth: u8,
) -> (i32, Vec<Move>)
where
    E: Evaluator + Send + Sync + 'static,
{
    let mut alpha = -SEARCH_INF;
    let beta = SEARCH_INF;
    let mut best_score = -SEARCH_INF;
    let mut pv = Vec::new();

    // Generate all legal moves
    let mut move_gen_impl = crate::movegen::generator::MoveGenImpl::new(pos);
    let moves = move_gen_impl.generate_all();

    if moves.is_empty() {
        // No legal moves - checkmate or stalemate
        if pos.is_in_check() {
            return (mate_score(0, false), pv); // Getting mated at root
        } else {
            return (0, pv); // Stalemate
        }
    }

    // Order moves
    let ordered_moves = if USE_TT || USE_PRUNING {
        searcher.ordering.order_moves(pos, &moves, None, 0)
    } else {
        moves.as_slice().to_vec()
    };

    // Search each move
    for (move_idx, &mv) in ordered_moves.iter().enumerate() {
        // Make move
        let undo_info = pos.do_move(mv);

        // Increment node counter
        searcher.stats.nodes += 1;

        // Search with null window for moves after the first
        let score = if move_idx == 0 {
            -alpha_beta(searcher, pos, depth - 1, -beta, -alpha, 1)
        } else {
            // Late Move Reduction (if enabled)
            let reduction = if USE_PRUNING && depth >= 3 && move_idx >= 4 && !pos.is_in_check() {
                1
            } else {
                0
            };

            let reduced_depth = (depth - 1).saturating_sub(reduction);
            let score = -alpha_beta(searcher, pos, reduced_depth, -alpha - 1, -alpha, 1);

            // Re-search if score beats alpha
            if score > alpha && score < beta {
                -alpha_beta(searcher, pos, depth - 1, -beta, -alpha, 1)
            } else {
                score
            }
        };

        // Undo move
        pos.undo_move(mv, undo_info);

        // Process events (including ponder hit) every move at root
        searcher.context.process_events(&searcher.time_manager);

        // Check for timeout
        if searcher.context.should_stop() {
            break;
        }

        // Update best move
        if score > best_score {
            best_score = score;
            pv.clear();
            pv.push(mv);

            // Get PV from recursive search
            let child_pv = searcher.pv_table.get_line(1);
            pv.extend_from_slice(child_pv);

            if score > alpha {
                alpha = score;
            }
        }
    }

    (best_score, pv)
}

/// Alpha-beta search with pruning
pub(super) fn alpha_beta<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>(
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
    // Check limits
    searcher.stats.nodes += 1;

    // Process events every 1024 nodes to keep overhead low
    if (searcher.stats.nodes & 0x3FF) == 0 {
        searcher.context.process_events(&searcher.time_manager);
    }

    if searcher.context.should_stop() {
        return 0;
    }

    // Mate distance pruning
    if USE_PRUNING {
        alpha = alpha.max(mate_score(ply as u8, false)); // Getting mated
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
            if tt_entry.depth() >= depth {
                let tt_score = tt_entry.score();
                match tt_entry.node_type() {
                    crate::search::tt::NodeType::Exact => return tt_score as i32,
                    crate::search::tt::NodeType::LowerBound => {
                        if tt_score as i32 >= beta {
                            return tt_score as i32;
                        }
                        alpha = alpha.max(tt_score as i32);
                    }
                    crate::search::tt::NodeType::UpperBound => {
                        if tt_score as i32 <= alpha {
                            return tt_score as i32;
                        }
                    }
                }
            }
        }
    }

    // Quiescence search at leaf nodes
    if depth == 0 {
        return quiescence_search(searcher, pos, alpha, beta, ply);
    }

    // Null move pruning
    if USE_PRUNING && depth >= 3 && !pos.is_in_check() {
        if let Some(score) = try_null_move(searcher, pos, depth, beta, ply) {
            return score;
        }
    }

    // Regular alpha-beta search
    node::search_node(searcher, pos, depth, alpha, beta, ply)
}

/// Quiescence search to handle captures
fn quiescence_search<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
    pos: &mut Position,
    mut alpha: i32,
    beta: i32,
    ply: u16,
) -> i32
where
    E: Evaluator + Send + Sync + 'static,
{
    use crate::search::constants::QUIESCE_MAX_PLY;

    searcher.stats.nodes += 1;

    // Stand pat
    let stand_pat = searcher.evaluator.evaluate(pos);
    if stand_pat >= beta {
        return beta;
    }
    if alpha < stand_pat {
        alpha = stand_pat;
    }

    // Depth limit check to prevent infinite recursion
    if ply >= searcher.context.max_depth() as u16 + QUIESCE_MAX_PLY as u16 {
        return alpha;
    }

    // Generate all moves for quiescence (we'll filter captures manually)
    let mut move_gen_impl = crate::movegen::generator::MoveGenImpl::new(pos);
    let all_moves = move_gen_impl.generate_all();

    // Filter to captures only
    let mut moves = crate::shogi::MoveList::new();
    for &mv in all_moves.iter() {
        if mv.is_capture_hint() {
            moves.push(mv);
        }
    }

    // Search captures
    for &mv in moves.as_slice().iter() {
        // Make move
        let undo_info = pos.do_move(mv);

        // Recursive search
        let score = -quiescence_search(searcher, pos, -beta, -alpha, ply + 1);

        // Undo move
        pos.undo_move(mv, undo_info);

        // Update alpha
        if score >= beta {
            return beta; // Beta cutoff
        }
        if score > alpha {
            alpha = score;
        }
    }

    alpha
}

/// Check if position has non-pawn material
fn has_non_pawn_material(pos: &Position) -> bool {
    use crate::PieceType;

    // Check if current side has any pieces other than pawns and king
    let color = pos.side_to_move;

    // Check pieces on board
    for piece_type in [
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
    ] {
        if !pos.board.pieces_of_type_and_color(piece_type, color).is_empty() {
            return true;
        }
    }

    // Check pieces in hand
    let color_idx = color as usize;
    for piece_idx in 0..7 {
        if piece_idx != PieceType::Pawn as usize && pos.hands[color_idx][piece_idx] > 0 {
            return true;
        }
    }

    false
}

/// Try null move pruning
fn try_null_move<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
    pos: &mut Position,
    depth: u8,
    beta: i32,
    ply: u16,
) -> Option<i32>
where
    E: Evaluator + Send + Sync + 'static,
{
    // Don't do null move if we might be in zugzwang
    // Check if we have non-pawn material (simplified check)
    if has_non_pawn_material(pos) {
        // Make null move by changing side to move
        pos.side_to_move = pos.side_to_move.opposite();
        pos.ply += 1;

        // Search with reduced depth
        let reduction = 2 + depth / 4;
        let score = -alpha_beta(
            searcher,
            pos,
            depth.saturating_sub(reduction + 1),
            -beta,
            -beta + 1,
            ply + 1,
        );

        // Undo null move
        pos.side_to_move = pos.side_to_move.opposite();
        pos.ply -= 1;

        if score >= beta {
            return Some(beta);
        }
    }

    None
}
