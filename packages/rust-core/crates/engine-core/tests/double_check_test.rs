//! Test that only king moves are allowed in double check

use engine_core::{
    movegen::MoveGen,
    shogi::{MoveList, PieceType, Position},
};

#[test]
fn test_double_check_only_king_moves() {
    // Position with black king at 5e in double check from white rook at 5a and white bishop at 1a
    let pos = Position::from_sfen("r3k3b/9/9/9/4K4/9/9/9/9 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // All moves should be king moves
    for i in 0..moves.len() {
        let mv = &moves[i];
        let from = mv.from().expect("Should have from square");
        let piece_on_from = pos.board.piece_on(from).expect("Should have piece");
        assert_eq!(
            piece_on_from.piece_type,
            PieceType::King,
            "In double check, only king moves are allowed"
        );
    }

    // King should have some moves available
    assert!(!moves.is_empty(), "King should have moves to escape double check");
}

#[test]
fn test_double_check_from_sliding_pieces() {
    // Black king at 5i in double check from white rook at 5a and white bishop at 1e
    // This creates a more realistic double check scenario
    let pos = Position::from_sfen("4r4/9/9/9/b8/9/9/9/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Count king moves vs non-king moves
    let mut king_moves = 0;
    let mut non_king_moves = 0;

    for i in 0..moves.len() {
        let mv = &moves[i];
        if let Some(from) = mv.from() {
            let piece = pos.board.piece_on(from).expect("Should have piece");
            if piece.piece_type == PieceType::King {
                king_moves += 1;
            } else {
                non_king_moves += 1;
            }
        } else {
            // Drop move
            non_king_moves += 1;
        }
    }

    assert_eq!(non_king_moves, 0, "No non-king moves should be generated in double check");
    assert!(king_moves > 0, "King should have some escape moves");
}

#[test]
fn test_single_check_allows_block_and_capture() {
    // Black king at 5i in single check from white rook at 5a
    // Black has a gold at 5h that can block
    let pos = Position::from_sfen("4r4/9/9/9/9/9/9/4G4/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Should have both king moves and gold moves (to block)
    let mut has_king_moves = false;
    let mut has_gold_moves = false;

    for i in 0..moves.len() {
        let mv = &moves[i];
        if let Some(from) = mv.from() {
            let piece = pos.board.piece_on(from).expect("Should have piece");
            match piece.piece_type {
                PieceType::King => has_king_moves = true,
                PieceType::Gold => has_gold_moves = true,
                _ => {}
            }
        }
    }

    assert!(has_king_moves, "Should have king moves in single check");
    assert!(has_gold_moves, "Should have gold moves to block in single check");
}

#[test]
fn test_double_check_with_other_pieces() {
    // Black king at 5e in double check from white rook at 5a and white bishop at 1a
    // Black has a gold at 6f - this piece should NOT be able to move
    let pos = Position::from_sfen("r3k3b/9/9/9/4K4/3G5/9/9/9 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Count king moves vs non-king moves
    let mut king_moves = 0;
    let mut non_king_moves = 0;

    for i in 0..moves.len() {
        let mv = &moves[i];
        if let Some(from) = mv.from() {
            let piece = pos.board.piece_on(from).expect("Should have piece");
            if piece.piece_type == PieceType::King {
                king_moves += 1;
            } else {
                non_king_moves += 1;
            }
        } else {
            // Drop move
            non_king_moves += 1;
        }
    }

    assert_eq!(
        non_king_moves, 0,
        "No non-king moves should be generated in double check (including gold at 6f)"
    );
    assert!(king_moves > 0, "King should have some escape moves");
}

#[test]
fn test_double_check_with_many_pieces() {
    // Black king at 5i in double check from white rook at 5a and white bishop at 1e
    // Black has multiple pieces: silver at 6h, gold at 4g, knight at 7g
    // None of these pieces should be able to move
    let pos = Position::from_sfen("4r4/9/9/9/b8/9/3G2N2/5S3/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // All moves should be king moves
    for i in 0..moves.len() {
        let mv = &moves[i];
        if let Some(from) = mv.from() {
            let piece_on_from = pos.board.piece_on(from).expect("Should have piece");
            assert_eq!(
                piece_on_from.piece_type,
                PieceType::King,
                "In double check, only king moves are allowed"
            );
        } else {
            // Drop move - should not happen in double check
            panic!("Drop moves should not be generated in double check");
        }
    }

    // King should have some moves available
    assert!(!moves.is_empty(), "King should have moves to escape double check");
}
