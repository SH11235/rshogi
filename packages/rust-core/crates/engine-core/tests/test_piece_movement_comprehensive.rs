//! Comprehensive test suite for piece movement directions and promotion rules
//!
//! This test consolidates and extends the functionality from:
//! - test_lance_movement.rs
//! - test_pawn_move_generation.rs  
//! - test_piece_movements.rs

#[cfg(test)]
mod tests {
    use engine_core::{
        movegen::MoveGen,
        shogi::{Color, MoveList, Piece, PieceType},
        usi::parse_usi_square,
        Position,
    };

    // ========== PAWN TESTS ==========

    #[test]
    fn test_pawn_movement_direction() {
        // Test Black pawn moves towards rank 0 (up)
        let sfen = "9/9/9/9/9/9/2P6/9/K8 b - 1";
        let mut pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7g")
            .collect();

        assert_eq!(pawn_moves.len(), 1, "Black pawn should have 1 move");
        assert_eq!(
            pawn_moves[0].to().to_string(),
            "7f",
            "Black pawn should move from 7g to 7f (up)"
        );

        // Test White pawn moves towards rank 8 (down)
        let sfen = "k8/9/2p6/9/9/9/9/9/9 w - 1";
        pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7c")
            .collect();

        assert_eq!(pawn_moves.len(), 1, "White pawn should have 1 move");
        assert_eq!(
            pawn_moves[0].to().to_string(),
            "7d",
            "White pawn should move from 7c to 7d (down)"
        );
    }

    #[test]
    fn test_pawn_initial_position_moves() {
        // Test from the original test_pawn_move_generation.rs
        let pos = Position::startpos();
        let mut movegen = MoveGen::new();
        let mut moves = MoveList::new();
        movegen.generate_all(&pos, &mut moves);

        // Check for specific pawn moves from initial position
        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().map(|sq| sq.rank() == 6).unwrap_or(false))
            .map(|m| format!("{}{}", m.from().unwrap(), m.to()))
            .collect();

