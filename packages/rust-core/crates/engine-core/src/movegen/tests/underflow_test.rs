//! Test for potential underflow issues in move generation

#[cfg(test)]
mod tests {
    use crate::{
        movegen::generator::MoveGenImpl,
        shogi::{board::Position, Color, Piece, PieceType, Square},
    };

    #[test]
    fn test_pawn_at_edge_no_underflow() {
        // Create a position with a black pawn at rank 0 (edge of board)
        // This should not generate any moves and should not panic
        let mut pos = Position::empty();

        // Place a black king (required)
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::King, Color::Black));

        // Place a white king (required)
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

        // Place a black pawn at rank 0 (shouldn't be able to move)
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::Pawn, Color::Black));

        pos.side_to_move = Color::Black;

        // This should not panic even with overflow checks
        let mut generator = MoveGenImpl::new(&pos);
        let moves = generator.generate_all();

        // The pawn at rank 0 should not generate any moves
        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| {
                if let Some(from) = m.from() {
                    from == Square::new(4, 0)
                } else {
                    false
                }
            })
            .collect();

        assert_eq!(pawn_moves.len(), 0, "Pawn at rank 0 should not generate any moves");
    }

    #[test]
    fn test_white_pawn_at_edge_no_overflow() {
        // Similar test for white pawn at rank 8
        let mut pos = Position::empty();

        // Place kings
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::King, Color::White));

        // Place a white pawn at rank 8 (shouldn't be able to move)
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::Pawn, Color::White));

        pos.side_to_move = Color::White;

        // This should not panic even with overflow checks
        let mut generator = MoveGenImpl::new(&pos);
        let moves = generator.generate_all();

        // The pawn at rank 8 should not generate any moves
        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| {
                if let Some(from) = m.from() {
                    from == Square::new(4, 8)
                } else {
                    false
                }
            })
            .collect();

        assert_eq!(pawn_moves.len(), 0, "Pawn at rank 8 should not generate any moves");
    }
}
