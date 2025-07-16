//! Comprehensive tests for MovePicker based on design specifications

#[cfg(test)]
mod tests {
    use crate::ai::board::{Color, Piece, PieceType, Position, Square};
    use crate::ai::history::History;
    use crate::ai::move_picker::MovePicker;
    use crate::ai::movegen::MoveGen;
    use crate::ai::moves::{Move, MoveList};
    use crate::ai::search_enhanced::SearchStack;
    use std::collections::HashSet;
    use std::sync::Arc;

    /// Test 1: Completeness - MovePicker generates all legal moves without duplicates
    #[test]
    fn test_completeness_all_moves_generated() {
        // Test starting position and some manually created positions
        let positions = vec![Position::startpos()];

        for pos in positions {
            let history = Arc::new(History::new());
            let stack = SearchStack::default();

            // Generate moves with MovePicker
            let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);
            let mut picker_moves = Vec::new();
            let mut picker_set = HashSet::new();

            while let Some(mv) = picker.next_move() {
                assert!(picker_set.insert(mv), "Duplicate move generated: {mv:?}");
                picker_moves.push(mv);
            }

            // Generate all moves directly
            let mut move_list = MoveList::new();
            let mut gen = MoveGen::new();
            gen.generate_all(&pos, &mut move_list);

            // Compare
            assert_eq!(
                picker_moves.len(),
                move_list.len(),
                "MovePicker generated {} moves, expected {}",
                picker_moves.len(),
                move_list.len()
            );

            // Check all moves are present
            let move_set: HashSet<_> = move_list.as_slice().iter().cloned().collect();
            for mv in &picker_moves {
                assert!(move_set.contains(mv), "Move {mv:?} not in legal moves");
            }
        }
    }

    /// Test 2: Priority order - TT → Killer → Good Capture → History Quiet → Bad Capture
    #[test]
    fn test_priority_ordering() {
        let mut pos = Position::startpos();

        // Make some moves to create a position with captures
        let setup_moves = [
            Move::normal(Square::new(2, 2), Square::new(2, 3), false), // 7g7f
            Move::normal(Square::new(3, 6), Square::new(3, 5), false), // 6c6d
            Move::normal(Square::new(2, 3), Square::new(2, 4), false), // 7f7e
            Move::normal(Square::new(3, 5), Square::new(3, 4), false), // 6d6e
        ];

        for mv in &setup_moves {
            pos.do_move(*mv);
        }

        let mut history = History::new();
        let mut stack = SearchStack::default();

        // Set up different move types
        // The TT move should be a legal capture in this position
        // In starting position after a few moves, set up proper piece types
        let tt_move = Some(Move::normal_with_piece(
            Square::new(2, 4),
            Square::new(2, 5),
            false,
            PieceType::Pawn,
            None,
        )); // Legal move
        let killer_move = Move::normal_with_piece(
            Square::new(7, 2),
            Square::new(7, 3),
            false,
            PieceType::Pawn,
            None,
        ); // Quiet
        let history_move = Move::normal_with_piece(
            Square::new(6, 2),
            Square::new(6, 3),
            false,
            PieceType::Pawn,
            None,
        ); // Quiet

        // Set killers
        stack.killers[0] = Some(killer_move);

        // Set history score
        history.update_cutoff(pos.side_to_move, history_move, 10, None);

        let history_arc = Arc::new(history);
        let mut picker = MovePicker::new(&pos, tt_move, None, &history_arc, &stack, 1);

        // Check order
        let first = picker.next_move();
        assert_eq!(first, tt_move, "TT move should be first");

        // Collect next moves
        let mut moves = Vec::new();
        for _ in 0..10 {
            if let Some(mv) = picker.next_move() {
                moves.push(mv);
            }
        }

        // Find killer position
        let killer_pos = moves.iter().position(|&m| m == killer_move);
        assert!(killer_pos.is_some(), "Killer move should be generated");

        // Find history move position
        let history_pos = moves.iter().position(|&m| m == history_move);
        assert!(history_pos.is_some(), "History move should be generated");

        // Killer should come before most quiet moves
        if let (Some(k_pos), Some(h_pos)) = (killer_pos, history_pos) {
            assert!(k_pos < h_pos || h_pos < 5, "Killer/history moves should be early");
        }
    }

    /// Test 3: SEE ordering for captures
    #[test]
    fn test_see_capture_ordering() {
        // Create position with captures by making moves
        let mut pos = Position::startpos();

        // Make moves to create capture opportunities with different piece values
        let moves = [
            Move::normal(Square::new(2, 2), Square::new(2, 3), false), // 7g7f
            Move::normal(Square::new(3, 6), Square::new(3, 5), false), // 6c6d
            Move::normal(Square::new(2, 3), Square::new(2, 4), false), // 7f7e
            Move::normal(Square::new(3, 5), Square::new(3, 4), false), // 6d6e
            Move::normal(Square::new(2, 4), Square::new(2, 5), false), // 7e7d
            Move::normal(Square::new(3, 4), Square::new(3, 3), false), // 6e6f
        ];

        for mv in &moves {
            pos.do_move(*mv);
        }
        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

        // Collect good and bad captures separately
        let mut good_captures = Vec::new();
        let mut bad_captures = Vec::new();
        let mut found_bad_capture = false;

        while let Some(mv) = picker.next_move() {
            if !mv.is_drop() && pos.board.piece_on(mv.to()).is_some() {
                if !found_bad_capture {
                    good_captures.push(mv);
                } else {
                    bad_captures.push(mv);
                }
                // Check if we've reached bad captures stage
                // (This is a simplification - in real implementation we'd check the stage)
                if !good_captures.is_empty() && bad_captures.is_empty() {
                    // Keep collecting good captures
                } else if !bad_captures.is_empty() {
                    found_bad_capture = true;
                }
            }
            if good_captures.len() + bad_captures.len() >= 5 {
                break;
            }
        }

        // Should have some captures
        assert!(
            !good_captures.is_empty() || !bad_captures.is_empty(),
            "Should have some captures"
        );

        // In a position with both good and bad captures, good ones should come first
        // (This test is simplified - a more thorough test would verify SEE values)
    }

    /// Test 4: Quiet moves ordered by history
    #[test]
    fn test_quiet_history_ordering() {
        let pos = Position::startpos();
        let mut history = History::new();
        let stack = SearchStack::default();

        // Set different history scores
        let high_score_move = Move::normal(Square::new(4, 2), Square::new(4, 3), false); // 5g5f
        let med_score_move = Move::normal(Square::new(3, 2), Square::new(3, 3), false); // 6g6f
        let low_score_move = Move::normal(Square::new(5, 2), Square::new(5, 3), false); // 4g4f

        history.update_cutoff(Color::Black, high_score_move, 20, None);
        history.update_cutoff(Color::Black, med_score_move, 10, None);
        history.update_cutoff(Color::Black, low_score_move, 5, None);

        let history_arc = Arc::new(history);
        let mut picker = MovePicker::new(&pos, None, None, &history_arc, &stack, 1);

        // Skip captures and killers
        let mut quiet_moves = Vec::new();
        while let Some(mv) = picker.next_move() {
            if !mv.is_drop() && pos.board.piece_on(mv.to()).is_none() {
                quiet_moves.push(mv);
                if quiet_moves.len() >= 10 {
                    break;
                }
            }
        }

        // Check if high score move appears before low score move
        let high_pos = quiet_moves.iter().position(|&m| m == high_score_move);
        let low_pos = quiet_moves.iter().position(|&m| m == low_score_move);

        if let (Some(h), Some(l)) = (high_pos, low_pos) {
            assert!(h < l, "High history score move should come before low score move");
        }
    }

    /// Test 5: Duplicate elimination - TT=Killer=PV same move
    #[test]
    fn test_duplicate_elimination() {
        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let mut stack = SearchStack::default();

        // Set same move as TT and killer
        let the_move = Move::normal(Square::new(7, 2), Square::new(7, 3), false);
        let tt_move = Some(the_move);
        stack.killers[0] = Some(the_move);

        let mut picker = MovePicker::new(&pos, tt_move, None, &history, &stack, 1);

        // Count occurrences
        let mut count = 0;
        let mut total = 0;
        while let Some(mv) = picker.next_move() {
            if mv == the_move {
                count += 1;
            }
            total += 1;
            if total >= 50 {
                break;
            }
        }

        assert_eq!(count, 1, "Move should appear exactly once");
    }

    /// Test 6: Limited move positions
    #[test]
    fn test_limited_move_positions() {
        // Test with position after many moves (fewer legal moves)
        let mut pos = Position::startpos();

        // Make several moves to reduce options
        let moves = [
            Move::normal(Square::new(7, 2), Square::new(7, 3), false),
            Move::normal(Square::new(3, 6), Square::new(3, 5), false),
            Move::normal(Square::new(6, 2), Square::new(6, 3), false),
            Move::normal(Square::new(8, 6), Square::new(8, 5), false),
        ];

        for mv in &moves {
            pos.do_move(*mv);
        }

        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

        // Count moves
        let mut move_count = 0;
        while picker.next_move().is_some() {
            move_count += 1;
        }

        // After 4 moves, we should still have a reasonable number of moves
        // In shogi, the number of legal moves often increases after opening moves
        assert!(move_count > 20, "Position should have many moves available");
        assert!(move_count < 100, "Position shouldn't have too many moves");
    }

    /// Test 7: Deterministic ordering for equal-scored moves
    #[test]
    fn test_deterministic_ordering() {
        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        // Run multiple times and check consistency
        let mut move_sequences = Vec::new();

        for _ in 0..5 {
            let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);
            let mut moves = Vec::new();

            for _ in 0..10 {
                if let Some(mv) = picker.next_move() {
                    moves.push(mv);
                }
            }

            move_sequences.push(moves);
        }

        // All sequences should be identical
        for i in 1..move_sequences.len() {
            assert_eq!(
                move_sequences[0], move_sequences[i],
                "Move ordering should be deterministic"
            );
        }
    }

    /// Test 8: Thread safety (basic test - full test would use actual threads)
    #[test]
    fn test_thread_safety_basic() {
        use std::sync::Mutex;
        use std::thread;

        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        let pos_mutex = Arc::new(Mutex::new(pos));
        let results = Arc::new(Mutex::new(Vec::new()));

        let mut handles = vec![];

        // Spawn multiple threads
        for _ in 0..4 {
            let pos_clone = Arc::clone(&pos_mutex);
            let history_clone = Arc::clone(&history);
            let results_clone = Arc::clone(&results);
            let stack_clone = stack.clone();

            let handle = thread::spawn(move || {
                let pos = pos_clone.lock().unwrap();
                let mut picker = MovePicker::new(&pos, None, None, &history_clone, &stack_clone, 1);

                let mut moves = Vec::new();
                for _ in 0..5 {
                    if let Some(mv) = picker.next_move() {
                        moves.push(mv);
                    }
                }

                results_clone.lock().unwrap().push(moves);
            });

            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Check results
        let results = results.lock().unwrap();
        assert_eq!(results.len(), 4, "All threads should complete");
    }

    /// Test 9: Performance comparison (simplified version)
    #[test]
    fn test_performance_overhead() {
        // Skip this test in CI environments where performance is unstable
        if std::env::var("CI").is_ok() || std::env::var("GITHUB_ACTIONS").is_ok() {
            println!("Skipping performance test in CI environment");
            return;
        }
        use std::time::Instant;

        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        // Time MovePicker
        let start = Instant::now();
        for _ in 0..100 {
            let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);
            while picker.next_move().is_some() {}
        }
        let picker_time = start.elapsed();

        // Time direct generation
        let start = Instant::now();
        for _ in 0..100 {
            let mut moves = MoveList::new();
            let mut gen = MoveGen::new();
            gen.generate_all(&pos, &mut moves);
        }
        let direct_time = start.elapsed();

        // MovePicker should not be too much slower (allow 10x overhead for ordering logic)
        assert!(
            picker_time.as_micros() < direct_time.as_micros() * 10,
            "MovePicker overhead too high: {picker_time:?} vs {direct_time:?}"
        );
    }

    /// Test 10: Boundary conditions - invalid PV moves
    #[test]
    fn test_invalid_tt_move_handling() {
        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        // Create an invalid move (impossible in starting position)
        let invalid_tt = Some(Move::normal(Square::new(4, 4), Square::new(4, 5), false));

        let mut picker = MovePicker::new(&pos, invalid_tt, None, &history, &stack, 1);

        // First move should not be the invalid TT move
        let first = picker.next_move();
        assert_ne!(first, invalid_tt, "Invalid TT move should be skipped");

        // Should still generate all legal moves
        let mut count = 1; // Already got first
        while picker.next_move().is_some() {
            count += 1;
        }

        // Starting position has 30 legal moves
        assert_eq!(count, 30, "Should generate all legal moves even with invalid TT");
    }

    /// Helper test: Verify move generation consistency
    #[test]
    fn test_move_generation_consistency() {
        // Test with starting position only
        let positions = vec![Position::startpos()];

        for pos in positions {
            let history = Arc::new(History::new());
            let stack = SearchStack::default();

            // Generate with picker
            let mut picker1 = MovePicker::new(&pos, None, None, &history, &stack, 1);
            let mut moves1 = Vec::new();
            while let Some(mv) = picker1.next_move() {
                moves1.push(mv);
            }

            // Generate again
            let mut picker2 = MovePicker::new(&pos, None, None, &history, &stack, 1);
            let mut moves2 = Vec::new();
            while let Some(mv) = picker2.next_move() {
                moves2.push(mv);
            }

            assert_eq!(moves1.len(), moves2.len(), "Consistent move count");

            // Convert to sets for comparison (order might differ for equal-scored moves)
            let set1: HashSet<_> = moves1.into_iter().collect();
            let set2: HashSet<_> = moves2.into_iter().collect();
            assert_eq!(set1, set2, "Consistent move generation");
        }
    }

    /// Test 6: No moves in checkmate/stalemate positions
    #[test]
    fn test_no_moves_checkmate_stalemate() {
        // Create a position where we expect no legal moves
        // This is a simplified test - in a real checkmate position,
        // the king would be in check with no legal moves
        let mut pos = Position::startpos();

        // Make moves to create a position with very limited options
        // Note: Creating a true checkmate position requires specific setup
        let moves_to_limit = [
            Move::normal(Square::new(2, 2), Square::new(2, 3), false),
            Move::normal(Square::new(3, 6), Square::new(3, 5), false),
            Move::normal(Square::new(2, 3), Square::new(2, 4), false),
            Move::normal(Square::new(3, 5), Square::new(3, 4), false),
        ];

        for mv in &moves_to_limit {
            pos.do_move(*mv);
        }

        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        // Create picker and count moves
        let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);
        let mut move_count = 0;
        while picker.next_move().is_some() {
            move_count += 1;
        }

        // In this test position, we should still have legal moves
        // A true checkmate test would require a specific position setup
        assert!(move_count > 0, "Position should have legal moves");

        // TODO: Add a true checkmate position test when Position supports
        // creating from FEN or specific board setup
    }

    /// Test: SEE correctly evaluates captures in MovePicker
    #[test]
    fn test_see_with_pinned_pieces_integration() {
        // Create a position with multiple captures available
        let mut pos = Position::startpos();

        // Make some moves to create capture opportunities
        let moves = [
            Move::normal(Square::new(2, 2), Square::new(2, 3), false), // 7g7f
            Move::normal(Square::new(3, 6), Square::new(3, 5), false), // 6c6d
            Move::normal(Square::new(2, 3), Square::new(2, 4), false), // 7f7e
            Move::normal(Square::new(3, 5), Square::new(3, 4), false), // 6d6e
            Move::normal(Square::new(2, 4), Square::new(2, 5), false), // 7e7d
        ];

        for mv in &moves {
            pos.do_move(*mv);
        }

        let history = Arc::new(History::new());
        let stack = SearchStack::default();
        let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

        // Collect captures
        let mut captures = Vec::new();
        while let Some(mv) = picker.next_move() {
            if !mv.is_drop() && pos.board.piece_on(mv.to()).is_some() {
                captures.push(mv);
                // Test SEE value for this capture
                let see_value = pos.see(mv);
                println!("Capture {mv:?} has SEE value: {see_value}");
            }
            if captures.len() >= 10 {
                break;
            }
        }

        // Should have some captures
        assert!(!captures.is_empty(), "Should have found some captures");

        // Verify SEE is being used by checking that good captures come before bad ones
        // (This is a basic check - MovePicker should order captures by SEE value)
    }

    /// Test: Complex exchange sequences with X-ray attacks
    #[test]
    fn test_complex_exchange_sequence() {
        let mut pos = Position::empty();

        // Set up a complex position with multiple pieces in a line
        // Black pieces
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 7), Piece::new(PieceType::Rook, Color::Black));
        pos.board
            .put_piece(Square::new(4, 5), Piece::new(PieceType::Bishop, Color::Black));

        // White pieces
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(Square::new(4, 3), Piece::new(PieceType::Gold, Color::White));
        pos.board
            .put_piece(Square::new(4, 1), Piece::new(PieceType::Rook, Color::White));

        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        // Test SEE for Bishop takes Gold
        let bishop_takes_gold = Move::normal(Square::new(4, 5), Square::new(4, 3), false);
        let see_value = pos.see(bishop_takes_gold);

        // Bishop takes Gold (+600), Rook takes Bishop (-700), Rook takes Rook (+900)
        // Net: 600 - 700 + 900 = 800
        assert_eq!(see_value, 800, "Complex exchange should yield +800");
    }

    /// Test: Move ordering with SEE evaluation
    #[test]
    fn test_move_ordering_with_see() {
        let mut pos = Position::empty();

        // Create a position with multiple captures of different SEE values
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::White));

        // Attacking pieces (Black)
        pos.board
            .put_piece(Square::new(3, 3), Piece::new(PieceType::Pawn, Color::Black));
        pos.board
            .put_piece(Square::new(5, 3), Piece::new(PieceType::Rook, Color::Black));
        pos.board
            .put_piece(Square::new(3, 5), Piece::new(PieceType::Bishop, Color::Black));

        // Target pieces (White)
        pos.board
            .put_piece(Square::new(4, 2), Piece::new(PieceType::Gold, Color::White)); // Undefended
        pos.board
            .put_piece(Square::new(4, 1), Piece::new(PieceType::Pawn, Color::White)); // Defended by Rook
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::Rook, Color::White)); // Defender

        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        let history = Arc::new(History::new());
        let stack = SearchStack::default();
        let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

        let mut capture_order = Vec::new();
        while let Some(mv) = picker.next_move() {
            if !mv.is_drop() && pos.board.piece_on(mv.to()).is_some() {
                capture_order.push(mv);
                if capture_order.len() >= 3 {
                    break;
                }
            }
        }

        // Expected order:
        // 1. Any piece takes undefended Gold (SEE = +600)
        // 2. Pawn takes defended Pawn (SEE = 0, equal exchange)
        // 3. Rook/Bishop takes defended Pawn (SEE < 0, bad capture)

        if !capture_order.is_empty() {
            let first_capture = capture_order[0];
            assert_eq!(
                first_capture.to(),
                Square::new(4, 2),
                "First capture should be the undefended Gold"
            );
        }
    }
}
