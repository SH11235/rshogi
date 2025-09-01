//! Root search implementation
//!
//! Handles the search from the root position with aspiration windows

use crate::{
    evaluation::evaluate::Evaluator,
    movegen::MoveGenerator,
    search::{
        common::mate_score,
        constants::SEARCH_INF,
        unified::{tt_operations::TTOperations, UnifiedSearcher},
    },
    shogi::{Move, Position},
};

/// Search from root position with aspiration window
pub fn search_root_with_window<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING>,
    pos: &mut Position,
    depth: u8,
    initial_alpha: i32,
    initial_beta: i32,
    previous_pv: &[Move],
) -> (i32, Vec<Move>)
where
    E: Evaluator + Send + Sync + 'static,
{
    // Complete any remaining garbage collection from previous search
    #[cfg(feature = "hashfull_filter")]
    if USE_TT {
        if let Some(ref tt) = searcher.tt {
            // Limit GC iterations to prevent potential infinite loops
            const MAX_GC_ITERATIONS: usize = 8;
            const MAX_GC_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);
            let gc_start = std::time::Instant::now();
            let mut gc_iterations = 0;
            while tt.should_trigger_gc()
                && gc_iterations < MAX_GC_ITERATIONS
                && gc_start.elapsed() < MAX_GC_BUDGET
            {
                tt.perform_incremental_gc(512); // Larger batch size before search starts
                gc_iterations += 1;
            }
        }
    }

    let mut alpha = initial_alpha;
    let beta = initial_beta;
    let mut best_score = -SEARCH_INF;
    let mut pv = Vec::new();

    // Generate all legal moves
    let move_gen = MoveGenerator::new();
    let moves = match move_gen.generate_all(pos) {
        Ok(moves) => moves,
        Err(_) => {
            // King not found - should not happen in valid position
            return (-SEARCH_INF, pv);
        }
    };

    if moves.is_empty() {
        // No legal moves - checkmate or stalemate
        if pos.is_in_check() {
            // Root position is checkmate - update mate distance
            searcher.context.update_mate_distance(0);
            return (mate_score(0, false), pv); // Getting mated at root
        } else {
            return (0, pv); // Stalemate
        }
    }

    // Get TT move for root position if available
    let tt_move = if USE_TT {
        searcher
            .tt
            .as_ref()
            .and_then(|tt| tt.probe(pos.zobrist_hash))
            .and_then(|e| e.get_move())
    } else {
        None
    };

    // Order moves - avoid Vec to SmallVec conversion
    let (ordered_slice, _owned_moves);
    if USE_TT || USE_PRUNING {
        let moves_vec = searcher.ordering.order_moves_at_root(pos, &moves, tt_move, previous_pv);
        _owned_moves = Some(moves_vec);
        let vec_ref = _owned_moves.as_ref().unwrap();
        ordered_slice = &vec_ref[..];
    } else {
        ordered_slice = moves.as_slice();
        _owned_moves = None;
    };

    // Note: Prefetching is done selectively inside the move loop below

    // Search each move
    for (move_idx, &mv) in ordered_slice.iter().enumerate() {
        // In debug builds, validate that all moves from move generator are legal
        // (this should always be true, so we just assert it)
        #[cfg(debug_assertions)]
        {
            // Since moves come from generate_all(), they should all be legal
            // TT move is not injected into the list, so this check is purely defensive
            debug_assert!(
                pos.is_pseudo_legal(mv),
                "Move from generate_all() should be pseudo-legal: {} in position {}",
                crate::usi::move_to_usi(&mv),
                crate::usi::position_to_sfen(pos)
            );
        }

        // Make move
        let undo_info = pos.do_move(mv);

        // Note: Node counting is done in alpha_beta() to avoid double counting

        // Save child hash for PV owner validation
        // This is the Zobrist hash of the child position after making the move
        let _child_hash = pos.zobrist_hash;

        // Prefetch TT entry for the new position (root moves are always important)
        if USE_TT && !searcher.disable_prefetch {
            searcher.prefetch_tt(pos.zobrist_hash);
        }

        // Search with null window for moves after the first
        let score = if move_idx == 0 {
            -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha, 1)
        } else {
            // Late Move Reduction (if enabled)
            let reduction = if USE_PRUNING && depth >= 3 && move_idx >= 4 && !pos.is_in_check() {
                // Use more sophisticated reduction based on depth and move count
                crate::search::unified::pruning::lmr_reduction(depth, move_idx as u32)
            } else {
                0
            };

            let reduced_depth = (depth - 1).saturating_sub(reduction);
            let score = -super::alpha_beta(searcher, pos, reduced_depth, -alpha - 1, -alpha, 1);

            // Re-search if score beats alpha
            if score > alpha && score < beta {
                -super::alpha_beta(searcher, pos, depth - 1, -beta, -alpha, 1)
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

        // Phase 1: advise rounded stop near hard while iterating root moves
        if let Some(ref tm) = searcher.time_manager {
            let elapsed_ms = searcher.context.elapsed().as_millis() as u64;
            tm.advise_after_iteration(elapsed_ms);
        }

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

            // Check if this is a mate score and update mate distance
            if crate::search::common::is_mate_score(score) {
                if let Some(distance) = crate::search::common::extract_mate_distance(score) {
                    searcher.context.update_mate_distance(distance);
                }
            }

            pv.clear();
            pv.push(mv);

            // Get PV from recursive search (stack-based)
            let child_pv: &[crate::shogi::Move] =
                if crate::search::types::SearchStack::is_valid_ply(1) {
                    &searcher.search_stack[1].pv_line
                } else {
                    &[]
                };

            // Debug logging for PV construction at root
            #[cfg(debug_assertions)]
            if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                eprintln!(
                    "[ROOT PV] depth={depth}, move_idx={move_idx}, best_move={}, score={score}",
                    crate::usi::move_to_usi(&mv)
                );
                if !child_pv.is_empty() {
                    eprintln!(
                        "  Child PV (ply=1): {}",
                        child_pv.iter().map(crate::usi::move_to_usi).collect::<Vec<_>>().join(" ")
                    );
                } else {
                    eprintln!("  Child PV (ply=1): <empty>");
                }
            }

            // Extend with child PV (stack-based). In debug, optionally validate.
            #[cfg(not(debug_assertions))]
            {
                pv.extend_from_slice(child_pv);
            }
            #[cfg(debug_assertions)]
            {
                if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                    // Validate child PV moves before extending
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
                            eprintln!("[WARNING] Invalid move in child PV at root, truncating");
                            eprintln!("  Move: {}", crate::usi::move_to_usi(&child_mv));
                            eprintln!("  Position: {}", crate::usi::position_to_sfen(&temp_pos));
                            break;
                        }
                    }
                    for (child_mv, child_undo) in undo_infos.iter().rev() {
                        temp_pos.undo_move(*child_mv, child_undo.clone());
                    }
                    temp_pos.undo_move(mv, undo_info);
                    pv.extend_from_slice(&valid_child_pv);
                } else {
                    pv.extend_from_slice(child_pv);
                }
            }

            // Notify time manager that PV head changed to refresh PV stability window
            if let Some(ref tm) = searcher.time_manager {
                tm.on_pv_change(depth as u32);
            }

            if score > alpha {
                alpha = score;
            }
        }
    }

    // Validate PV before returning
    if !pv.is_empty() {
        // Debug logging for final PV construction
        #[cfg(debug_assertions)]
        if std::env::var("SHOGI_DEBUG_PV").is_ok() {
            eprintln!(
                "[ROOT FINAL PV] depth={depth}, score={best_score}, pv_len={}, pv={}",
                pv.len(),
                pv.iter().map(crate::usi::move_to_usi).collect::<Vec<_>>().join(" ")
            );

            // Check for suspicious moves in PV
            if pv.len() >= 8 {
                if let Some(&mv) = pv.get(7) {
                    eprintln!("  PV[7] (8th move) = {}", crate::usi::move_to_usi(&mv));
                }
            }
        }

        // Use centralized PV trimming function
        // This ensures all PVs are validated consistently
        let original_len = pv.len();
        pv = super::pv::trim_legal_pv(pos.clone(), &pv);

        // Record trimming statistics
        crate::search::SearchStats::bump(&mut searcher.stats.pv_trim_checks, 1);
        if pv.len() < original_len {
            crate::search::SearchStats::bump(&mut searcher.stats.pv_trim_cuts, 1);
        }

        // Full validation only in debug builds
        #[cfg(debug_assertions)]
        {
            // First check occupancy invariants (doesn't rely on move generator)
            super::pv_validation::pv_local_sanity(pos, &pv);
            // Then check legal moves
            super::pv_validation::assert_pv_legal(pos, &pv);
        }
    }

    (best_score, pv)
}
