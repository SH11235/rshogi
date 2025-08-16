//! Test for illegal pawn drop checkmate

use engine_core::{
    movegen::MoveGen,
    shogi::{MoveList, PieceType, Position},
};

#[test]
fn test_drop_pawn_mate_no_support_but_king_cannot_capture() {
    // White king at 1a, black rook at 1c and black gold at 2b, black king at 9i
    // Dropping a pawn at 1b would be checkmate even without support
    // because the gold prevents king from capturing
    let pos = Position::from_sfen("k8/1G7/R8/9/9/9/9/9/8K b P 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check that P*1b is not in the move list (illegal drop pawn mate)
    for i in 0..moves.len() {
        let mv = &moves[i];
        if mv.is_drop() {
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            if piece_type == PieceType::Pawn && to.file() == 0 && to.rank() == 1 {
                panic!("P*1b should not be allowed - it's drop pawn mate");
            }
        }
    }
}

#[test]
fn test_drop_pawn_mate_with_support() {
    // White king at 1g (rank 6), black rook at 1a supporting pawn drop, black king at 9a
    // Dropping a pawn at 1h (rank 7) would be checkmate
    let pos = Position::from_sfen("r7K/9/9/9/9/9/k8/9/9 b P 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check that P*1h is not in the move list (illegal drop pawn mate)
    for i in 0..moves.len() {
        let mv = &moves[i];
        if mv.is_drop() {
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            if piece_type == PieceType::Pawn && to.file() == 0 && to.rank() == 7 {
                panic!("P*1h should not be allowed - it's drop pawn mate");
            }
        }
    }
}

#[test]
fn test_drop_pawn_not_mate_king_can_escape() {
    // White king at 5i, no pieces blocking escape, black king at 9a
    // Dropping a pawn at 5h is check but not mate (king can escape)
    let pos = Position::from_sfen("8K/9/9/9/9/9/9/9/4k4 b P 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check that P*5h is in the move list (not mate)
    let mut found_pawn_drop = false;
    for i in 0..moves.len() {
        let mv = &moves[i];
        if mv.is_drop() {
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            if piece_type == PieceType::Pawn && to.file() == 4 && to.rank() == 7 {
                found_pawn_drop = true;
                break;
            }
        }
    }
    assert!(found_pawn_drop, "P*5h should be allowed - king can escape");
}

#[test]
fn test_drop_pawn_not_mate_other_piece_can_capture() {
    // White king at 1i, white gold at 2i, black king at 9a
    // Dropping a pawn at 1h is check but not mate (gold can capture)
    let pos = Position::from_sfen("8K/9/9/9/9/9/9/9/kg7 b P 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check that P*1h is in the move list (not mate)
    let mut found_pawn_drop = false;
    for i in 0..moves.len() {
        let mv = &moves[i];
        if mv.is_drop() {
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            if piece_type == PieceType::Pawn && to.file() == 0 && to.rank() == 7 {
                found_pawn_drop = true;
                break;
            }
        }
    }
    assert!(found_pawn_drop, "P*1h should be allowed - gold can capture");
}

#[test]
fn test_drop_pawn_mate_complex_position() {
    // Complex position where pawn drop creates mate
    // White king at 5a, surrounded by black pieces
    // Black: gold at 4a, silver at 6a, rook at 5c, black king at 5i
    // Dropping pawn at 5b is mate
    let pos = Position::from_sfen("3GkS3/9/4R4/9/9/9/9/9/4K4 b P 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check that P*5b is not in the move list (illegal drop pawn mate)
    for i in 0..moves.len() {
        let mv = &moves[i];
        if mv.is_drop() {
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            if piece_type == PieceType::Pawn && to.file() == 4 && to.rank() == 1 {
                panic!("P*5b should not be allowed - it's drop pawn mate");
            }
        }
    }
}

#[test]
fn test_drop_pawn_not_check() {
    // Position where pawn drop doesn't give check
    // Should always be allowed
    let pos = Position::from_sfen("k8/9/9/9/9/9/9/9/K8 b P 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Count pawn drops - should have many options
    let mut pawn_drop_count = 0;
    for i in 0..moves.len() {
        let mv = &moves[i];
        if mv.is_drop() {
            let piece_type = mv.drop_piece_type();
            if piece_type == PieceType::Pawn {
                pawn_drop_count += 1;
            }
        }
    }
    assert!(pawn_drop_count > 50, "Should have many pawn drop options when not giving check");
}
