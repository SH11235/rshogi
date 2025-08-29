use crate::movegen::MoveGenerator;
use crate::shogi::Position;
use crate::usi::move_to_usi;

/// Generate an emergency move using simple heuristics.
/// Returns a USI string if any legal move exists.
///
/// Heuristics:
/// - prefer captures (via hint)
/// - then a small set of common opening/developing moves
/// - otherwise the first legal move
pub fn emergency_move_usi(position: &Position) -> Option<String> {
    let movegen = MoveGenerator::new();
    let Ok(legal_moves) = movegen.generate_all(position) else {
        return None;
    };
    if legal_moves.is_empty() {
        return None;
    }

    let slice = legal_moves.as_slice();
    let common_opening_moves = [
        // Black (sente) common moves
        "7g7f", "2g2f", "6i7h", "5i6h", "8h7g", "2h7h", // White (gote) common moves
        "3c3d", "7c7d", "6a7b", "5a6b", "2b7b", "8c8d",
    ];

    let best = slice
        .iter()
        .copied()
        .max_by_key(|m| {
            let s = move_to_usi(m);
            if m.is_capture_hint() {
                100
            } else if common_opening_moves.contains(&s.as_str()) {
                10
            } else {
                0
            }
        })
        .unwrap_or(slice[0]);

    Some(move_to_usi(&best))
}
