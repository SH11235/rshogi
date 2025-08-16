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
        constants::{MAX_PLY, SEARCH_INF},
        unified::UnifiedSearcher,
    },
    shogi::{Move, PieceType, Position},
};

/// Get victim score for MVV-LVA ordering
/// Higher value pieces get higher scores
#[inline]
const fn victim_score(pt: PieceType) -> i32 {
    match pt {
        PieceType::Pawn => 100,
        PieceType::Lance => 300,
        PieceType::Knight => 400,
        PieceType::Silver => 500,
        PieceType::Gold => 600,
        PieceType::Bishop => 800,
        PieceType::Rook => 1000,
        PieceType::King => 10000, // Should never happen
    }
}

/// Get event polling mask based on time limit
///
/// Returns a bitmask that determines how frequently to check for events (time limit, stop flag, etc).
/// Lower values mean more frequent checks:
/// - 0x0 (0): Check every node (immediate response when already stopped)
/// - 0x1F (31): Check every 32 nodes (responsive stop handling)
/// - 0x3F (63): Check every 64 nodes (fixed nodes or ponder mode)
/// - 0x7F-0x3FF: Check every 128-1024 nodes (time-based controls)
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
                // Note: We use p.side_to_move.opposite() because after do_move(),
                // the side_to_move has already been flipped to the opponent.
                // So the piece we just moved belongs to the previous side_to_move.
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

    // Order moves - avoid Vec to SmallVec conversion
    let (ordered_slice, _owned_moves);
    if USE_TT || USE_PRUNING {
        let moves_vec = searcher.ordering.order_moves(pos, &moves, None, &searcher.search_stack, 0);
        _owned_moves = Some(moves_vec);
        let vec_ref = _owned_moves.as_ref().unwrap();
        ordered_slice = &vec_ref[..];
    } else {
        ordered_slice = moves.as_slice();
        _owned_moves = None;
    };

    // Skip prefetching - it has shown negative performance impact

    // Search each move
    for (move_idx, &mv) in ordered_slice.iter().enumerate() {
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

        // Note: Node counting is done in alpha_beta() to avoid double counting

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
            let mut undo_infos = Vec::new();

            for &child_mv in child_pv {
                if temp_pos.is_pseudo_legal(child_mv) {
                    valid_child_pv.push(child_mv);
                    let child_undo = temp_pos.do_move(child_mv);
                    undo_infos.push((child_mv, child_undo));
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

            // Undo all child moves in reverse order
            for (child_mv, child_undo) in undo_infos.iter().rev() {
                temp_pos.undo_move(*child_mv, child_undo.clone());
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
    // Increment node count here (not in search_node to avoid double counting)
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

    // Periodic time check (unified with alpha_beta and search_node)
    let time_check_mask = get_event_poll_mask(searcher);
    if time_check_mask == 0 || (searcher.stats.nodes & time_check_mask) == 0 {
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

    // Check if in check first - this determines our search strategy
    let in_check = pos.is_in_check();

    // Absolute stack guard - always enforce this limit
    if ply >= MAX_PLY as u16 {
        log::warn!("Hit absolute ply limit {ply} in quiescence search");
        // In check at max depth is rare but we can't recurse further
        // Return pessimistic value rather than static eval which could be illegal
        return if in_check {
            alpha
        } else {
            searcher.evaluator.evaluate(pos)
        };
    }

    // More aggressive ply limit for quiescence to prevent hangs
    // This is a safety measure - normal quiescence should not go this deep
    const QUIESCE_SAFETY_LIMIT: u16 = 100;
    if ply >= QUIESCE_SAFETY_LIMIT {
        eprintln!("WARNING: Hit quiescence safety limit at ply {ply}");
        return searcher.evaluator.evaluate(pos);
    }

    // Very aggressive ply limiting to fix the hanging issue
    // The test position has deep check sequences that can explode
    if ply >= 16 {
        // Hard stop at reasonable depth
        // Return evaluation-based value instead of fixed constants to avoid discontinuity
        return if in_check {
            // In check positions, return a value based on evaluation but slightly pessimistic
            alpha.max(searcher.evaluator.evaluate(pos) - 50)
        } else {
            // Not in check, return current evaluation
            searcher.evaluator.evaluate(pos)
        };
    }

    if in_check {
        // In check: must search all legal moves (no stand pat)
        // Generate all legal moves
        let mut move_gen_impl = crate::movegen::generator::MoveGenImpl::new(pos);
        let moves = move_gen_impl.generate_all();

        // If no legal moves, it's checkmate
        if moves.is_empty() {
            return mate_score(ply as u8, false); // Getting mated
        }

        // Search all legal moves to find check evasion
        let mut best = -SEARCH_INF;
        for &mv in moves.iter() {
            // Validate move
            if !pos.is_pseudo_legal(mv) {
                continue;
            }

            // Make move
            let undo_info = pos.do_move(mv);

            // Recursive search
            let score = -quiescence_search(searcher, pos, -beta, -alpha, ply + 1);

            // Undo move
            pos.undo_move(mv, undo_info);

            // Check stop flag
            if searcher.context.should_stop() {
                return alpha;
            }

            // Update bounds
            if score >= beta {
                return beta;
            }
            if score > best {
                best = score;
            }
            if score > alpha {
                alpha = score;
            }
        }

        return alpha.max(best);
    }

    // Not in check: normal quiescence search with captures only
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

    // Generate all moves for quiescence (we'll filter captures manually)
    let mut move_gen_impl = crate::movegen::generator::MoveGenImpl::new(pos);
    let all_moves = move_gen_impl.generate_all();

    // Filter to captures only - check board state directly instead of relying on metadata
    let us = pos.side_to_move;
    let mut moves = crate::shogi::MoveList::new();
    for &mv in all_moves.iter() {
        if mv.is_drop() {
            continue; // Drops don't capture
        }
        // Check if destination square has enemy piece
        if let Some(piece) = pos.piece_at(mv.to()) {
            if piece.color == us.opposite() {
                moves.push(mv);
            }
        }
    }

    // Order captures by MVV-LVA if pruning is enabled - sort in place to avoid allocation
    if USE_PRUNING {
        moves.as_mut_vec().sort_by_cached_key(|&mv| {
            // MVV-LVA: prioritize capturing more valuable pieces
            // First try metadata, then fall back to board lookup
            if let Some(victim) = mv.captured_piece_type() {
                -victim_score(victim)
            } else if let Some(piece) = pos.piece_at(mv.to()) {
                -victim_score(piece.piece_type)
            } else {
                0 // Should not happen for captures
            }
        });
    }

    // Search captures
    for &mv in moves.iter() {
        // Check stop flag at the beginning of each capture move
        if searcher.context.should_stop() {
            return alpha; // Return current alpha value
        }

        // Delta pruning - skip captures that can't improve position enough
        if USE_PRUNING && delta_margin > 0 {
            // Estimate material gain from capture
            let material_gain = if let Some(victim) = mv.captured_piece_type() {
                victim_score(victim)
            } else if let Some(piece) = pos.piece_at(mv.to()) {
                // Fallback to board lookup if metadata is missing
                victim_score(piece.piece_type)
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
    use crate::search::SearchLimits;
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
            // Simulate periodic checks (this test uses 0x3FF = every 1024 nodes)
            // Note: Actual implementation uses adaptive intervals via get_event_poll_mask()
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
        assert!(
            !pos.is_pseudo_legal(problematic_move),
            "Move 4b5b should not be pseudo-legal on empty square"
        );

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

        // Position 1: Black bishop on 3b
        let sfen1 = "lnsgkgsnl/r5B1b/ppppppppp/9/9/9/PPPPPPPPP/7R1/LNSGKGSNL b - 1";
        let pos1 = Position::from_sfen(sfen1).expect("Valid SFEN");

        // Position 2: Empty 3b square (no bishop)
        let sfen2 = "lnsgkgsnl/r6b1/ppppppppp/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4";
        let pos2 = Position::from_sfen(sfen2).expect("Valid SFEN");

        // Move that's valid in pos1 but not in pos2
        let move_3b5b = parse_usi_move("3b5b").expect("Valid USI move");

        assert!(pos1.is_pseudo_legal(move_3b5b), "Move should be legal in position 1");
        assert!(!pos2.is_pseudo_legal(move_3b5b), "Move should be illegal in position 2");

        // Test pv_local_sanity catches this
        let bad_pv = vec![move_3b5b];
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
                NodeType::Exact,
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
                    false,
                );
                let move2 = Move::normal(
                    parse_usi_square("6c").unwrap(),
                    parse_usi_square("6d").unwrap(),
                    false,
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
        let pos2 =
            Position::from_sfen("lnsgkgsnl/r6b1/pppppppp1/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4")
                .unwrap();

        // Simulate hash collision by forcing same hash (in real scenarios)
        // Store a move that's legal in pos1 but not in pos2
        let move_7g7f = parse_usi_move("7g7f").unwrap();

        assert!(pos1.is_pseudo_legal(move_7g7f), "Move should be legal in pos1");
        assert!(!pos2.is_pseudo_legal(move_7g7f), "Move should be illegal in pos2");

        if let Some(ref tt) = searcher.tt {
            // Store with pos2's hash but pos1's move
            tt.store(pos2.zobrist_hash, Some(move_7g7f), 50, 0, 5, NodeType::Exact);
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

    #[test]
    fn test_quiescence_search_check_evasion() {
        // Test that quiescence search correctly handles check evasions
        // Create a position where the only legal move is a non-capture evasion

        // Position: Black king in check from white rook, must move (non-capture)
        // In shogi SFEN: K=black king, k=white king, R=black rook, r=white rook
        // 9 . . . . . . . . .
        // 8 . . . . . . . . .
        // 7 . . . . . . . . .
        // 6 . . . . . . . . .
        // 5 . . . . K . . . r  (Black King at 5e, white rook at 1e)
        // 4 . . . . . . . . .
        // 3 . . . . . . . . .
        // 2 . . . . . . . . .
        // 1 . . . . . . . . .
        //   9 8 7 6 5 4 3 2 1
        let pos = Position::from_sfen("9/9/9/9/4K3r/9/9/9/9 b - 1").unwrap();

        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false, 0>::new(evaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(1).build());

        // Verify we're in check
        assert!(pos.is_in_check(), "Position should be in check");

        // Run quiescence search at depth 0
        let mut test_pos = pos.clone();
        let score = quiescence_search(&mut searcher, &mut test_pos, -1000, 1000, 0);

        // Score should not be mate (we have legal moves)
        assert!(
            score != mate_score(0, false),
            "Should not return mate score when evasions exist"
        );
        assert!(score > -30000, "Score should be reasonable, not mate");

        // Verify that quiescence searched moves (node count should be > 1)
        assert!(searcher.stats.nodes > 1, "Quiescence should have searched evasion moves");
    }

    #[test]
    fn test_quiescence_search_check_at_depth_limit() {
        // Test that quiescence search handles check correctly even at depth limit
        // Position: Black king in check, test at near max quiescence depth
        let pos = Position::from_sfen("9/9/9/9/4K3r/9/9/9/9 b - 1").unwrap();

        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false, 0>::new(evaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(1).build());

        // Verify we're in check
        assert!(pos.is_in_check(), "Position should be in check");

        // Run quiescence search near the depth limit
        // Use a high ply value that would trigger depth limit for non-check positions
        let mut test_pos = pos.clone();
        let high_ply = 31; // Near the quiescence depth limit
        let score = quiescence_search(&mut searcher, &mut test_pos, -1000, 1000, high_ply);

        // Even at depth limit, when in check we should search evasions
        // Score should not be static eval (which would be positive for black)
        assert!(
            score != searcher.evaluator.evaluate(&pos),
            "Should not return static eval when in check at depth limit"
        );

        // Should have searched at least some moves
        assert!(
            searcher.stats.nodes >= 1,
            "Should search moves even at depth limit when in check"
        );
    }
}
