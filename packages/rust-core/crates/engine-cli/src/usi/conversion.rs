//! Conversion between USI notation and engine types

use anyhow::{anyhow, Result};
use engine_core::movegen::MoveGen;
use engine_core::shogi::{Move, MoveList, Position};
use engine_core::usi::{parse_sfen, parse_usi_move, position_to_sfen};
use log::debug;

/// Create a Position from USI position command data
pub fn create_position(startpos: bool, sfen: Option<&str>, moves: &[String]) -> Result<Position> {
    // Create initial position
    let mut pos = if startpos {
        Position::startpos()
    } else if let Some(sfen_str) = sfen {
        parse_sfen(sfen_str).map_err(|e| anyhow!(e))?
    } else {
        return Err(anyhow!(
            "Position command must specify either 'startpos' or 'sfen <fen_string>'"
        ));
    };

    // Apply moves with validation
    // Note: Currently MoveGen is stateless and can be reused across multiple calls.
    // If MoveGen becomes stateful in the future, consider creating a new instance
    // for each position or ensuring proper state reset.
    let mut move_gen = MoveGen::new();
    let mut legal_moves = MoveList::new();

    for (i, move_str) in moves.iter().enumerate() {
        let mv = parse_usi_move(move_str).map_err(|e| anyhow!(e))?;

        // Clear legal moves list before generating new moves
        legal_moves.clear();

        // Generate all legal moves for current position
        move_gen.generate_all(&pos, &mut legal_moves);

        // Find matching legal move with promotion priority
        let mut fallback: Option<Move> = None;
        let mut exact: Option<Move> = None;

        for &lm in legal_moves.as_slice() {
            let matched = if mv.is_drop() || lm.is_drop() {
                // Drop moves: match to square and drop piece type
                mv.is_drop() == lm.is_drop()
                    && mv.drop_piece_type() == lm.drop_piece_type()
                    && mv.to() == lm.to()
            } else {
                // Normal moves: match from and to squares
                mv.from() == lm.from() && mv.to() == lm.to()
            };

            if matched {
                if lm.is_promote() == mv.is_promote() {
                    // Found exact promotion match
                    exact = Some(lm);
                    break;
                }
                if fallback.is_none() {
                    // Store first match as fallback
                    fallback = Some(lm);
                }
            }
        }

        let legal_move = exact.or(fallback);

        if let Some(legal_mv) = legal_move {
            // Log if USI specified promotion but it's not possible
            if mv.is_promote() && !legal_mv.is_promote() {
                debug!(
                    "Move {} specified promotion (+) but piece cannot promote at this position",
                    move_str
                );
            }
            // Use the actual legal move which has correct piece type and capture info
            pos.do_move(legal_mv);
        } else {
            // Additional debugging
            debug!(
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
                        debug!(
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
                debug!("No legal moves found from square {:?}", mv.from());
                // Show first few moves from nearby squares
                debug!("First 10 legal moves:");
                for (i, &legal_mv) in legal_moves.as_slice().iter().take(10).enumerate() {
                    debug!("  {}: from={:?}, to={:?}", i, legal_mv.from(), legal_mv.to());
                }
            }
            return Err(anyhow!(
                "Illegal move '{}' at move {} in position after: {} (parsed: {:?}, side_to_move: {:?}, legal_moves_count: {}, sfen: {})",
                move_str,
                i + 1,
                if i == 0 {
                    "initial position".to_string()
                } else {
                    format!("{i} moves")
                },
                mv,
                pos.side_to_move,
                legal_moves.len(),
                position_to_sfen(&pos)
            ));
        }
    }

    Ok(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_position_startpos() {
        let pos = create_position(true, None, &[]).unwrap();
        assert_eq!(pos.side_to_move, engine_core::shogi::Color::Black);
    }

    #[test]
    fn test_create_position_with_moves() {
        // In shogi initial position with new convention:
        // - Black pieces are at ranks 6-8 (USI: g-i)
        // - White pieces are at ranks 0-2 (USI: a-c)
        // Common opening move: push the pawn in front of the rook
        let moves = vec!["2g2f".to_string()]; // Move Black pawn forward (rank 6->5)
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
        let moves = vec!["7a7b".to_string()]; // This is a White pawn at rank 0
        let result = create_position(true, None, &moves);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Illegal move"));

        // Legal move followed by illegal move
        let moves = vec!["2g2f".to_string(), "2f2e".to_string()]; // Can't move same piece twice in a row
        let result = create_position(true, None, &moves);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Illegal move"));
        assert!(err_msg.contains("at move 2"));
    }

    #[test]
    fn test_promotion_mismatch_fallback() {
        // Test that invalid promotion ("+") is ignored and falls back to normal move
        // Move sequence: advance pawn to a position where it cannot promote
        let moves = vec![
            "7g7f".to_string(),  // Black pawn forward
            "3c3d".to_string(),  // White pawn forward
            "2g2f+".to_string(), // Black pawn forward with invalid promotion (not in promotion zone)
        ];

        // This should succeed - the "+" should be ignored
        let result = create_position(true, None, &moves);
        assert!(result.is_ok(), "Should accept move with invalid promotion flag");

        let pos = result.unwrap();
        assert_eq!(pos.side_to_move, engine_core::shogi::Color::White);
    }

    #[test]
    fn test_promotion_priority_matching() {
        // Test that when both promoted and non-promoted moves are legal,
        // the exact match (with promotion flag) is preferred
        use engine_core::shogi::{Piece, PieceType, Square};

        // Create a position where a silver can promote
        let mut pos = Position::empty();

        // Place kings (required for legal position)
        pos.board.put_piece(
            Square::new(4, 8),
            Piece::new(PieceType::King, engine_core::shogi::Color::Black),
        );
        pos.board.put_piece(
            Square::new(4, 0),
            Piece::new(PieceType::King, engine_core::shogi::Color::White),
        );

        // Place a black silver at 3d (can move to promotion zone)
        pos.board.put_piece(
            Square::new(6, 3), // 3d in internal coordinates
            Piece::new(PieceType::Silver, engine_core::shogi::Color::Black),
        );

        // Place a white pawn that the silver can capture in promotion zone
        pos.board.put_piece(
            Square::new(7, 2), // 2c in internal coordinates - diagonal from silver
            Piece::new(PieceType::Pawn, engine_core::shogi::Color::White),
        );

        pos.side_to_move = engine_core::shogi::Color::Black;

        // Convert position to SFEN
        let sfen = position_to_sfen(&pos);

        // Test that "3d2c+" (with promotion) is correctly applied
        let moves_with_promotion = vec!["3d2c+".to_string()];
        let result = create_position(false, Some(&sfen), &moves_with_promotion);
        assert!(result.is_ok(), "Should accept silver promotion move");

        // Test that "3d2c" (without promotion) is also accepted
        let moves_without_promotion = vec!["3d2c".to_string()];
        let result = create_position(false, Some(&sfen), &moves_without_promotion);
        assert!(result.is_ok(), "Should accept silver non-promotion move");
    }
}
