//! Test modules for move generation

#[cfg(test)]
mod basic;
#[cfg(test)]
mod check_evasion;
#[cfg(test)]
mod checks;
#[cfg(test)]
mod checkers_orientation;
#[cfg(test)]
mod drops;
#[cfg(test)]
mod has_any_legal_move_test;
#[cfg(test)]
mod pieces;
#[cfg(test)]
mod promotion_from_promotion_zone;
#[cfg(test)]
mod underflow_test;

#[cfg(test)]
mod king_danger_squares;

#[cfg(test)]
mod lance_edge_tests {
    use crate::{
        movegen::MoveGenerator,
        shogi::{Color, Piece, PieceType, Position},
        usi,
    };

    #[test]
    fn test_lance_blocker_detection() {
        // Test that lances correctly detect blockers and don't jump over pieces
        let pos = Position::startpos(); // Use starting position which has pawns blocking lances

        // Debug: print position
        println!("Position SFEN: {}", usi::position_to_sfen(&pos));

        // Generate moves
        let gen = MoveGenerator::new();
        let moves = gen.generate_all(&pos).expect("Failed to generate moves");

        // In starting position, lance at 9i (file 0, rank 8) should be blocked by pawn at 9g
        let lance_sq = usi::parse_usi_square("9i").unwrap(); // 9i
        let lance_moves: Vec<_> =
            moves.as_slice().iter().filter(|mv| mv.from() == Some(lance_sq)).collect();

        println!("Lance at 9i generated {} moves", lance_moves.len());
        for mv in &lance_moves {
            println!("  {}", usi::move_to_usi(mv));
        }

        // The lance should not be able to jump over the pawn at 9g
        for mv in &lance_moves {
            let to = mv.to();
            // Lance should not reach beyond 9g (rank 6) due to the pawn
            assert!(
                to.rank() >= 7,
                "Lance at 9i illegally jumped over pawn to reach {}",
                usi::move_to_usi(mv)
            );
        }

        // Specifically check that the illegal move 9i9a+ is not generated
        let has_illegal_move = lance_moves.iter().any(|mv| {
            mv.to().rank() == 0 // 9a
        });
        assert!(!has_illegal_move, "Lance generated illegal move jumping over pawn");
    }

    #[test]
    fn test_white_lance_from_front_rank() {
        // Test white lance on rank 1
        let mut pos = Position::empty();
        pos.side_to_move = Color::White;

        // Place white lance on 1a (file 0, rank 0)
        let white_lance = Piece::new(PieceType::Lance, Color::White);
        pos.board.put_piece(usi::parse_usi_square("9a").unwrap(), white_lance); // 9a (注: 内部file 0 = 9筋)

        // Place kings
        pos.board.put_piece(
            usi::parse_usi_square("5i").unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        pos.board.put_piece(
            usi::parse_usi_square("5a").unwrap(),
            Piece::new(PieceType::King, Color::White),
        );

        // Generate moves
        let gen = MoveGenerator::new();
        let moves = gen.generate_all(&pos).expect("Failed to generate moves");

        // Check no moves from the lance at 1a
        let lance_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|mv| mv.from() == Some(usi::parse_usi_square("1a").unwrap()))
            .collect();

        assert!(
            lance_moves.is_empty(),
            "White lance at 1a should have no moves, but generated: {:?}",
            lance_moves.iter().map(|m| usi::move_to_usi(m)).collect::<Vec<_>>()
        );
    }
}
