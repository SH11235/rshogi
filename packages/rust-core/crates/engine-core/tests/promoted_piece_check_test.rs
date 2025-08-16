//! Tests for promoted pieces giving check as gold
//!
//! Promoted pawns (tokin) and promoted knights/lances/silvers move as gold.
//! This test verifies that check detection works correctly for these pieces.

use engine_core::{
    movegen::MoveGen,
    shogi::{MoveList, Position, Square},
};

#[test]
fn test_promoted_pawn_gives_check_as_gold() {
    // Position with promoted pawn (tokin) giving check as gold
    // White king at 5e, black promoted pawn at 5d
    // Switch to white's turn to test if king is in check
    let pos = Position::from_sfen("9/9/9/9/4k4/4+P4/9/9/9 w - 1").unwrap();
    
    // When in check, only king moves and blocking moves are legal
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // All moves should be king moves (no other pieces can block adjacent check)
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    assert_eq!(king_moves, moves.len(), "All moves should be king moves when in check from adjacent piece");
    assert!(moves.len() > 0 && moves.len() <= 8, "King should have escape moves");
}

#[test]
fn test_promoted_knight_gives_check_as_gold() {
    // Position with promoted knight giving check as gold (diagonal attack)
    // White king at 5e, black promoted knight at 4d
    let pos = Position::from_sfen("9/9/9/9/4k4/3+N5/9/9/9 w - 1").unwrap();
    
    // When in check, only king moves and blocking moves are legal
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // All moves should be king moves (no other pieces can block adjacent check)
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    assert_eq!(king_moves, moves.len(), "All moves should be king moves when in check from adjacent piece");
    assert!(moves.len() > 0 && moves.len() <= 8, "King should have escape moves");
}

#[test]
fn test_promoted_lance_gives_check_as_gold() {
    // Position with promoted lance giving check as gold (horizontal attack)
    // White king at 5e, black promoted lance at 4e
    let pos = Position::from_sfen("9/9/9/9/3+Lk4/9/9/9/9 w - 1").unwrap();
    
    // When in check, only king moves and blocking moves are legal
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // All moves should be king moves (no other pieces can block adjacent check)
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    assert_eq!(king_moves, moves.len(), "All moves should be king moves when in check from adjacent piece");
    assert!(moves.len() > 0 && moves.len() <= 8, "King should have escape moves");
}

#[test]
fn test_promoted_silver_gives_check_as_gold() {
    // Position with promoted silver giving check as gold (backward attack)
    // White king at 5e, black promoted silver at 5f
    let pos = Position::from_sfen("9/9/9/9/4k4/4+S4/9/9/9 w - 1").unwrap();
    
    // When in check, only king moves and blocking moves are legal
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // All moves should be king moves (no other pieces can block adjacent check)
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    assert_eq!(king_moves, moves.len(), "All moves should be king moves when in check from adjacent piece");
    assert!(moves.len() > 0 && moves.len() <= 8, "King should have escape moves");
}

#[test]
fn test_unpromoted_pawn_not_giving_diagonal_check() {
    // Position with unpromoted pawn not giving check diagonally
    // White king at 5e, black unpromoted pawn at 4d, white gold at 3e
    let pos = Position::from_sfen("9/9/9/9/2g1k4/3P5/9/9/9 w - 1").unwrap();
    
    // When NOT in check, should have moves for all pieces
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // Should have moves for both king and gold
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    let gold_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(2, 4)) // Gold at 3e
    }).count();
    
    assert!(king_moves > 0, "King should have moves");
    assert!(gold_moves > 0, "Gold should have moves when not in check");
}

#[test]
fn test_dragon_horse_adjacent_check() {
    // Test dragon (promoted rook) giving adjacent diagonal check
    // White king at 5e, black dragon at 4d
    let pos = Position::from_sfen("9/9/9/9/4k4/3+R5/9/9/9 w - 1").unwrap();
    
    // When in check from adjacent piece, only king can move
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // All moves should be king moves
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    // No drops should be allowed
    let drop_count = moves.iter().filter(|m| m.is_drop()).count();
    
    assert_eq!(king_moves, moves.len(), "All moves should be king moves");
    assert_eq!(drop_count, 0, "No drops should be allowed to block adjacent check");
}

#[test]
fn test_dragon_sliding_check() {
    // Test dragon (promoted rook) giving sliding check
    // White king at 5e, black dragon at 5a
    let pos = Position::from_sfen("4+R4/9/9/9/4k4/9/9/9/8K w p 1").unwrap();
    
    // When in sliding check, can block with drops
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // Should have king moves and drop moves
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    let drop_count = moves.iter().filter(|m| m.is_drop()).count();
    
    assert!(king_moves > 0, "King should have escape moves");
    assert!(drop_count > 0, "Drops should be allowed to block sliding check");
    
    // Verify a drop at 5c blocks the check
    let has_blocking_drop = moves.iter().any(|m| {
        m.is_drop() && m.to() == Square::new(4, 2) // 5c
    });
    assert!(has_blocking_drop, "Should be able to drop at 5c to block check");
}

#[test]
fn test_horse_adjacent_check() {
    // Test horse (promoted bishop) giving adjacent orthogonal check
    // White king at 5e, black horse at 5d
    let pos = Position::from_sfen("9/9/9/9/4k4/4+B4/9/9/9 w - 1").unwrap();
    
    // When in check from adjacent piece, only king can move
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // All moves should be king moves
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    // No drops should be allowed
    let drop_count = moves.iter().filter(|m| m.is_drop()).count();
    
    assert_eq!(king_moves, moves.len(), "All moves should be king moves");
    assert_eq!(drop_count, 0, "No drops should be allowed to block adjacent check");
}

#[test]
fn test_horse_sliding_check() {
    // Test horse (promoted bishop) giving sliding diagonal check
    // White king at 5e, black horse at 1a
    let pos = Position::from_sfen("+B8/9/9/9/4k4/9/9/9/8K w p 1").unwrap();
    
    // When in sliding check, can block with drops
    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);
    
    // Should have king moves and drop moves
    let king_moves = moves.iter().filter(|m| {
        !m.is_drop() && m.from() == Some(Square::new(4, 4)) // King at 5e
    }).count();
    
    let drop_count = moves.iter().filter(|m| m.is_drop()).count();
    
    assert!(king_moves > 0, "King should have escape moves");
    assert!(drop_count > 0, "Drops should be allowed to block sliding check");
    
    // Verify a drop at 3c blocks the check
    let has_blocking_drop = moves.iter().any(|m| {
        m.is_drop() && m.to() == Square::new(2, 2) // 3c
    });
    assert!(has_blocking_drop, "Should be able to drop at 3c to block check");
}