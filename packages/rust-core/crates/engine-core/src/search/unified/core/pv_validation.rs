//! PV (Principal Variation) validation utilities
//!
//! Functions for validating that PV moves are legal and consistent

use crate::shogi::{Move, Position};

/// Validate that all moves in a PV are legal from the given position
pub(crate) fn assert_pv_legal(pos: &Position, pv: &[Move]) {
    let mut p = pos.clone();
    for (i, mv) in pv.iter().enumerate() {
        if !p.is_legal_move(*mv) {
            crate::pv_debug!("[BUG] illegal pv at ply {i}: {}", crate::usi::move_to_usi(mv));
            crate::pv_debug!("  Position: {}", crate::usi::position_to_sfen(&p));
            crate::pv_debug!(
                "  Full PV: {}",
                pv.iter().map(crate::usi::move_to_usi).collect::<Vec<_>>().join(" ")
            );
            // Log issue at appropriate level - this is a serious bug that should be investigated
            log::error!("[BUG] Illegal move in PV at ply {}: {}", i, crate::usi::move_to_usi(mv));
            // Still continue checking rather than break to identify all issues
            break;
        }
        let _undo_info = p.do_move(*mv);
        // For PV validation, we don't need to undo since we're working on a clone
    }
}

/// Validate PV using occupancy invariants (not relying on move generator)
pub fn pv_local_sanity(pos: &Position, pv: &[Move]) {
    let mut p = pos.clone();

    for (mut _i, &mv) in pv.iter().enumerate() {
        // Skip null moves
        if mv == Move::NULL {
            crate::pv_debug!("[BUG] NULL move in PV at ply {i}");
            return;
        }

        let _usi = crate::usi::move_to_usi(&mv);

        // Pre-move validation
        if mv.is_drop() {
            // For drops: check we have the piece in hand
            let piece_type = mv.drop_piece_type();
            let hands = &p.hands[p.side_to_move as usize];
            let Some(hand_idx) = piece_type.hand_index() else {
                crate::pv_debug!("[BUG] Invalid drop piece type (King) at ply {_i}: {_usi}");
                return;
            };
            let count = hands[hand_idx];
            if count == 0 {
                crate::pv_debug!("[BUG] No piece in hand for drop at ply {_i}: {_usi}");
                crate::pv_debug!("  Position: {}", crate::usi::position_to_sfen(&p));
                return;
            }
        } else {
            // For normal moves: check piece exists at from square
            if let Some(from) = mv.from() {
                if p.piece_at(from).is_none() {
                crate::pv_debug!("[BUG] No piece at from square at ply {_i}: {_usi}");
                    crate::pv_debug!("  Position: {}", crate::usi::position_to_sfen(&p));
                    crate::pv_debug!("  From square {from:?} is empty");
                    return;
                }
            } else {
                crate::pv_debug!("[BUG] Normal move has no from square at ply {_i}: {_usi}");
                return;
            }
        }

        // Check if move is pseudo-legal before applying
        if !p.is_pseudo_legal(mv) {
            crate::pv_debug!("[BUG] Illegal move in PV at ply {_i}: {_usi}");
            crate::pv_debug!("  Position: {}", crate::usi::position_to_sfen(&p));
            crate::pv_debug!("  Move is not pseudo-legal");
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
                    crate::pv_debug_exec!({
                        eprintln!("[BUG] From square not cleared at ply {_i}: {_usi}");
                        eprintln!("  Position after move: {}", crate::usi::position_to_sfen(&p));
                    });
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
                crate::pv_debug!("[BUG] To square not occupied by our piece at ply {_i}: {_usi}");
                crate::pv_debug!("  Position after move: {}", crate::usi::position_to_sfen(&p));
                return;
            }
        }
    }
}
