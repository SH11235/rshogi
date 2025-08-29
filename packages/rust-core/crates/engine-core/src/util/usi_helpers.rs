use crate::movegen::MoveGenerator;
use crate::shogi::Position;

/// Check if a USI move string is legal in the given position.
///
/// This function is tolerant to a specific USI edge: it treats a move with a promotion flag
/// as matching a non-promoting legal move with same from/to (for cases where `+` is present
/// even when promotion is impossible). This mirrors CLI-side behavior to avoid regressions.
pub fn is_legal_usi_move(position: &Position, usi_move: &str) -> bool {
    // Parse the USI move
    let Ok(parsed_move) = crate::usi::parse_usi_move(usi_move) else {
        return false;
    };

    // Generate legal moves and compare semantically
    let movegen = MoveGenerator::new();
    let Ok(legal_moves) = movegen.generate_all(position) else {
        return false;
    };

    legal_moves.as_slice().iter().any(|&legal_move| {
        // Basic move matching
        parsed_move.from() == legal_move.from()
            && parsed_move.to() == legal_move.to()
            && parsed_move.is_drop() == legal_move.is_drop()
            && (!parsed_move.is_drop()
                || parsed_move.drop_piece_type() == legal_move.drop_piece_type())
            && (
                // Either promotion flags match exactly
                parsed_move.is_promote() == legal_move.is_promote()
                // OR the parsed move tries to promote but the legal move doesn't allow it
                // (This mirrors existing CLI tolerance for e.g. "2b8h+")
                || (parsed_move.is_promote() && !legal_move.is_promote())
            )
    })
}
