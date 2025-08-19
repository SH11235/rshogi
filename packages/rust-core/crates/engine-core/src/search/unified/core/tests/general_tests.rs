//! General tests for unified searcher core

use crate::evaluation::evaluate::MaterialEvaluator;
use crate::search::unified::core::null_move;
use crate::search::unified::UnifiedSearcher;
use crate::Position;

#[test]
fn test_stop_flag_polling_interval() {
    // Test that stop flag checks are done at appropriate intervals
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false, 0>::new(evaluator);

    // Simulate node counting and verify polling frequency
    let mut check_count = 0;
    for i in 0..100000 {
        searcher.stats.nodes = i;
        // Simulate periodic checks (this test uses 0x3FF = every 1024 nodes)
        // Note: Actual implementation uses adaptive intervals via get_event_poll_mask()
        if searcher.stats.nodes & 0x3FF == 0 {
            check_count += 1;
        }
    }

    // Should check approximately 97 times (100000 / 1024)
    assert!((95..=100).contains(&check_count), "Check count: {check_count}");
}

#[test]
fn test_has_non_pawn_material() {
    let pos = Position::startpos();

    // Starting position has non-pawn material (checks current side to move)
    assert!(null_move::has_non_pawn_material(&pos)); // Black's turn at start

    // Test endgame position with only pawns and kings
    // Using startpos but capturing all pieces except pawns and kings
    // This is a more realistic test scenario

    // First, test a position where black has only pawns (plus king)
    // SFEN notation: k8/9/ppp6/9/9/9/6PPP/9/K8 b - 1
    let black_pawn_only = Position::from_sfen("k8/9/ppp6/9/9/9/6PPP/9/K8 b - 1").unwrap();
    assert!(
        !null_move::has_non_pawn_material(&black_pawn_only),
        "Black should have no non-pawn material"
    );

    // Test a position where white has only pawns (switch turn)
    let white_pawn_only = Position::from_sfen("k8/9/ppp6/9/9/9/6PPP/9/K8 w - 1").unwrap();
    assert!(
        !null_move::has_non_pawn_material(&white_pawn_only),
        "White should have no non-pawn material"
    );

    // Test position with one non-pawn piece (silver for black)
    // SFEN: k8/9/ppp6/9/9/9/6PPP/S8/K8 b - 1
    let mixed_pos = Position::from_sfen("k8/9/ppp6/9/9/9/6PPP/S8/K8 b - 1").unwrap();
    assert!(
        null_move::has_non_pawn_material(&mixed_pos),
        "Black has a silver, so has non-pawn material"
    );

    // Test position with non-pawn piece in hand
    let with_hand = Position::from_sfen("k8/9/ppp6/9/9/9/6PPP/9/K8 b S 1").unwrap();
    assert!(null_move::has_non_pawn_material(&with_hand), "Black has a silver in hand");

    // Test position with promoted pawn (tokin) - should count as non-pawn material
    // SFEN: k8/9/+P8/9/9/9/9/9/K8 b - 1 (black has a promoted pawn - uppercase P for black)
    let with_tokin = Position::from_sfen("k8/9/+P8/9/9/9/9/9/K8 b - 1").unwrap();
    assert!(
        null_move::has_non_pawn_material(&with_tokin),
        "Black has a tokin (promoted pawn), which is equivalent to gold"
    );

    // Test position with promoted pawn for white
    let white_tokin = Position::from_sfen("k8/9/9/9/9/9/+p8/9/K8 w - 1").unwrap();
    assert!(
        null_move::has_non_pawn_material(&white_tokin),
        "White has a tokin (promoted pawn), which is equivalent to gold"
    );
}

#[test]
fn test_reduced_depth_calculation() {
    // Test that reduced depth calculation handles edge cases properly
    let depth: u8 = 3;
    let reduction: u8 = 2;

    // saturating_sub ensures we don't underflow
    let reduced_depth = depth.saturating_sub(1 + reduction);
    assert_eq!(reduced_depth, 0); // Should transition to quiescence search

    // Normal case
    let depth: u8 = 6;
    let reduction: u8 = 1;
    let reduced_depth = depth.saturating_sub(1 + reduction);
    assert_eq!(reduced_depth, 4);
}
