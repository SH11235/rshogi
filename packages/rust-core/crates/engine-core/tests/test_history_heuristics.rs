//! Test history heuristics functionality
//!
//! This is a simple smoke test to ensure the history heuristic code paths
//! are being exercised. Full integration testing happens through the search tests.

use engine_core::search::history::History;
use engine_core::shogi::Move;
use engine_core::usi::parse_usi_square;
use engine_core::{Color, PieceType};

#[test]
fn test_history_tables_basic_functionality() {
    let mut history = History::new();
    let color = Color::Black;

    // Test butterfly history
    let mv = Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);

    // Initial score should be 0
    assert_eq!(history.get_score(color, mv, None), 0);

    // Update with cutoff
    history.update_cutoff(color, mv, 5, None);
    assert!(history.get_score(color, mv, None) > 0);

    // Update with quiet move
    let quiet_mv =
        Move::normal(parse_usi_square("2h").unwrap(), parse_usi_square("2g").unwrap(), false);
    history.update_quiet(color, quiet_mv, 3, None);
    assert!(history.get_score(color, quiet_mv, None) < 0);
}

#[test]
fn test_counter_move_functionality() {
    let mut history = History::new();
    let color = Color::Black;

    let prev_move =
        Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);
    let counter_move =
        Move::normal(parse_usi_square("1d").unwrap(), parse_usi_square("1e").unwrap(), false);

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
    let target = parse_usi_square("5e").unwrap();

    // Initial score should be 0
    assert_eq!(history.capture.get(color, attacker, victim, target), 0);

    // Update with good capture
    history.capture.update_good(color, attacker, victim, target, 4);
    let score = history.capture.get(color, attacker, victim, target);
    assert!(score > 0);

    // Age the scores
    history.age_all();
    assert_eq!(history.capture.get(color, attacker, victim, target), score / 2);
}

#[test]
fn test_history_with_continuation() {
    let mut history = History::new();
    let color = Color::Black;

    // Create moves with piece type information
    let prev_move = Move::normal_with_piece(
        parse_usi_square("7h").unwrap(),
        parse_usi_square("7g").unwrap(),
        false,
        PieceType::Pawn,
        None,
    );
    let curr_move = Move::normal_with_piece(
        parse_usi_square("1d").unwrap(),
        parse_usi_square("1e").unwrap(),
        false,
        PieceType::Pawn,
        None,
    );

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
    let mv = Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);

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
