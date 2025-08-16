//! Tests for mandatory promotion (must promote) rules
//!
//! In Shogi, certain pieces must promote when they reach specific ranks:
//! - Pawn and Lance must promote when reaching the back rank
//! - Knight must promote when reaching the back two ranks

use engine_core::{
    movegen::MoveGen,
    shogi::{Move, MoveList, Position},
};

#[test]
fn test_black_pawn_must_promote_to_rank_1() {
    // Black pawn at 8b moving to 8a must promote
    let pos = Position::from_sfen("4k4/1P7/9/9/9/9/9/9/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find move 8b8a
    let pawn_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("8b8a")
        })
        .copied()
        .collect();

    // Should only have promoted move
    assert_eq!(pawn_moves.len(), 1, "Expected 1 move but found {}", pawn_moves.len());
    assert!(pawn_moves[0].is_promote(), "Move should be promoted");
}

#[test]
fn test_black_lance_must_promote_to_rank_1() {
    // Black lance at 8b moving to 8a must promote
    let pos = Position::from_sfen("4k4/1L7/9/9/9/9/9/9/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find move 8b8a
    let lance_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("8b8a")
        })
        .copied()
        .collect();

    // Should only have promoted move
    assert_eq!(lance_moves.len(), 1, "Expected 1 move but found {}", lance_moves.len());
    assert!(lance_moves[0].is_promote(), "Move should be promoted");
}

#[test]
fn test_black_knight_must_promote_to_rank_1() {
    // Black knight at 5c moving to rank 1 must promote
    let pos = Position::from_sfen("4k4/9/4N4/9/9/9/9/9/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find knight moves to rank 1 (7a or 9a)
    let knight_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("5c") && (usi.contains("4a") || usi.contains("6a"))
        })
        .copied()
        .collect();

    // All moves to rank 1 must be promoted
    assert!(!knight_moves.is_empty(), "Knight should have moves to rank 1");
    for mv in knight_moves {
        assert!(mv.is_promote(), "Knight move to rank 1 must be promoted");
    }
}

#[test]
fn test_black_knight_must_promote_to_rank_2() {
    // Black knight at 5d moving to rank 2 must also promote
    let pos = Position::from_sfen("4k4/9/9/4N4/9/9/9/9/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find knight moves to rank 2 (7b or 9b)
    let knight_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("5d") && (usi.contains("4b") || usi.contains("6b"))
        })
        .copied()
        .collect();

    // All moves to rank 2 must be promoted
    assert!(!knight_moves.is_empty(), "Knight should have moves to rank 2");
    for mv in knight_moves {
        assert!(mv.is_promote(), "Knight move to rank 2 must be promoted");
    }
}

#[test]
fn test_white_pawn_must_promote_to_rank_9() {
    // White pawn at 2h moving to 2i must promote
    let pos = Position::from_sfen("4K4/9/9/9/9/9/9/7p1/4k4 w - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find move 2h2i
    let pawn_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("2h2i")
        })
        .copied()
        .collect();

    // Should only have promoted move
    assert_eq!(pawn_moves.len(), 1, "Expected 1 move but found {}", pawn_moves.len());
    assert!(pawn_moves[0].is_promote(), "Move should be promoted");
}

#[test]
fn test_white_lance_must_promote_to_rank_9() {
    // White lance at 2h moving to 2i must promote
    let pos = Position::from_sfen("4K4/9/9/9/9/9/9/7l1/4k4 w - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find move 2h2i
    let lance_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("2h2i")
        })
        .copied()
        .collect();

    // Should only have promoted move
    assert_eq!(lance_moves.len(), 1, "Expected 1 move but found {}", lance_moves.len());
    assert!(lance_moves[0].is_promote(), "Move should be promoted");
}

