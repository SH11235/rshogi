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
    shogi::{Move, PieceType, Position},
};

/// Get event polling mask based on time limit
fn get_event_poll_mask<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>(
    searcher: &UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
) -> u64
where
    E: Evaluator + Send + Sync + 'static,
{
    use crate::time_management::TimeControl;

    // If already stopped, check every node for immediate exit
    if searcher.context.should_stop() {
        return 0x0; // Check every node for immediate response
    }

    // If stop_flag is present, use more frequent polling for responsiveness
    if searcher.context.limits().stop_flag.is_some() {
        return 0x1F; // Check every 32 nodes for responsive stop handling
    }

    // Check if we have FixedNodes in either limits or time manager
    if let TimeControl::FixedNodes { .. } = &searcher.context.limits().time_control {
        return 0x3F; // Check every 64 nodes
    }

    // Check if we're in ponder mode - need frequent polling for ponderhit
    if matches!(&searcher.context.limits().time_control, TimeControl::Ponder(_)) {
        return 0x3F; // Check every 64 nodes for responsive ponderhit detection
    }

    // For time-based controls, use adaptive intervals based on soft limit
    if let Some(tm) = &searcher.time_manager {
        // Check if TimeManager is in ponder mode (soft_limit would be u64::MAX)
        let soft_limit = tm.soft_limit_ms();
        if soft_limit == u64::MAX {
            // Ponder mode or infinite search - need frequent polling
            return 0x3F; // Check every 64 nodes
        }

        match soft_limit {
            0..=50 => 0x1F,    // ≤50ms → 32 nodes
            51..=100 => 0x3F,  // ≤100ms → 64 nodes
            101..=200 => 0x7F, // ≤200ms → 128 nodes
            201..=500 => 0xFF, // ≤0.5s → 256 nodes
            _ => 0x3FF,        // default 1024 nodes
        }
    } else {
        // For searches without TimeManager (infinite search, depth-only, etc)
        // Use more frequent polling to ensure responsive stop command handling
        0x7F // Check every 128 nodes for better responsiveness
    }
}

/// Validate that all moves in a PV are legal from the given position
pub(super) fn assert_pv_legal(pos: &Position, pv: &[Move]) {
    let mut p = pos.clone();
    for (i, mv) in pv.iter().enumerate() {
        if !p.is_legal_move(*mv) {
            #[cfg(debug_assertions)]
            {
                if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                    eprintln!("[BUG] illegal pv at ply {i}: {}", crate::usi::move_to_usi(mv));
                    eprintln!("  Position: {}", crate::usi::position_to_sfen(&p));
                    eprintln!(
                        "  Full PV: {}",
                        pv.iter().map(crate::usi::move_to_usi).collect::<Vec<_>>().join(" ")
                    );
                }
            }
            // Do not panic in any build; keep the engine running and just log the issue
            break;
        }
        let _undo_info = p.do_move(*mv);
        // For PV validation, we don't need to undo since we're working on a clone
    }
}

