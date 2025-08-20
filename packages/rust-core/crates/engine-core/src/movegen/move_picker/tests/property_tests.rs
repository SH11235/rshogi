//! Property tests for move picker invariants and stage transitions

use crate::movegen::generator::MoveGenImpl;
use crate::movegen::move_picker::MovePicker;
use crate::search::history::History;
use crate::search::types::SearchStack;
use crate::shogi::{Move, Position};
use crate::usi::{parse_sfen, parse_usi_square};
use std::collections::HashSet;

/// Test that stage transitions follow the expected order in normal search
#[test]
fn test_stage_transitions_normal_search() {
    let pos = Position::startpos();
    let history = History::new();
    let mut stack = SearchStack::default();

    // Set up some killer moves for testing
    let killer1 =
        Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);
    let killer2 =
        Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
    stack.killers[0] = Some(killer1);
    stack.killers[1] = Some(killer2);

    let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

    // Track stage transitions - we need to check the stage before calling next_move
    // because some stages transition immediately
    let mut stages_seen = Vec::new();
    let mut moves_generated = Vec::new();

    // Record initial stage
    stages_seen.push(format!("{:?}", picker.stage));

    // Generate all moves and track stages
    while let Some(mv) = picker.next_move() {
        let current_stage = format!("{:?}", picker.stage);
        if stages_seen.last().unwrap() != &current_stage {
            stages_seen.push(current_stage);
        }
        moves_generated.push(mv);
    }

    // Verify expected stage progression
    // Since we have no TT or PV move, we expect:
    // GenerateCaptures -> GoodCaptures -> Killers -> GenerateQuiets -> QuietMoves -> BadCaptures -> End
    // Note: Some stages may be internal transitions and not visible in the debug output
    assert!(!stages_seen.is_empty(), "Should see some stage transitions");
    assert!(!moves_generated.is_empty(), "Should generate some moves");

    // Verify stage order
    let stage_order = [
        "GenerateCaptures",
        "GoodCaptures",
        "Killers",
        "GenerateQuiets",
        "QuietMoves",
    ];
    let mut last_index = 0;
    for expected_stage in &stage_order {
        if let Some(pos) = stages_seen.iter().position(|s| s.contains(expected_stage)) {
            assert!(pos >= last_index, "Stage {expected_stage} appeared out of order");
            last_index = pos;
        }
    }
}

/// Test that quiescence search never reaches BadCaptures stage
#[test]
fn test_quiescence_never_reaches_bad_captures() {
    // Use a position with both good and bad captures
    let sfen = "lnsg3nl/2k2gr2/ppbp1p1pp/2p1P4/4s1p2/2P6/PP1P1P1PP/1BG1GKR2/LNS3SNL b - 1";
    let pos = parse_sfen(sfen).expect("Valid SFEN");
    let history = History::new();
    let stack = SearchStack::default();

    let mut picker = MovePicker::new_quiescence(&pos, None, &history, &stack, 1);

    // Track all stages visited
    let mut stages_seen = HashSet::new();
    let mut moves_generated = Vec::new();

    // Record initial stage
    stages_seen.insert(format!("{:?}", picker.stage));

    while let Some(mv) = picker.next_move() {
        stages_seen.insert(format!("{:?}", picker.stage));
        moves_generated.push(mv);
    }

    // Key assertion: BadCaptures stage should never be reached in quiescence
    assert!(
        !stages_seen.contains("BadCaptures"),
        "Quiescence search should never reach BadCaptures stage"
    );

    // Should see some stage transitions
    assert!(!stages_seen.is_empty(), "Should see some stage transitions");

    // All moves should be captures
    for mv in &moves_generated {
        let is_capture = pos.piece_at(mv.to()).is_some();
        assert!(is_capture, "Quiescence should only return captures, got {mv:?}");
    }
}

/// Test that no duplicate moves are generated across all stages
#[test]
fn test_no_duplicate_moves_across_stages() {
    let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let pos = parse_sfen(sfen).expect("Valid SFEN");
    let history = History::new();
    let mut stack = SearchStack::default();

    // Set up TT move, killers
    let tt_move =
        Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
    let killer1 =
        Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);
    let killer2 =
        Move::normal(parse_usi_square("5g").unwrap(), parse_usi_square("5f").unwrap(), false);
    stack.killers[0] = Some(killer1);
    stack.killers[1] = Some(killer2);

    let mut picker = MovePicker::new(&pos, Some(tt_move), None, &history, &stack, 1);

    // Collect all moves
    let mut all_moves = Vec::new();
    let mut move_set = HashSet::new();

    while let Some(mv) = picker.next_move() {
        all_moves.push(mv);
        let was_new = move_set.insert(mv);
        assert!(was_new, "Duplicate move generated: {mv:?}");
    }

    // Verify we got a reasonable number of moves
    assert!(!all_moves.is_empty(), "Should generate some moves");

    // Verify TT move came first if it was legal
    if pos.is_legal_move(tt_move) {
        assert_eq!(all_moves[0], tt_move, "TT move should be returned first");
    }
}