        assert!(pawn_moves.contains(&"8g8f".to_string()), "Move 8g8f should be available");
        assert!(pawn_moves.contains(&"7g7f".to_string()), "Move 7g7f should be available");
        assert!(pawn_moves.contains(&"2g2f".to_string()), "Move 2g2f should be available");
    }

    // ========== LANCE TESTS ==========

    #[test]
    fn test_lance_movement_direction() {
        // Test Black lance moves towards rank 0 (up)
        let sfen = "9/9/9/9/9/9/2L6/9/K8 b - 1";
        let mut pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let lance_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7g")
            .map(|m| m.to().to_string())
            .collect();

        assert!(
            lance_moves.contains(&"7f".to_string()),
            "Black lance should be able to move to 7f"
        );
        assert!(
            lance_moves.contains(&"7e".to_string()),
            "Black lance should be able to move to 7e"
        );
        assert!(
            lance_moves.contains(&"7d".to_string()),
            "Black lance should be able to move to 7d"
        );
        assert!(
            lance_moves.iter().all(|mv| {
                let rank = mv.chars().nth(1).unwrap();
                rank < 'g'
            }),
            "Black lance should only move up (to smaller ranks)"
        );

        // Test White lance moves towards rank 8 (down)
        let sfen = "k8/9/2l6/9/9/9/9/9/9 w - 1";
        pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let lance_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7c")
            .map(|m| m.to().to_string())
            .collect();

        assert!(
            lance_moves.contains(&"7d".to_string()),
            "White lance should be able to move to 7d"
        );
        assert!(
            lance_moves.contains(&"7e".to_string()),
            "White lance should be able to move to 7e"
        );
        assert!(
            lance_moves.iter().all(|mv| {
                let rank = mv.chars().nth(1).unwrap();
                rank > 'c'
            }),
            "White lance should only move down (to larger ranks)"
        );
    }

    #[test]
    fn test_lance_detailed_movement() {
        // Detailed test from the original test_lance_movement.rs
        let mut pos = Position::empty();

        // Place Black Lance at rank 7, file 4
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Lance, Color::Black));
        // Place kings
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        pos.side_to_move = Color::Black;

        let mut movegen = MoveGen::new();
        let mut moves = MoveList::new();
        movegen.generate_all(&pos, &mut moves);

        let lance_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5h").unwrap()))
            .collect();

        // Black Lance should be able to move to ranks 6, 5, 4, 3, 2, 1
        let valid_targets = vec![
            parse_usi_square("5g").unwrap(),
            parse_usi_square("5f").unwrap(),
            parse_usi_square("5e").unwrap(),
            parse_usi_square("5d").unwrap(),
            parse_usi_square("5c").unwrap(),
            parse_usi_square("5b").unwrap(),
        ];

        for target in valid_targets {
            assert!(
                lance_moves.iter().any(|m| m.to() == target),
                "Black Lance at 4,7 should be able to move to {target:?}"
            );
        }

        // Should NOT be able to move backward
        assert!(
            !lance_moves.iter().any(|m| m.to() == parse_usi_square("5i").unwrap()),
            "Black Lance should NOT be able to move backward to rank 8"
        );
    }

    #[test]
    fn test_lance_check_mechanics() {
        // Test from original that Lance attack checks are consistent with movement
        let mut pos = Position::empty();

        // Place Black king at rank 4
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
        // Place White Lance at rank 6 (below the Black king)
        pos.board
            .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Lance, Color::White));
        // Place White king somewhere
        pos.board
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

        pos.side_to_move = Color::Black;

        let mut movegen = MoveGen::new();
        let mut moves = MoveList::new();
        movegen.generate_all(&pos, &mut moves);

        // Black should be in check from the White Lance
        let king_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5e").unwrap()))
            .collect();

        assert!(
            !king_moves.is_empty(),
            "Black king should have escape moves when in check from White Lance"
        );
    }

    // ========== KNIGHT TESTS ==========

    #[test]
    fn test_knight_movement_direction() {
        // Test Black knight moves towards rank 0 (up)
        let sfen = "9/9/9/9/2N6/9/9/9/K8 b - 1";
        let mut pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let knight_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7e")
            .map(|m| m.to().to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        assert_eq!(knight_moves.len(), 2, "Black knight should have 2 moves");
        assert!(
            knight_moves.contains(&"6c".to_string()) || knight_moves.contains(&"8c".to_string()),
            "Black knight should jump to rank c (2 ranks up)"
        );

        // Test White knight moves towards rank 8 (down)
        let sfen = "k8/9/9/2n6/9/9/9/9/9 w - 1";
        pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let knight_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7d")
            .map(|m| m.to().to_string())
            .collect();

        assert_eq!(knight_moves.len(), 2, "White knight should have 2 moves");
        assert!(
            knight_moves.contains(&"6f".to_string()) || knight_moves.contains(&"8f".to_string()),
            "White knight should jump to rank f (2 ranks down)"
        );
    }

    // ========== PROMOTION TESTS ==========

    #[test]
    fn test_promotion_zones() {
        // Test Black promotion zone (ranks 0, 1, 2)
        let sfen = "9/9/2P6/9/9/9/9/9/K8 b - 1";
        let mut pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7c")
            .collect();

        // Moving to rank 1 (7b) should allow promotion
        assert!(
            pawn_moves.iter().any(|m| m.to().to_string() == "7b" && m.is_promote()),
            "Black pawn should be able to promote when moving to rank 1"
        );
        assert!(
            pawn_moves.iter().any(|m| m.to().to_string() == "7b" && !m.is_promote()),
            "Black pawn should also have option not to promote"
        );

        // Test White promotion zone (ranks 6, 7, 8)
        let sfen = "k8/9/9/9/9/9/2p6/9/9 w - 1";
        pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7g")
            .collect();

        // Moving to rank 7 (7h) should allow promotion
        assert!(
            pawn_moves.iter().any(|m| m.to().to_string() == "7h" && m.is_promote()),
            "White pawn should be able to promote when moving to rank 7"
        );
        assert!(
            pawn_moves.iter().any(|m| m.to().to_string() == "7h" && !m.is_promote()),
            "White pawn should also have option not to promote"
        );
    }

    #[test]
    fn test_must_promote_positions() {
        // Test Black pawn must promote at rank 0
        let sfen = "9/2P6/9/9/9/9/9/9/K8 b - 1";
        let mut pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7b")
            .collect();

        assert_eq!(pawn_moves.len(), 1, "Black pawn at rank 1 must promote");
        assert!(pawn_moves[0].is_promote(), "Black pawn must promote when moving to rank 0");

        // Test White pawn must promote at rank 8
        let sfen = "k8/9/9/9/9/9/9/2p6/9 w - 1";
        pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7h")
            .collect();

        assert_eq!(pawn_moves.len(), 1, "White pawn at rank 7 must promote");
        assert!(pawn_moves[0].is_promote(), "White pawn must promote when moving to rank 8");
    }

    #[test]
    fn test_lance_must_promote() {
        // Test Black lance must promote at rank 0
        let sfen = "9/2L6/9/9/9/9/9/9/K8 b - 1";
        let mut pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let lance_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| {
                !m.is_drop() && m.from().unwrap().to_string() == "7b" && m.to().to_string() == "7a"
            })
            .collect();

        assert_eq!(lance_moves.len(), 1, "Black lance must promote when moving to rank 0");
        assert!(lance_moves[0].is_promote(), "Black lance must promote at rank 0");

        // Test White lance must promote at rank 8
        let sfen = "k8/9/9/9/9/9/9/2l6/9 w - 1";
        pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let lance_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| {
                !m.is_drop() && m.from().unwrap().to_string() == "7h" && m.to().to_string() == "7i"
            })
            .collect();

        assert_eq!(lance_moves.len(), 1, "White lance must promote when moving to rank 8");
        assert!(lance_moves[0].is_promote(), "White lance must promote at rank 8");
    }

    #[test]
    fn test_knight_must_promote() {
        // Test Black knight must promote at ranks 0 and 1
        let sfen = "9/9/2N6/9/9/9/9/9/K8 b - 1";
        let mut pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let knight_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7c")
            .collect();

        // All moves to rank 0 (7a) must be promotions
        let moves_to_rank_0: Vec<_> = knight_moves.iter().filter(|m| m.to().rank() == 0).collect();

        for mv in moves_to_rank_0 {
            assert!(mv.is_promote(), "Black knight must promote when moving to rank 0");
        }

        // Test White knight must promote at ranks 7 and 8
        let sfen = "k8/9/9/9/9/9/2n6/9/9 w - 1";
        pos = Position::from_sfen(sfen).unwrap();
        let mut moves = MoveList::new();
        MoveGen::new().generate_all(&pos, &mut moves);

        let knight_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from().unwrap().to_string() == "7g")
            .collect();

        // All moves to rank 8 (7i) must be promotions
        let moves_to_rank_8: Vec<_> = knight_moves.iter().filter(|m| m.to().rank() == 8).collect();

        for mv in moves_to_rank_8 {
            assert!(mv.is_promote(), "White knight must promote when moving to rank 8");
        }
    }
}
