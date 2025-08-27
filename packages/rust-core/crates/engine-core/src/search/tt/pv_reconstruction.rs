//! Common PV reconstruction functionality for transposition tables

use super::TTEntry;
use crate::movegen::MoveGenerator;
use crate::search::constants::MAX_PLY;
use crate::search::NodeType;
use crate::shogi::{Move, Position, UndoInfo};
use crate::usi::move_to_usi;
use std::collections::HashSet;

/// Trait for types that can probe transposition table entries
pub trait TTProbe {
    /// Probe the transposition table for a given hash
    fn probe(&self, hash: u64) -> Option<TTEntry>;
}

/// Generic PV reconstruction from transposition table
///
/// This function follows the best moves stored in EXACT TT entries to build
/// a principal variation. It stops at the first non-EXACT entry to ensure
/// PV reliability.
///
/// # Arguments
/// * `tt` - Transposition table implementing TTProbe
/// * `pos` - Current position to start reconstruction from
/// * `max_depth` - Maximum depth to search (prevents infinite loops)
///
/// # Returns
/// * Vector of moves forming the PV (empty if no PV found)
pub fn reconstruct_pv_generic<T: TTProbe>(tt: &T, pos: &mut Position, max_depth: u8) -> Vec<Move> {
    let mut pv = Vec::new();
    let mut visited_hashes = HashSet::new();
    let mut undo_stack: Vec<(Move, UndoInfo)> = Vec::new();

    // Limit PV length to prevent excessive reconstruction
    let max_pv_length = max_depth.min(MAX_PLY as u8) as usize;

    for _ in 0..max_pv_length {
        let hash = pos.zobrist_hash;

        // Check for cycles
        if !visited_hashes.insert(hash) {
            log::debug!("PV reconstruction: Cycle detected at hash {hash:016x}");
            break;
        }

        // Probe TT
        let entry = match tt.probe(hash) {
            Some(e) if e.matches(hash) => {
                log::trace!("PV reconstruction: Found TT entry for hash {hash:016x}");
                e
            }
            Some(e) => {
                log::trace!("PV reconstruction: TT entry hash mismatch. Entry hash: {:016x}, Looking for: {hash:016x}", e.key());
                break;
            }
            None => {
                log::trace!("PV reconstruction: No TT entry for hash {hash:016x}");
                break;
            }
        };

        // Only follow EXACT entries
        if entry.node_type() != NodeType::Exact {
            log::trace!(
                "PV reconstruction: Stopping at non-EXACT node (type: {:?}) at depth {}",
                entry.node_type(),
                pv.len()
            );
            break;
        }

        // Skip shallow depth entries for PV reconstruction reliability
        // Shallow entries are more likely to be from different positions due to hash collisions
        const MIN_DEPTH_FOR_PV_TRUST: u8 = 4;
        if entry.depth() < MIN_DEPTH_FOR_PV_TRUST && !pv.is_empty() {
            log::trace!(
                "PV reconstruction: Stopping at shallow entry (depth: {}) at ply {}",
                entry.depth(),
                pv.len()
            );
            break;
        }

        // Get the best move
        let best_move = match entry.get_move() {
            Some(mv) => mv,
            None => {
                log::trace!("PV reconstruction: No move in TT entry at depth {}", pv.len());
                break;
            }
        };

        // Validate move is legal
        // Since TT stores moves as 16-bit, we need to find the matching legal move
        // with full piece type information
        let move_gen = MoveGenerator::new();
        let legal_moves = match move_gen.generate_all(pos) {
            Ok(moves) => moves,
            Err(_) => {
                log::warn!("Failed to generate moves during PV reconstruction");
                break;
            }
        };
        let legal_move =
            legal_moves.as_slice().iter().find(|m| m.equals_without_piece_type(&best_move));

        let valid_move = match legal_move {
            Some(m) => *m,
            None => {
                log::warn!(
                    "PV reconstruction: Illegal move {} at depth {}",
                    move_to_usi(&best_move),
                    pv.len()
                );
                break;
            }
        };

        // Add move to PV (use the valid move with piece type info)
        pv.push(valid_move);

        // Make the move
        let undo_info = pos.do_move(valid_move);
        undo_stack.push((valid_move, undo_info));

        // Check for terminal positions
        if pos.is_draw() {
            log::trace!("PV reconstruction: Draw position reached at depth {}", pv.len());
            break;
        }

        // Check if we have no legal moves (mate)
        let move_gen = MoveGenerator::new();
        let has_moves = match move_gen.generate_all(pos) {
            Ok(moves) => !moves.is_empty(),
            Err(_) => false,
        };
        if !has_moves {
            log::trace!("PV reconstruction: Mate position reached at depth {}", pv.len());
            break;
        }
    }

    // Undo all moves
    for (mv, undo_info) in undo_stack.into_iter().rev() {
        pos.undo_move(mv, undo_info);
    }

    log::debug!("PV reconstruction: Found {} moves from TT (max_depth: {})", pv.len(), max_depth);

    pv
}