#[test]
fn test_white_knight_must_promote_to_rank_9() {
    // White knight at 5g moving to rank 9 must promote
    let pos = Position::from_sfen("4K4/9/9/9/9/9/4n4/9/4k4 w - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find knight moves to rank 9 (1i or 3i)
    let knight_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("5g") && (usi.contains("4i") || usi.contains("6i"))
        })
        .copied()
        .collect();

    // All moves to rank 9 must be promoted
    assert!(!knight_moves.is_empty(), "Knight should have moves to rank 9");
    for mv in knight_moves {
        assert!(mv.is_promote(), "Knight move to rank 9 must be promoted");
    }
}

#[test]
fn test_white_knight_must_promote_to_rank_8() {
    // White knight at 5f moving to rank 8 must also promote
    let pos = Position::from_sfen("4K4/9/9/9/9/4n4/9/9/4k4 w - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find knight moves to rank 8 (1h or 3h)
    let knight_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("5f") && (usi.contains("4h") || usi.contains("6h"))
        })
        .copied()
        .collect();

    // All moves to rank 8 must be promoted
    assert!(!knight_moves.is_empty(), "Knight should have moves to rank 8");
    for mv in knight_moves {
        assert!(mv.is_promote(), "Knight move to rank 8 must be promoted");
    }
}

#[test]
fn test_optional_promotion_black_pawn() {
    // Black pawn at 8d moving to 8c (entering promotion zone but not forced)
    let pos = Position::from_sfen("4k4/9/9/1P7/9/9/9/9/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find moves 8d8c
    let pawn_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("8d8c")
        })
        .copied()
        .collect();

    // Should have both promoted and non-promoted moves
    assert_eq!(pawn_moves.len(), 2, "Expected 2 moves but found {}", pawn_moves.len());

    let promoted_count = pawn_moves.iter().filter(|m| m.is_promote()).count();
    let non_promoted_count = pawn_moves.iter().filter(|m| !m.is_promote()).count();

    assert_eq!(promoted_count, 1, "Should have 1 promoted move");
    assert_eq!(non_promoted_count, 1, "Should have 1 non-promoted move");
}

#[test]
fn test_optional_promotion_white_silver() {
    // White silver at 5f (rank 5) moving into promotion zone (ranks 6-8: g,h,i)
    let pos = Position::from_sfen("4K4/9/9/9/9/4s4/9/9/4k4 w - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find silver moves to rank g (entering promotion zone)
    // Silver from 5f can move to: 4g, 5g, 6g
    for dest in ["4g", "5g", "6g"] {
        let moves_to_dest: Vec<Move> = moves
            .iter()
            .filter(|m| {
                let usi = engine_core::usi::move_to_usi(m);
                usi.starts_with("5f") && usi.ends_with(dest)
            })
            .copied()
            .collect();

        // Silver can move diagonally forward and forward
        // Check if this destination is reachable
        if !moves_to_dest.is_empty() {
            assert_eq!(moves_to_dest.len(), 2, "Should have 2 moves to {dest}");
            let promoted_count = moves_to_dest.iter().filter(|m| m.is_promote()).count();
            let non_promoted_count = moves_to_dest.iter().filter(|m| !m.is_promote()).count();
            assert_eq!(promoted_count, 1, "Should have 1 promoted move to {dest}");
            assert_eq!(non_promoted_count, 1, "Should have 1 non-promoted move to {dest}");
        }
    }
}

#[test]
fn test_no_promotion_outside_zone() {
    // Black pawn at 8f moving to 8e (outside promotion zone)
    let pos = Position::from_sfen("4k4/9/9/9/9/1P7/9/9/4K4 b - 1").unwrap();
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Find move 8f8e
    let pawn_moves: Vec<Move> = moves
        .iter()
        .filter(|m| {
            let usi = engine_core::usi::move_to_usi(m);
            usi.starts_with("8f8e")
        })
        .copied()
        .collect();

    // Should only have non-promoted move (not in promotion zone)
    assert_eq!(pawn_moves.len(), 1, "Expected 1 move but found {}", pawn_moves.len());
    assert!(
        !pawn_moves[0].is_promote(),
        "Move should not be promoted outside promotion zone"
    );
}