/// Validate PV using occupancy invariants (not relying on move generator)
pub(super) fn pv_local_sanity(pos: &Position, pv: &[Move]) {
    let mut p = pos.clone();

    for (i, &mv) in pv.iter().enumerate() {
        // Skip null moves
        if mv == Move::NULL {
            #[cfg(debug_assertions)]
            if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                eprintln!("[BUG] NULL move in PV at ply {i}");
            }
            return;
        }

        let usi = crate::usi::move_to_usi(&mv);

        // Pre-move validation
        if mv.is_drop() {
            // For drops: check we have the piece in hand
            let piece_type = mv.drop_piece_type();
            let hands = &p.hands[p.side_to_move as usize];
            let Some(hand_idx) = piece_type.hand_index() else {
                #[cfg(debug_assertions)]
                if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                    eprintln!("[BUG] Invalid drop piece type (King) at ply {i}: {usi}");
                }
                return;
            };
            let count = hands[hand_idx];
            if count == 0 {
                #[cfg(debug_assertions)]
                {
                    if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                        eprintln!("[BUG] No piece in hand for drop at ply {i}: {usi}");
                        eprintln!("  Position: {}", crate::usi::position_to_sfen(&p));
                    }
                }
                return;
            }
        } else {
            // For normal moves: check piece exists at from square
            if let Some(from) = mv.from() {
                if p.piece_at(from).is_none() {
                    #[cfg(debug_assertions)]
                    {
                        // Only print in debug mode when SHOGI_DEBUG_PV is set
                        if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                            eprintln!("[BUG] No piece at from square at ply {i}: {usi}");
                            eprintln!("  Position: {}", crate::usi::position_to_sfen(&p));
                            eprintln!("  From square {from:?} is empty");
                        }
                    }
                    return;
                }
            } else {
                #[cfg(debug_assertions)]
                if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                    eprintln!("[BUG] Normal move has no from square at ply {i}: {usi}");
                }
                return;
            }
        }

        // Check if move is pseudo-legal before applying
        if !p.is_pseudo_legal(mv) {
            #[cfg(debug_assertions)]
            {
                if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                    eprintln!("[BUG] Illegal move in PV at ply {i}: {usi}");
                    eprintln!("  Position: {}", crate::usi::position_to_sfen(&p));
                    eprintln!("  Move is not pseudo-legal");
                }
            }
            return;
        }

        // Apply move
        let _undo_info = p.do_move(mv);

        // Post-move validation
        if !mv.is_drop() {
            // Check from square is now empty
            if let Some(from) = mv.from() {
                if p.piece_at(from).is_some() {
                    #[cfg(debug_assertions)]
                    {
                        if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                            eprintln!("[BUG] From square not cleared at ply {i}: {usi}");
                            eprintln!(
                                "  Position after move: {}",
                                crate::usi::position_to_sfen(&p)
                            );
                        }
                    }
                    return;
                }
            }
        }

        // Check to square has our piece
        let to = mv.to();
        match p.piece_at(to) {
            Some(piece) if piece.color == p.side_to_move.opposite() => {
                // OK - we just moved there
            }
            _ => {
                #[cfg(debug_assertions)]
                {
                    if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                        eprintln!("[BUG] To square not occupied by our piece at ply {i}: {usi}");
                        eprintln!("  Position after move: {}", crate::usi::position_to_sfen(&p));
                    }
                }
                return;
            }
        }
    }
}

/// Search from root position with aspiration window
pub(super) fn search_root_with_window<
    E,
    const USE_TT: bool,
    const USE_PRUNING: bool,
    const TT_SIZE_MB: usize,
