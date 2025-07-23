//! Conversion between USI notation and engine types

use anyhow::{anyhow, Result};
use engine_core::movegen::MoveGen;
use engine_core::shogi::{Move, MoveList, Position};
use engine_core::usi::{move_to_usi, parse_sfen, parse_usi_move, position_to_sfen};

/// Convert a list of USI move strings to Move objects
pub fn parse_moves(move_strings: &[String]) -> Result<Vec<Move>> {
    move_strings.iter().map(|s| parse_usi_move(s).map_err(|e| anyhow!(e))).collect()
}

/// Convert a list of Move objects to USI strings
pub fn moves_to_usi(moves: &[Move]) -> Vec<String> {
    moves.iter().map(move_to_usi).collect()
}

/// Helper function to compare moves semantically (ignoring piece type encoding)
fn moves_equal(m1: Move, m2: Move) -> bool {
    // Compare the basic move properties
    m1.from() == m2.from() &&
    m1.to() == m2.to() &&
    m1.is_drop() == m2.is_drop() &&
    m1.is_promote() == m2.is_promote() &&
    // For drops, also compare the piece type
    (!m1.is_drop() || m1.drop_piece_type() == m2.drop_piece_type())
}

/// Create a Position from USI position command data
pub fn create_position(startpos: bool, sfen: Option<&str>, moves: &[String]) -> Result<Position> {
    // Create initial position
    let mut pos = if startpos {
        Position::startpos()
    } else if let Some(sfen_str) = sfen {
        parse_sfen(sfen_str).map_err(|e| anyhow!(e))?
    } else {
        return Err(anyhow!("Must specify either startpos or sfen"));
    };

    // Apply moves with validation
    let mut move_gen = MoveGen::new();
    let mut legal_moves = MoveList::new();

    for (i, move_str) in moves.iter().enumerate() {
        let mv = parse_usi_move(move_str).map_err(|e| anyhow!(e))?;

        // Generate all legal moves for current position
        move_gen.generate_all(&pos, &mut legal_moves);

        // Check if the parsed move is legal by comparing semantically
        let is_legal = legal_moves.as_slice().iter().any(|&legal_mv| moves_equal(legal_mv, mv));

        if !is_legal {
            // Additional debugging
            eprintln!(
                "Parsed move details: from={:?}, to={:?}, drop={}, promote={}",
                mv.from(),
                mv.to(),
                mv.is_drop(),
                mv.is_promote()
            );

            // Check if we can find any move with the same from/to squares
            let mut found_from_square = false;
            for &legal_mv in legal_moves.as_slice() {
                if legal_mv.from() == mv.from() {
                    found_from_square = true;
                    if legal_mv.to() == mv.to() {
                        eprintln!(
                            "Found similar legal move: from={:?}, to={:?}, drop={}, promote={}",
                            legal_mv.from(),
                            legal_mv.to(),
                            legal_mv.is_drop(),
                            legal_mv.is_promote()
                        );
                    }
                }
            }
            if !found_from_square {
                eprintln!("No legal moves found from square {:?}", mv.from());
                // Show first few moves from nearby squares
                eprintln!("First 10 legal moves:");
                for (i, &legal_mv) in legal_moves.as_slice().iter().take(10).enumerate() {
                    eprintln!("  {}: from={:?}, to={:?}", i, legal_mv.from(), legal_mv.to());
                }
            }
            return Err(anyhow!(
                "Illegal move '{}' at move {} in position after: {}",
                move_str,
                i + 1,
                if i == 0 {
                    "initial position".to_string()
                } else {
                    format!("{i} moves")
                }
            ));
        }

        pos.do_move(mv);
    }

    Ok(pos)
}

/// Convert Position to SFEN string
pub fn position_to_usi_string(pos: &Position) -> String {
    position_to_sfen(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_moves() {
        let move_strs = vec!["7g7f".to_string(), "3c3d".to_string()];
        let moves = parse_moves(&move_strs).unwrap();
        assert_eq!(moves.len(), 2);
        assert!(!moves[0].is_drop());
        assert!(!moves[1].is_drop());
    }

    #[test]
    fn test_create_position_startpos() {
        let pos = create_position(true, None, &[]).unwrap();
        assert_eq!(pos.side_to_move, engine_core::shogi::Color::Black);
    }

    #[test]
    fn test_create_position_with_moves() {
        // In shogi initial position:
        // - Black pieces are at ranks 0-2 (USI: a-c)
        // - White pieces are at ranks 6-8 (USI: g-i)
        // Common opening move: push the pawn in front of the bishop
        let moves = vec!["2c2d".to_string()]; // Move Black pawn forward (rank 2->3)
        let pos = create_position(true, None, &moves).unwrap();
        assert_eq!(pos.side_to_move, engine_core::shogi::Color::White);
    }

    #[test]
    fn test_create_position_illegal_move() {
        // Try to move a piece that doesn't exist
        let moves = vec!["5e5d".to_string()];
        let result = create_position(true, None, &moves);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Illegal move"));

        // Try to move opponent's piece (White pawn)
        let moves = vec!["7g7f".to_string()]; // This is a White pawn at rank 6
        let result = create_position(true, None, &moves);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Illegal move"));

        // Legal move followed by illegal move
        let moves = vec!["2c2d".to_string(), "2d2e".to_string()]; // Can't move same piece twice in a row
        let result = create_position(true, None, &moves);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Illegal move"));
        assert!(err_msg.contains("at move 2"));
    }
}
