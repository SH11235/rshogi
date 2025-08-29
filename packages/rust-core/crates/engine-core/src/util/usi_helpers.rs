use crate::movegen::MoveGenerator;
use crate::shogi::{Move, Position};
use crate::usi::move_to_usi;

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

/// Resolve a USI move string to a concrete legal Move for the given position.
///
/// - Prefers exact promotion flag match when both promoted and non-promoted are legal
/// - Falls back to the first legal match with same from/to (or drop target)
pub fn resolve_usi_move(position: &Position, usi_move: &str) -> Option<Move> {
    let Ok(parsed) = crate::usi::parse_usi_move(usi_move) else {
        return None;
    };
    let movegen = MoveGenerator::new();
    let Ok(legal_moves) = movegen.generate_all(position) else {
        return None;
    };

    let mut fallback: Option<Move> = None;
    for &lm in legal_moves.as_slice() {
        let matched = if parsed.is_drop() || lm.is_drop() {
            parsed.is_drop() == lm.is_drop()
                && parsed.drop_piece_type() == lm.drop_piece_type()
                && parsed.to() == lm.to()
        } else {
            parsed.from() == lm.from() && parsed.to() == lm.to()
        };

        if matched {
            if lm.is_promote() == parsed.is_promote() {
                return Some(lm);
            }
            if fallback.is_none() {
                fallback = Some(lm);
            }
        }
    }
    fallback
}

/// Normalize a USI move string to the engine's canonical USI for a legal Move.
/// Returns None if the move is not legal in the given position.
pub fn normalize_usi_move(position: &Position, usi_move: &str) -> Option<String> {
    resolve_usi_move(position, usi_move).map(|mv| move_to_usi(&mv))
}