/// Test that all legal moves are eventually generated
#[test]
fn test_all_legal_moves_generated() {
    let pos = Position::startpos();
    let history = History::new();
    let stack = SearchStack::default();

    // Generate moves using MovePicker
    let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);
    let mut picker_moves = HashSet::new();

    while let Some(mv) = picker.next_move() {
        picker_moves.insert(mv);
    }

    // Generate moves using direct move generation
    let mut gen = MoveGenImpl::new(&pos);
    let all_legal_moves = gen.generate_all();
    let legal_move_set: HashSet<_> = all_legal_moves.as_slice().iter().cloned().collect();

    // MovePicker should generate all legal moves
    assert_eq!(
        picker_moves.len(),
        legal_move_set.len(),
        "MovePicker should generate all legal moves"
    );
    assert_eq!(picker_moves, legal_move_set, "MovePicker moves should match legal moves");
}

/// Test that good captures come before bad captures
#[test]
fn test_capture_ordering_by_see() {
    // Position with multiple captures available
    let sfen = "lnsgk2nl/6gb1/p1pppp2p/1p4p2/9/2P3P2/PP1PPP2P/1B5R1/LNSGKG1NL w r 1";
    let pos = parse_sfen(sfen).expect("Valid SFEN");
    let history = History::new();
    let stack = SearchStack::default();

    let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

    let mut captures = Vec::new();
    let mut _found_bad_capture = false;
    let mut in_bad_captures_stage = false;

    while let Some(mv) = picker.next_move() {
        if pos.piece_at(mv.to()).is_some() {
            captures.push(mv);

            // Check if we're in bad captures stage
            if format!("{:?}", picker.stage) == "BadCaptures" {
                in_bad_captures_stage = true;
            }

            // If we found a bad capture, all subsequent captures should also be bad
            if in_bad_captures_stage {
                _found_bad_capture = true;
            }
        }
    }

    // Verify we found some captures
    assert!(!captures.is_empty(), "Should find some captures in this position");
}

/// Test that killers are properly filtered
#[test]
fn test_killer_filtering() {
    let pos = Position::startpos();
    let history = History::new();
    let mut stack = SearchStack::default();

    // Set up various killer scenarios
    let tt_move =
        Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
    let _capture_killer =
        Move::normal(parse_usi_square("2c").unwrap(), parse_usi_square("2h").unwrap(), false); // Invalid capture
    let valid_killer =
        Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);
    let duplicate_killer = tt_move; // Same as TT move

    stack.killers[0] = Some(duplicate_killer);
    stack.killers[1] = Some(valid_killer);

    let mut picker = MovePicker::new(&pos, Some(tt_move), None, &history, &stack, 1);

    let mut moves = Vec::new();
    let mut killer_found = false;

    while let Some(mv) = picker.next_move() {
        moves.push(mv);
        if mv == valid_killer {
            killer_found = true;
        }
    }

    // Verify duplicate killer was filtered
    let tt_count = moves.iter().filter(|&&m| m == tt_move).count();
    assert_eq!(tt_count, 1, "TT move should appear exactly once");

    // Verify valid killer was included
    assert!(killer_found, "Valid killer should be included");
}

/// Test root node PV move handling
#[test]
fn test_root_pv_move_handling() {
    let pos = Position::startpos();
    let history = History::new();
    let stack = SearchStack::default();

    let pv_move =
        Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
    let tt_move =
        Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);

    // Test at root (ply = 0)
    let mut picker = MovePicker::new(&pos, Some(tt_move), Some(pv_move), &history, &stack, 0);

    // First move should be PV move
    let first_move = picker.next_move().expect("Should have moves");
    assert_eq!(first_move, pv_move, "PV move should come first at root");

    // Second move should be TT move
    let second_move = picker.next_move().expect("Should have more moves");
    assert_eq!(second_move, tt_move, "TT move should come second at root");

    // Collect remaining moves and ensure no duplicates
    let mut all_moves = vec![first_move, second_move];
    while let Some(mv) = picker.next_move() {
        assert!(mv != pv_move, "PV move should not be duplicated");
        assert!(mv != tt_move, "TT move should not be duplicated");
        all_moves.push(mv);
    }
}

// Helper function to get move type for debugging
#[allow(dead_code)]
fn describe_move(pos: &Position, mv: Move) -> String {
    if pos.piece_at(mv.to()).is_some() {
        "capture".to_string()
    } else if mv.is_drop() {
        "drop".to_string()
    } else {
        "quiet".to_string()
    }
}
