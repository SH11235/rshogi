//! Root search implementation
//!
//! Handles the search from the root position with aspiration windows

use crate::{
    evaluation::evaluate::Evaluator,
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
                tt.incremental_gc(512); // Larger batch size before search starts
                gc_iterations += 1;
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

    // Note: Prefetching is done selectively inside the move loop below

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

            // In release builds, trust the child PV without validation
            // This avoids expensive cloning and move validation in hot path
            #[cfg(not(debug_assertions))]
            {
                pv.extend_from_slice(&child_pv);
            }

            // In debug builds or with SHOGI_DEBUG_PV, validate child PV moves
            #[cfg(debug_assertions)]
            {
                if std::env::var("SHOGI_DEBUG_PV").is_ok() {
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
                            eprintln!("[WARNING] Invalid move in child PV at root, truncating");
                            eprintln!("  Move: {}", crate::usi::move_to_usi(&child_mv));
                            eprintln!("  Position: {}", crate::usi::position_to_sfen(&temp_pos));
                            break;
                        }
                    }

                    // Undo all child moves in reverse order
                    for (child_mv, child_undo) in undo_infos.iter().rev() {
                        temp_pos.undo_move(*child_mv, child_undo.clone());
                    }
                    temp_pos.undo_move(mv, undo_info);
                    pv.extend_from_slice(&valid_child_pv);
                } else {
                    // Debug build but no SHOGI_DEBUG_PV - just use child PV as-is
                    pv.extend_from_slice(child_pv);
                }
            }

            if score > alpha {
                alpha = score;
            }
        }
    }

    // Validate PV before returning
    if !pv.is_empty() {
        // Minimal O(1) sanity check even in release builds
        if pv.contains(&Move::NULL) || pv.len() > crate::search::constants::MAX_PLY {
            pv.clear(); // Discard corrupted PV
        } else {
            // Full validation only in debug builds
            #[cfg(debug_assertions)]
            {
                // First check occupancy invariants (doesn't rely on move generator)
                super::pv_validation::pv_local_sanity(pos, &pv);
                // Then check legal moves
                super::pv_validation::assert_pv_legal(pos, &pv);
            }
        }
    }

    (best_score, pv)
}
