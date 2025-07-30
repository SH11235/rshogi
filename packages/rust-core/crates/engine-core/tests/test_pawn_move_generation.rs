#[cfg(test)]
mod tests {
    use engine_core::movegen::MoveGen;
    use engine_core::shogi::{Color, MoveList, PieceType, Position, Square};

    #[test]
    fn test_initial_pawn_moves() {
        // Get initial position
        let pos = Position::startpos();

        // Generate all legal moves
        let mut movegen = MoveGen::new();
        let mut moves = MoveList::new();
        movegen.generate_all(&pos, &mut moves);

        println!("Total moves from initial position: {}", moves.len());

        // Look for pawn moves, specifically from 8g (internal: file=1, rank=6)
        let sq_8g = Square::new(1, 6);
        let sq_8f = Square::new(1, 5);

        println!(
            "\nLooking for move from {} (index {}) to {} (index {})",
            sq_8g,
            sq_8g.index(),
            sq_8f,
            sq_8f.index()
        );

        // Check all pawn moves
        let mut pawn_moves = Vec::new();
        for mv in &moves {
            if !mv.is_drop() {
                if let Some(from) = mv.from() {
                    let to = mv.to();

                    // Check if this is a pawn move (from rank 6 for Black)
                    if from.rank() == 6 {
                        let from_str = from.to_string();
                        let to_str = to.to_string();
                        let mv_str = format!("{from_str}{to_str}");
                        pawn_moves.push((mv_str.clone(), from, to));
                        println!(
                            "Pawn move: {} (from index {} to index {})",
                            mv_str,
                            from.index(),
                            to.index()
                        );
                    }
                }
            }
        }

        println!("\nTotal pawn moves: {}", pawn_moves.len());

        // Check if 8g8f exists
        let has_8g8f = pawn_moves.iter().any(|(mv_str, _, _)| mv_str == "8g8f");
        assert!(has_8g8f, "Move 8g8f should be available from initial position");

        // Also check 7g7f
        let has_7g7f = pawn_moves.iter().any(|(mv_str, _, _)| mv_str == "7g7f");
        assert!(has_7g7f, "Move 7g7f should be available from initial position");
    }

    #[test]
    fn test_specific_8g_square() {
        let pos = Position::startpos();

        // 8g is at internal coordinates (1, 6)
        let sq_8g = Square::new(1, 6);
        println!(
            "8g internal representation: file={}, rank={}, index={}, display={}",
            sq_8g.file(),
            sq_8g.rank(),
            sq_8g.index(),
            sq_8g
        );

        // Verify there's a Black pawn at 8g
        let piece = pos.board.piece_on(sq_8g);
        println!("Piece at 8g: {piece:?}");

        assert!(piece.is_some(), "There should be a piece at 8g");
        let piece = piece.unwrap();
        assert_eq!(piece.piece_type, PieceType::Pawn, "Piece at 8g should be a pawn");
        assert_eq!(piece.color, Color::Black, "Pawn at 8g should be Black");
    }
}
