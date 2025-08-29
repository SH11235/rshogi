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
        // Black (sente) common moves (avoid early king/gold moves)
        "7g7f", "2g2f", "3g3f", "4g4f", "5g5f", "6g6f", "8g8f", "9g9f", "8h7g", "2h7h",
        // White (gote) common moves
        "3c3d", "7c7d", "2c2d", "4c4d", "5c5d", "6c6d", "8c8d", "9c9d", "6a7b", "5a6b", "2b7b",
    ];

    let ply = position.ply as u32;
    let best = slice
        .iter()
        .copied()
        .max_by_key(|m| {
            let s = move_to_usi(m);
            // Prefer captures strongly
            let mut score = if m.is_capture_hint() { 200 } else { 0 };

            // Prefer a small set of common opening/developing moves
            if common_opening_moves.contains(&s.as_str()) {
                score += 50;
            }

            // Penalize early king moves heavily to avoid unnatural play
            if let Some(from_sq) = m.from() {
                if let Some(piece) = position.piece_at(from_sq) {
                    if piece.piece_type == crate::PieceType::King {
                        // Up to ply 80 (~40 moves total), discourage king moves
                        if ply < 80 {
                            score -= 1000;
                        } else {
                            score -= 200; // still de-prioritize later
                        }
                    }
                }
            }

            score
        })
        .unwrap_or(slice[0]);

    Some(move_to_usi(&best))
}
