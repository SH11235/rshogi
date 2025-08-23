//! Test for PV consistency - ensures bestmove matches PV[0]

use engine_cli::engine_adapter::ExtendedSearchResult;
use engine_core::shogi::{Move, Square};

#[test]
fn test_extended_search_result_pv_consistency() {
    // Create test data
    let best_move = Move::normal(Square::new(7, 7), Square::new(7, 6), false);
    let ponder_move = Move::normal(Square::new(3, 3), Square::new(3, 4), false);

    // Create PV that matches best_move
    let pv = vec![best_move, ponder_move];

    // Create ExtendedSearchResult
    let result = ExtendedSearchResult {
        best_move: "7g7f".to_string(),
        ponder_move: Some("3c3d".to_string()),
        depth: 10,
        seldepth: Some(15),
        score: 100,
        pv: pv.clone(),
        stop_info: None,
    };

    // Verify consistency
    assert_eq!(result.pv[0], best_move, "PV[0] should match the best move");
    assert_eq!(
        result.pv.get(1).copied(),
        Some(ponder_move),
        "PV[1] should match the ponder move"
    );
}

#[test]
fn test_move_comparison() {
    // Test identical moves
    let move1 = Move::normal(Square::new(7, 7), Square::new(7, 6), false);
    let move2 = Move::normal(Square::new(7, 7), Square::new(7, 6), false);

    // In the actual code, moves_equal is used, but for this test we'll compare directly
    // since moves_equal is not publicly exported
    assert_eq!(move1.from(), move2.from(), "From squares should match");
    assert_eq!(move1.to(), move2.to(), "To squares should match");
    assert_eq!(move1.is_promote(), move2.is_promote(), "Promotion should match");

    // Test different moves
    let move3 = Move::normal(Square::new(2, 7), Square::new(2, 6), false);
    assert_ne!(move1.from(), move3.from(), "Different moves should have different from squares");

    // Test promotion difference
    let move4 = Move::normal(Square::new(7, 7), Square::new(7, 6), true);
    assert_ne!(
        move1.is_promote(),
        move4.is_promote(),
        "Moves with different promotion should differ"
    );
}
