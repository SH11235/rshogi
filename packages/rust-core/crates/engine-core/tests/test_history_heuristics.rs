//! Test history heuristics functionality
//!
//! This is a simple smoke test to ensure the history heuristic code paths
//! are being exercised. Full integration testing happens through the search tests.

use engine_core::search::history::History;
use engine_core::shogi::{Move, Square};
use engine_core::{Color, PieceType};

#[test]
fn test_history_tables_basic_functionality() {
    let mut history = History::new();
    let color = Color::Black;

    // Test butterfly history
    let mv = Move::normal(Square::new(2, 7), Square::new(2, 6), false);

    // Initial score should be 0
    assert_eq!(history.get_score(color, mv, None), 0);

    // Update with cutoff
    history.update_cutoff(color, mv, 5, None);
    assert!(history.get_score(color, mv, None) > 0);

    // Update with quiet move
    let quiet_mv = Move::normal(Square::new(7, 7), Square::new(7, 6), false);
    history.update_quiet(color, quiet_mv, 3, None);
    assert!(history.get_score(color, quiet_mv, None) < 0);
}

#[test]
fn test_counter_move_functionality() {
    let mut history = History::new();
    let color = Color::Black;

    let prev_move = Move::normal(Square::new(2, 7), Square::new(2, 6), false);
    let counter_move = Move::normal(Square::new(8, 3), Square::new(8, 4), false);

    // Initially no counter move
    assert!(history.counter_moves.get(color, prev_move).is_none());

    // Update counter move
    history.counter_moves.update(color, prev_move, counter_move);
    assert_eq!(history.counter_moves.get(color, prev_move), Some(counter_move));
}

#[test]
fn test_capture_history_functionality() {
    let mut history = History::new();
    let color = Color::Black;
    let attacker = PieceType::Knight;
    let victim = PieceType::Silver;

    // Initial score should be 0
    assert_eq!(history.capture.get(color, attacker, victim), 0);

    // Update with good capture
    history.capture.update_good(color, attacker, victim, 4);
    let score = history.capture.get(color, attacker, victim);
    assert!(score > 0);

    // Age the scores
    history.age_all();
    assert_eq!(history.capture.get(color, attacker, victim), score / 2);
}

#[test]
fn test_history_with_continuation() {
    let mut history = History::new();
    let color = Color::Black;

    // Create moves with piece type information
    let prev_move =
        Move::normal_with_piece(Square::new(2, 7), Square::new(2, 6), false, PieceType::Pawn, None);
    let curr_move =
        Move::normal_with_piece(Square::new(8, 3), Square::new(8, 4), false, PieceType::Pawn, None);

    // Get score with continuation history
    let score1 = history.get_score(color, curr_move, Some(prev_move));
    assert_eq!(score1, 0); // Should be 0 initially

    // Update with cutoff
    history.update_cutoff(color, curr_move, 5, Some(prev_move));

    // Score should now be positive (butterfly + continuation)
    let score2 = history.get_score(color, curr_move, Some(prev_move));
    assert!(score2 > 0);

    // Score without prev_move should be less (only butterfly)
    let score3 = history.get_score(color, curr_move, None);
    assert!(score3 > 0);
    assert!(score3 < score2); // Continuation history adds to the score
}

#[test]
fn test_history_aging() {
    let mut history = History::new();
    let color = Color::Black;
    let mv = Move::normal(Square::new(2, 7), Square::new(2, 6), false);

    // Build up history score
    for _ in 0..5 {
        history.update_cutoff(color, mv, 10, None);
    }

    let score_before = history.get_score(color, mv, None);
    assert!(score_before > 0);

    // Age all scores
    history.age_all();

    let score_after = history.get_score(color, mv, None);
    assert_eq!(score_after, score_before / 2);
}
