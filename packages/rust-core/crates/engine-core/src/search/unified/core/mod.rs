//! Core search logic for unified searcher
//!
//! Implements alpha-beta search with iterative deepening

mod node;
mod pv;

pub use pv::PVTable;

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        common::mate_score,
        constants::{MAX_PLY, MAX_QUIESCE_DEPTH, SEARCH_INF},
        unified::UnifiedSearcher,
    },
    shogi::{Move, Position},
};

/// Get event polling interval based on time limit
fn get_event_poll_interval<
    E,
    const USE_TT: bool,
    const USE_PRUNING: bool,
    const TT_SIZE_MB: usize,
>(
    searcher: &UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
) -> u64
where
    E: Evaluator + Send + Sync + 'static,
{
    use crate::time_management::TimeControl;

    // Check if we have FixedNodes in either limits or time manager
    if let TimeControl::FixedNodes { .. } = &searcher.context.limits().time_control {
        return 0x3F; // Check every 64 nodes
    }

    // For time-based controls, use adaptive intervals based on soft limit
    if let Some(tm) = &searcher.time_manager {
        match tm.soft_limit_ms() {
            0..=50 => 0x3F,    // ≤50ms → 64 nodes
            51..=500 => 0x1FF, // ≤0.5s → 512 nodes
            _ => 0x3FF,        // default 1024 nodes
        }
    } else {
        // TODO: Performance concern - no TimeManager means 1024 node intervals
        // This can make depth-only searches (e.g., depth 5) very slow as
        // stop conditions are checked infrequently.
        0x3FF // default for no time manager
    }
}

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

        // Also check time manager at root (but ensure at least one move is fully searched)
        if move_idx > 0 {
            if let Some(ref tm) = searcher.time_manager {
                if tm.should_stop(searcher.stats.nodes) {
                    searcher.context.stop();
                    break;
                }
            }
        }

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

    // Get adaptive polling interval based on time control
    let event_interval = get_event_poll_interval(searcher);

    // Process events based on adaptive interval
    if (searcher.stats.nodes & event_interval) == 0 {
        searcher.context.process_events(&searcher.time_manager);

        // For FixedNodes, also check time manager
        if let Some(ref tm) = searcher.time_manager {
            if tm.should_stop(searcher.stats.nodes) {
                searcher.context.stop();
                return 0;
            }
        }
    }

    if searcher.context.should_stop() {
        return 0;
    }

    // Absolute depth limit to prevent stack overflow
    if ply >= MAX_PLY as u16 {
        log::warn!("Hit absolute ply limit {ply} in alpha-beta search");
        let eval = searcher.evaluator.evaluate(pos);
        return eval;
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
    searcher.stats.nodes += 1;

    // Check time limits in quiescence search (especially important for FixedNodes)
    let event_interval = get_event_poll_interval(searcher);
    if (searcher.stats.nodes & event_interval) == 0 {
        if let Some(ref tm) = searcher.time_manager {
            if tm.should_stop(searcher.stats.nodes) {
                searcher.context.stop();
                return alpha; // Return current best value
            }
        }
    }

    // Stand pat
    let stand_pat = searcher.evaluator.evaluate(pos);
    if stand_pat >= beta {
        return beta;
    }
    if alpha < stand_pat {
        alpha = stand_pat;
    }

    // Depth limit check to prevent infinite recursion and stack overflow
    // More conservative limit: either absolute ply limit or quiescence depth limit
    let quiesce_ply = ply.saturating_sub(searcher.context.max_depth() as u16);
    if ply >= MAX_PLY as u16 || quiesce_ply >= MAX_QUIESCE_DEPTH {
        // Log if we hit the absolute limit (potential issue)
        if ply >= MAX_PLY as u16 {
            log::warn!("Hit absolute ply limit {ply} in quiescence search");
        }
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

        // Update zobrist hash for side to move change
        pos.hash ^= pos.side_to_move_zobrist();
        pos.zobrist_hash = pos.hash;

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

        // Restore zobrist hash
        pos.hash ^= pos.side_to_move_zobrist();
        pos.zobrist_hash = pos.hash;

        if score >= beta {
            return Some(beta);
        }
    }

    None
}
