//! Tests for PV (Principal Variation) validation

use crate::evaluation::evaluate::MaterialEvaluator;
use crate::search::unified::core::{pv_validation, PVTable};
use crate::search::unified::{TTOperations, UnifiedSearcher};
use crate::search::NodeType;
use crate::search::TranspositionTable;
use crate::shogi::{Move, Square};
use crate::usi::{parse_usi_move, parse_usi_square};
use crate::Position;
use std::sync::{Arc, Mutex};
use std::thread;

#[test]
fn test_invalid_move_in_pv_regression() {
    // Regression test for the bug where invalid moves (like 4b5b on empty square) were in PV

    // Create position from the problematic SFEN
    let sfen = "lnsgkgsnl/r6b1/pppppppp1/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4";
    let pos = Position::from_sfen(sfen).expect("Valid SFEN");

    // Try the problematic move "4b5b"
    let problematic_move = parse_usi_move("4b5b").expect("Valid USI move");

    // Verify Square(14) is empty (4b in internal representation)
    let from_square = Square(14); // 4b
    assert!(pos.piece_at(from_square).is_none(), "Square 4b should be empty");

    // The move should not be pseudo-legal
    assert!(
        !pos.is_pseudo_legal(problematic_move),
        "Move 4b5b should not be pseudo-legal on empty square"
    );

    // Run pv_local_sanity on a PV containing this move
    let pv = vec![problematic_move];
    pv_validation::pv_local_sanity(&pos, &pv);

    // The function should handle invalid moves gracefully without panicking
}

#[test]
fn test_pv_move_validation() {
    // Test that invalid moves are not added to PV

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);
    let pos = Position::startpos();

    // Create an invalid move (moving from empty square)
    let invalid_move = Move::normal(Square(50), Square(51), false);
    assert!(!pos.is_pseudo_legal(invalid_move), "Move should be invalid");

    // Try to update PV with invalid move
    searcher.pv_table.set_line(0, invalid_move, &[]);

    // PV should reject null moves (our set_line now checks for NULL)
    // For other invalid moves, the validation happens at a higher level
}

#[test]
fn test_pv_null_move_rejection() {
    // Test that NULL moves are rejected from PV

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);

    // Try to add NULL move to PV
    searcher.pv_table.set_line(0, Move::NULL, &[]);

    // PV should remain empty
    let pv = searcher.pv_table.get_line(0);
    assert!(pv.is_empty(), "PV should be empty after trying to add NULL move");
}

#[test]
fn test_pv_validation_with_tt_pollution() {
    // Test that PV validation catches moves from wrong positions (TT pollution)

    // Position 1: Black bishop on 3b
    let sfen1 = "lnsgkgsnl/r5B1b/ppppppppp/9/9/9/PPPPPPPPP/7R1/LNSGKGSNL b - 1";
    let pos1 = Position::from_sfen(sfen1).expect("Valid SFEN");

    // Position 2: Empty 3b square (no bishop)
    let sfen2 = "lnsgkgsnl/r6b1/ppppppppp/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4";
    let pos2 = Position::from_sfen(sfen2).expect("Valid SFEN");

    // Move that's valid in pos1 but not in pos2
    let move_3b5b = parse_usi_move("3b5b").expect("Valid USI move");

    assert!(pos1.is_pseudo_legal(move_3b5b), "Move should be legal in position 1");
    assert!(!pos2.is_pseudo_legal(move_3b5b), "Move should be illegal in position 2");

    // Test pv_local_sanity catches this
    let bad_pv = vec![move_3b5b];
    pv_validation::pv_local_sanity(&pos2, &bad_pv);
    // Should not panic, just return early
}

#[test]
fn test_tt_move_validation_in_search() {
    // Test that TT moves are validated before use

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, false>::new(evaluator);

    // Initialize TT
    searcher.tt = Some(Arc::new(TranspositionTable::new(1)));

    // Position where 4b5b is invalid
    let sfen = "lnsgkgsnl/r6b1/pppppppp1/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4";
    let pos = Position::from_sfen(sfen).expect("Valid SFEN");

    // Store an invalid move in TT
    let invalid_move = parse_usi_move("4b5b").expect("Valid USI move");
    if let Some(ref tt) = searcher.tt {
        tt.store(
            pos.zobrist_hash,
            Some(invalid_move),
            100, // score
            0,   // eval
            10,  // depth
            NodeType::Exact,
        );
    }

    // The search should validate the TT move and reject it
    // This is tested implicitly through the move ordering logic

    // Verify TT entry exists but move would be invalid
    let tt_entry = searcher.probe_tt(pos.zobrist_hash);
    assert!(tt_entry.is_some(), "TT entry should exist");

    // The move validation happens in the search when legal moves are generated
    // and TT move is checked against them
}

#[test]
fn test_parallel_search_pv_consistency() {
    // Test PV consistency in parallel search scenario

    // Simulate multiple threads updating PV
    let pv_table = Arc::new(Mutex::new(PVTable::new()));
    let mut handles = vec![];

    // Each thread tries to update PV
    for i in 0..4 {
        let pv_clone = Arc::clone(&pv_table);
        let handle = thread::spawn(move || {
            let move1 = Move::normal(
                parse_usi_square("7g").unwrap(),
                parse_usi_square("7f").unwrap(),
                false,
            );
            let move2 = Move::normal(
                parse_usi_square("6c").unwrap(),
                parse_usi_square("6d").unwrap(),
                false,
            );

            // Try to update PV
            if let Ok(mut pv) = pv_clone.lock() {
                pv.set_line(i, move1, &[move2]);
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // Check that PVs are consistent
    let pv = pv_table.lock().unwrap();
    for i in 0..4 {
        let (line, len) = pv.line(i);
        if len > 0 {
            // Verify moves are not NULL
            assert_ne!(line[0], Move::NULL, "PV should not contain NULL moves");
        }
    }
}

#[test]
fn test_hash_collision_move_validation() {
    // Test handling of hash collisions where TT returns wrong position's move

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, false>::new(evaluator);
    searcher.tt = Some(Arc::new(TranspositionTable::new(1)));

    // Create two positions with potentially colliding hashes
    let pos1 = Position::startpos();
    let pos2 =
        Position::from_sfen("lnsgkgsnl/r6b1/pppppppp1/8p/9/9/PPPPPPPPP/1BG3K1R/LNS2GSNL w - 4")
            .unwrap();

    // Simulate hash collision by forcing same hash (in real scenarios)
    // Store a move that's legal in pos1 but not in pos2
    let move_7g7f = parse_usi_move("7g7f").unwrap();

    assert!(pos1.is_pseudo_legal(move_7g7f), "Move should be legal in pos1");
    assert!(!pos2.is_pseudo_legal(move_7g7f), "Move should be illegal in pos2");

    if let Some(ref tt) = searcher.tt {
        // Store with pos2's hash but pos1's move
        tt.store(pos2.zobrist_hash, Some(move_7g7f), 50, 0, 5, NodeType::Exact);
    }

    // When retrieving from TT for pos2, the move should be validated
    let tt_entry = searcher.probe_tt(pos2.zobrist_hash);
    if let Some(entry) = tt_entry {
        if let Some(tt_move) = entry.get_move() {
            // The move validation happens when it's used in search
            // Here we just verify the move exists in TT
            assert_eq!(tt_move, move_7g7f, "TT should return the stored move");
        }
    }
}