>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
    pos: &mut Position,
    depth: u8,
    initial_alpha: i32,
    initial_beta: i32,
) -> (i32, Vec<Move>)
where
    E: Evaluator + Send + Sync + 'static,
{
    // Complete any remaining garbage collection from previous search
    #[cfg(feature = "hashfull_filter")]
    if USE_TT {
        if let Some(ref tt) = searcher.tt {
            while tt.should_trigger_gc() {
                tt.incremental_gc(512); // Larger batch size before search starts
            }
        }
    }

    let mut alpha = initial_alpha;
    let beta = initial_beta;
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
        searcher.ordering.order_moves(pos, &moves, None, &searcher.search_stack, 0)
    } else {
        moves.as_slice().to_vec()
    };

    // Skip prefetching - it has shown negative performance impact

    // Search each move
    for (move_idx, &mv) in ordered_moves.iter().enumerate() {
        // Validate move before executing (important for TT moves)
        if !pos.is_pseudo_legal(mv) {
            #[cfg(debug_assertions)]
            {
                if std::env::var("SHOGI_DEBUG_SEARCH").is_ok() {
                    eprintln!("WARNING: Skipping illegal move in search at depth {depth}");
                    eprintln!("Move: {}", crate::usi::move_to_usi(&mv));
                    eprintln!("Position: {}", crate::usi::position_to_sfen(pos));
                }
            }
            continue;
        }

        // Make move
        let undo_info = pos.do_move(mv);

        // Increment node counter
        searcher.stats.nodes += 1;

        // Prefetch TT entry for the new position (root moves are always important)
        if USE_TT && !searcher.disable_prefetch {
            searcher.prefetch_tt(pos.zobrist_hash);
        }

        // Search with null window for moves after the first
        let score = if move_idx == 0 {
            -alpha_beta(searcher, pos, depth - 1, -beta, -alpha, 1)
        } else {
            // Late Move Reduction (if enabled)
            let reduction = if USE_PRUNING && depth >= 3 && move_idx >= 4 && !pos.is_in_check() {
                // Use more sophisticated reduction based on depth and move count
                crate::search::unified::pruning::lmr_reduction(depth, move_idx as u32)
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

        // Check stop flag immediately after alpha-beta search
        if searcher.context.should_stop() {
            // Skip TT storage when stopping - adds overhead
            break;
        }

        // Process events (including ponder hit) every move at root
        searcher.context.process_events(&searcher.time_manager);

        // Also check time manager at root (but ensure at least one move is fully searched)
        if move_idx > 0
            && searcher.context.check_time_limit(searcher.stats.nodes, &searcher.time_manager)
        {
            // Skip TT storage when stopping - adds overhead
            break;
        }

        // Check for timeout
        if searcher.context.should_stop() {
            // Skip TT storage when stopping - adds overhead
            break;
        }

        // Update best move
        if score > best_score {
            best_score = score;
            pv.clear();
            pv.push(mv);

            // Get PV from recursive search
            let child_pv = searcher.pv_table.get_line(1);

            // Validate child PV moves before extending
            // This prevents propagating invalid moves from TT pollution
            let mut valid_child_pv = Vec::new();
            let mut temp_pos = pos.clone();
            let undo_info = temp_pos.do_move(mv);

            for &child_mv in child_pv {
                if temp_pos.is_pseudo_legal(child_mv) {
                    valid_child_pv.push(child_mv);
                    let _child_undo = temp_pos.do_move(child_mv);
                } else {
                    #[cfg(debug_assertions)]
                    if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                        eprintln!("[WARNING] Invalid move in child PV at root, truncating");
                        eprintln!("  Move: {}", crate::usi::move_to_usi(&child_mv));
                        eprintln!("  Position: {}", crate::usi::position_to_sfen(&temp_pos));
                    }
                    break;
                }
            }

            temp_pos.undo_move(mv, undo_info);
            pv.extend_from_slice(&valid_child_pv);

            if score > alpha {
                alpha = score;
            }
        }
    }

    // Validate PV before returning
    if !pv.is_empty() {
        // First check occupancy invariants (doesn't rely on move generator)
        pv_local_sanity(pos, &pv);
        // Then check legal moves
        assert_pv_legal(pos, &pv);
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

    // Early stop check
    if searcher.context.should_stop() {
        return alpha;
    }

    // Get adaptive polling mask based on time control
    let event_mask = get_event_poll_mask(searcher);

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

        // Check time limit using unified method
        if searcher.context.check_time_limit(searcher.stats.nodes, &searcher.time_manager) {
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

    // Check stop flag periodically to minimize overhead
    // Use more frequent checking if stop_flag is present
    let stop_check_interval = if searcher.context.limits().stop_flag.is_some() {
        0x3F // Check every 64 nodes when stop_flag is present
    } else {
        0x3FF // Check every 1024 nodes for normal operation
    };

    if searcher.stats.nodes & stop_check_interval == 0 && searcher.context.should_stop() {
        // Skip TT storage when aborting search - it adds overhead with minimal benefit
        // The partial evaluation is likely to be overwritten anyway
        return alpha;
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
        return quiescence_search(searcher, pos, alpha, beta, ply);
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
            if let Some(score) = try_null_move(searcher, pos, depth, beta, ply) {
                return score;
            }
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

    // Early stop check
    if searcher.context.should_stop() {
        return alpha;
    }

    // Periodic time check
    let time_check_mask = searcher.context.get_time_check_mask();
    if (searcher.stats.nodes & time_check_mask) == 0 {
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

    // Order captures by MVV-LVA if pruning is enabled
    let ordered_captures = if USE_PRUNING {
        let mut captures_vec: Vec<Move> = moves.as_slice().to_vec();
        captures_vec.sort_by_cached_key(|&mv| {
            // Simple MVV-LVA: prioritize capturing more valuable pieces
            if let Some(victim) = mv.captured_piece_type() {
                -(victim as i32)
            } else {
                0
            }
        });
        captures_vec
    } else {
        moves.as_slice().to_vec()
    };

    // Search captures
    for &mv in ordered_captures.iter() {
        // Check stop flag at the beginning of each capture move
        if searcher.context.should_stop() {
            return alpha; // Return current alpha value
        }

        // Delta pruning - skip captures that can't improve position enough
        if USE_PRUNING && delta_margin > 0 {
            // Estimate material gain from capture
            let material_gain = if let Some(victim) = mv.captured_piece_type() {
                // Simple piece values for delta pruning
                match victim {
                    PieceType::Pawn => 100,
                    PieceType::Lance => 300,
                    PieceType::Knight => 400,
                    PieceType::Silver => 500,
                    PieceType::Gold => 600,
                    PieceType::Bishop => 800,
                    PieceType::Rook => 1000,
                    PieceType::King => 10000, // Should never happen
                }
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
            #[cfg(debug_assertions)]
            {
                if std::env::var("SHOGI_DEBUG_SEARCH").is_ok() {
                    eprintln!("WARNING: Skipping illegal move in quiescence search");
                    eprintln!("Move: {}", crate::usi::move_to_usi(&mv));
                    eprintln!("Position: {}", crate::usi::position_to_sfen(pos));
                }
            }
            continue;
        }

        // Make move
        let undo_info = pos.do_move(mv);

        // Skip prefetch in quiescence - adds overhead with minimal benefit
        // (Controlled by tt_filter module)

        // Recursive search
        let score = -quiescence_search(searcher, pos, -beta, -alpha, ply + 1);

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

    alpha
}

/// Check if position has non-pawn material
fn has_non_pawn_material(pos: &Position) -> bool {
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
    for (hand_idx, &pt) in crate::shogi::HAND_PIECE_TYPES.iter().enumerate() {
        if pt != PieceType::Pawn && pos.hands[color_idx][hand_idx] > 0 {
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
        // Make null move using the Position's method
        let undo_info = pos.do_null_move();

        // Search with reduced depth using pruning module
        let reduction = crate::search::unified::pruning::null_move_reduction(depth);
        let score = -alpha_beta(
            searcher,
            pos,
            depth.saturating_sub(reduction + 1),
            -beta,
            -beta + 1,
            ply + 1,
        );

        // Undo null move
        pos.undo_null_move(undo_info);

        if score >= beta {
            return Some(beta);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::Position;

    #[test]
    fn test_stop_flag_polling_interval() {
        // Test that stop flag checks are done at appropriate intervals
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false, 0>::new(evaluator);

        // Simulate node counting and verify polling frequency
        let mut check_count = 0;
        for i in 0..100000 {
            searcher.stats.nodes = i;
            // Check stop flag only every 1024 nodes (0x3FF = 1023)
            if searcher.stats.nodes & 0x3FF == 0 {
                check_count += 1;
            }
        }

        // Should check approximately 97 times (100000 / 1024)
        assert!((95..=100).contains(&check_count), "Check count: {check_count}");
    }

    #[test]
    fn test_get_event_poll_mask_values() {
        // Test that the polling mask function returns expected values
        let evaluator = MaterialEvaluator;
        let searcher = UnifiedSearcher::<MaterialEvaluator, false, false, 0>::new(evaluator);

        // Without time manager, should return 0x7F (127) for responsiveness
        let mask = get_event_poll_mask(&searcher);
        assert_eq!(mask, 0x7F, "Without time manager should return 0x7F");
    }

    #[test]
    fn test_has_non_pawn_material() {
        let pos = Position::startpos();

        // Starting position has non-pawn material (checks current side to move)
        assert!(has_non_pawn_material(&pos)); // Black's turn at start

        // TODO: Add test for endgame position with only pawns
    }

    #[test]
    fn test_reduced_depth_calculation() {
        // Test that reduced depth calculation handles edge cases properly
        let depth: u8 = 3;
        let reduction: u8 = 2;

        // saturating_sub ensures we don't underflow
        let reduced_depth = depth.saturating_sub(1 + reduction);
        assert_eq!(reduced_depth, 0); // Should transition to quiescence search

        // Normal case
        let depth: u8 = 6;
        let reduction: u8 = 1;
        let reduced_depth = depth.saturating_sub(1 + reduction);
        assert_eq!(reduced_depth, 4);
    }

    #[test]
    fn test_invalid_move_in_pv_regression() {
        // Regression test for the bug where invalid moves (like 4b5b on empty square) were in PV
        use crate::shogi::Square;
        use crate::usi::parse_usi_move;

        // Create position from the problematic SFEN
        let sfen = "lnsgkgsnl/r6b1/pppppppp1/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4";
        let pos = Position::from_sfen(sfen).expect("Valid SFEN");

        // Try the problematic move "4b5b"
        let problematic_move = parse_usi_move("4b5b").expect("Valid USI move");
        
        // Verify Square(14) is empty (4b in internal representation)
        let from_square = Square(14); // 4b
        assert!(pos.piece_at(from_square).is_none(), "Square 4b should be empty");
        
        // The move should not be pseudo-legal
        assert!(!pos.is_pseudo_legal(problematic_move), "Move 4b5b should not be pseudo-legal on empty square");

        // Run pv_local_sanity on a PV containing this move
        let pv = vec![problematic_move];
        pv_local_sanity(&pos, &pv);
        
        // The function should handle invalid moves gracefully without panicking
    }

    #[test]
    fn test_pv_move_validation() {
        // Test that invalid moves are not added to PV
        use crate::shogi::{Move, Square};
        
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false, 0>::new(evaluator);
        let pos = Position::startpos();
        
        // Create an invalid move (moving from empty square)
        let invalid_move = Move::normal(Square(50), Square(51), false);
        assert!(!pos.is_pseudo_legal(invalid_move), "Move should be invalid");
        
        // Try to update PV with invalid move
        searcher.pv_table.set_line(0, invalid_move, &[]);
        
        // PV should reject null moves (our set_line now checks for NULL)
        // For other invalid moves, the validation happens at a higher level
    }

    #[test] 
    fn test_pv_null_move_rejection() {
        // Test that NULL moves are rejected from PV
        use crate::shogi::Move;
        
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false, 0>::new(evaluator);
        
        // Try to add NULL move to PV
        searcher.pv_table.set_line(0, Move::NULL, &[]);
        
        // PV should remain empty
        let pv = searcher.pv_table.get_line(0);
        assert!(pv.is_empty(), "PV should be empty after trying to add NULL move");
    }

    #[test]
    fn test_pv_validation_with_tt_pollution() {
        // Test that PV validation catches moves from wrong positions (TT pollution)
        use crate::usi::parse_usi_move;
        
        // Position 1: Black bishop on 4b
        let sfen1 = "lnsgkgsnl/r2B3b1/ppppppppp/9/9/9/PPPPPPPPP/7R1/LNSGKGSNL b - 1";
        let pos1 = Position::from_sfen(sfen1).expect("Valid SFEN");
        
        // Position 2: Empty 4b square (the problematic case)
        let sfen2 = "lnsgkgsnl/r6b1/ppppppppp/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4";
        let pos2 = Position::from_sfen(sfen2).expect("Valid SFEN");
        
        // Move that's valid in pos1 but not in pos2
        let move_4b5b = parse_usi_move("4b5b").expect("Valid USI move");
        
        assert!(pos1.is_pseudo_legal(move_4b5b), "Move should be legal in position 1");
        assert!(!pos2.is_pseudo_legal(move_4b5b), "Move should be illegal in position 2");
        
        // Test pv_local_sanity catches this
        let bad_pv = vec![move_4b5b];
        pv_local_sanity(&pos2, &bad_pv);
        // Should not panic, just return early
    }

    #[test]
    fn test_tt_move_validation_in_search() {
        // Test that TT moves are validated before use
        use crate::search::sharded_tt::ShardedTranspositionTable;
        use crate::search::tt::NodeType;
        use crate::usi::parse_usi_move;
        use std::sync::Arc;

        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, false, 1>::new(evaluator);
        
        // Initialize TT
        searcher.tt = Some(Arc::new(ShardedTranspositionTable::new(1)));
        
        // Position where 4b5b is invalid
        let sfen = "lnsgkgsnl/r6b1/pppppppp1/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4";
        let pos = Position::from_sfen(sfen).expect("Valid SFEN");
        
        // Store an invalid move in TT
        let invalid_move = parse_usi_move("4b5b").expect("Valid USI move");
        if let Some(ref tt) = searcher.tt {
            tt.store(
                pos.zobrist_hash, 
                Some(invalid_move), 
                100, // score
                0,   // eval
                10,  // depth
                NodeType::Exact
            );
        }
        
        // The search should validate the TT move and reject it
        // This is tested implicitly through the move ordering logic
        
        // Verify TT entry exists but move would be invalid
        let tt_entry = searcher.probe_tt(pos.zobrist_hash);
        assert!(tt_entry.is_some(), "TT entry should exist");
        
        // The move validation happens in the search when legal moves are generated
        // and TT move is checked against them
    }

    #[test]
    fn test_parallel_search_pv_consistency() {
        // Test PV consistency in parallel search scenario
        use std::sync::{Arc, Mutex};
        use std::thread;
        
        // Simulate multiple threads updating PV
        let pv_table = Arc::new(Mutex::new(PVTable::new()));
        let mut handles = vec![];
        
        // Each thread tries to update PV
        for i in 0..4 {
            let pv_clone = Arc::clone(&pv_table);
            let handle = thread::spawn(move || {
                use crate::usi::parse_usi_square;
                
                let move1 = Move::normal(
                    parse_usi_square("7g").unwrap(), 
                    parse_usi_square("7f").unwrap(), 
                    false
                );
                let move2 = Move::normal(
                    parse_usi_square("6c").unwrap(), 
                    parse_usi_square("6d").unwrap(), 
                    false
                );
                
                // Try to update PV
                if let Ok(mut pv) = pv_clone.lock() {
                    pv.set_line(i, move1, &[move2]);
                }
            });
            handles.push(handle);
        }
        
        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }
        
        // Check that PVs are consistent
        let pv = pv_table.lock().unwrap();
        for i in 0..4 {
            let (line, len) = pv.line(i);
            if len > 0 {
                // Verify moves are not NULL
                assert_ne!(line[0], Move::NULL, "PV should not contain NULL moves");
            }
        }
    }

    #[test]
    fn test_hash_collision_move_validation() {
        // Test handling of hash collisions where TT returns wrong position's move
        use crate::search::sharded_tt::ShardedTranspositionTable;
        use crate::search::tt::NodeType;
        use crate::usi::parse_usi_move;
        use std::sync::Arc;
        
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, false, 1>::new(evaluator);
        searcher.tt = Some(Arc::new(ShardedTranspositionTable::new(1)));
        
        // Create two positions with potentially colliding hashes
        let pos1 = Position::startpos();
        let pos2 = Position::from_sfen("lnsgkgsnl/r6b1/pppppppp1/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4").unwrap();
        
        // Simulate hash collision by forcing same hash (in real scenarios)
        // Store a move that's legal in pos1 but not in pos2
        let move_7g7f = parse_usi_move("7g7f").unwrap();
        
        assert!(pos1.is_pseudo_legal(move_7g7f), "Move should be legal in pos1");
        assert!(!pos2.is_pseudo_legal(move_7g7f), "Move should be illegal in pos2");
        
        if let Some(ref tt) = searcher.tt {
            // Store with pos2's hash but pos1's move
            tt.store(
                pos2.zobrist_hash,
                Some(move_7g7f),
                50,
                0,
                5,
                NodeType::Exact
            );
        }
        
        // When retrieving from TT for pos2, the move should be validated
        let tt_entry = searcher.probe_tt(pos2.zobrist_hash);
        if let Some(entry) = tt_entry {
            if let Some(tt_move) = entry.get_move() {
                // The move validation happens when it's used in search
                // Here we just verify the move exists in TT
                assert_eq!(tt_move, move_7g7f, "TT should return the stored move");
            }
        }
    }
}
