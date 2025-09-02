use crate::movegen::MoveGenerator;
use crate::shogi::Position;
use crate::usi::move_to_usi;
use log::error;

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

    // Defensive: verify legality again via position API (double-check)
    let verified: Vec<_> = slice.iter().copied().filter(|m| position.is_legal_move(*m)).collect();
    let slice = if !verified.is_empty() {
        verified.as_slice()
    } else {
        slice
    };
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

    let usi = move_to_usi(&best);
    if !is_valid_usi_move_str(&usi) {
        error!("Emergency move produced invalid USI format: {}", usi);
        return None;
    }
    Some(usi)
}

/// Minimal USI format validator (no regex, zero allocation)
/// Accepts:
/// - normal moves: [1-9][a-i][1-9][a-i](+optional)
/// - drops: [PLNSGBR]*[1-9][a-i]
/// - special: resign, win, 0000
fn is_valid_usi_move_str(s: &str) -> bool {
    match s {
        "resign" | "win" | "0000" => return true,
        _ => {}
    }

    let bs = s.as_bytes();
    // Drop move: X*Yy (length 4)
    if bs.len() == 4 && bs[1] == b'*' {
        let p = bs[0];
        let file = bs[2];
        let rank = bs[3];
        return matches!(p, b'P' | b'L' | b'N' | b'S' | b'G' | b'B' | b'R')
            && (b'1'..=b'9').contains(&file)
            && (b'a'..=b'i').contains(&rank);
    }

    // Normal move: from(to) with optional '+' at end
    // 4 or 5 chars
    if bs.len() == 4 || bs.len() == 5 {
        let f_file = bs[0];
        let f_rank = bs[1];
        let t_file = bs[2];
        let t_rank = bs[3];
        let base_ok = (b'1'..=b'9').contains(&f_file)
            && (b'a'..=b'i').contains(&f_rank)
            && (b'1'..=b'9').contains(&t_file)
            && (b'a'..=b'i').contains(&t_rank);
        if !base_ok {
            return false;
        }
        if bs.len() == 5 {
            return bs[4] == b'+';
        }
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::is_valid_usi_move_str;

    #[test]
    fn test_usi_validator_accepts_valid_moves() {
        // Normal moves
        assert!(is_valid_usi_move_str("7g7f"));
        assert!(is_valid_usi_move_str("2b8h+"));
        assert!(is_valid_usi_move_str("9a9i"));
        assert!(is_valid_usi_move_str("1i1a+"));

        // Drops
        assert!(is_valid_usi_move_str("P*5e"));
        assert!(is_valid_usi_move_str("G*5e"));
        assert!(is_valid_usi_move_str("R*1a"));

        // Specials
        assert!(is_valid_usi_move_str("resign"));
        assert!(is_valid_usi_move_str("win"));
        assert!(is_valid_usi_move_str("0000"));
    }

    #[test]
    fn test_usi_validator_rejects_invalid_moves() {
        // Invalid coordinates
        assert!(!is_valid_usi_move_str("0a0a")); // 0 not allowed
        assert!(!is_valid_usi_move_str("7j7f")); // j out of range
        assert!(!is_valid_usi_move_str("10a1a")); // too long numbers
        assert!(!is_valid_usi_move_str("7g7")); // too short

        // Invalid drops
        assert!(!is_valid_usi_move_str("Z*5e")); // invalid piece
        assert!(!is_valid_usi_move_str("P**5e")); // malformed

        // Invalid promotion syntax
        assert!(!is_valid_usi_move_str("7g7f++")); // double plus
        assert!(!is_valid_usi_move_str("7g7")); // missing target

        // Garbage
        assert!(!is_valid_usi_move_str("abc"));
        assert!(!is_valid_usi_move_str(""));
    }
}
