//! Null move pruning implementation
//!
//! Allows the opponent to make two moves in a row to detect forced moves

use crate::{
    evaluation::evaluate::Evaluator,
    search::unified::UnifiedSearcher,
    shogi::{PieceType, Position},
};

/// Check if position has non-pawn material
pub fn has_non_pawn_material(pos: &Position) -> bool {
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

    // Check for promoted pawns (tokins) - they are equivalent to gold
    // Get all pawns and check if any are promoted
    let pawn_bb = pos.board.pieces_of_type_and_color(PieceType::Pawn, color);
    let promoted_bb = pos.board.promoted_bb;

    // If any pawn is promoted, we have non-pawn material (tokin = gold)
    if !(pawn_bb & promoted_bb).is_empty() {
        return true;
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
pub fn try_null_move<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &mut UnifiedSearcher<E, USE_TT, USE_PRUNING>,
    pos: &mut Position,
    depth: u8,
    beta: i32,
    ply: u16,
) -> Option<i32>
where
    E: Evaluator + Send + Sync + 'static,
{
    // Don't do null move in check or if we might be in zugzwang
    if pos.is_in_check() {
        return None;
    }

    // Check if we have non-pawn material (simplified check)
    if has_non_pawn_material(pos) {
        // Make null move using the Position's method
        let undo_info = pos.do_null_move();

        // Search with reduced depth using pruning module
        let reduction = crate::search::unified::pruning::null_move_reduction(depth);
        let score = -super::alpha_beta(
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
