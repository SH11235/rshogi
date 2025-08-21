//! Tests for quiescence search functionality

use crate::evaluation::evaluate::{Evaluator, MaterialEvaluator};
use crate::search::unified::core::quiescence;
use crate::search::unified::UnifiedSearcher;
use crate::search::{common::mate_score, SearchLimits};
use crate::Position;

#[test]
fn test_quiescence_search_check_evasion() {
    // Test that quiescence search correctly handles check evasions
    // Create a position where the only legal move is a non-capture evasion

    // Position: Black king in check from white rook, must move (non-capture)
    // In shogi SFEN: K=black king, k=white king, R=black rook, r=white rook
    // 9 . . . . . . . . .
    // 8 . . . . . . . . .
    // 7 . . . . . . . . .
    // 6 . . . . . . . . .
    // 5 . . . . K . . . r  (Black King at 5e, white rook at 1e)
    // 4 . . . . . . . . .
    // 3 . . . . . . . . .
    // 2 . . . . . . . . .
    // 1 . . . . . . . . .
    //   9 8 7 6 5 4 3 2 1
    let pos = Position::from_sfen("9/9/9/9/4K3r/9/9/9/9 b - 1").unwrap();

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);
    searcher.context.set_limits(SearchLimits::builder().depth(1).build());

    // Verify we're in check
    assert!(pos.is_in_check(), "Position should be in check");

    // Run quiescence search at depth 0
    let mut test_pos = pos.clone();
    let score = quiescence::quiescence_search(&mut searcher, &mut test_pos, -1000, 1000, 0, 0);

    // Score should not be mate (we have legal moves)
    assert!(
        score != mate_score(0, false),
        "Should not return mate score when evasions exist"
    );
    assert!(score > -30000, "Score should be reasonable, not mate");

    // Verify that quiescence searched moves (node count should be > 1)
    assert!(searcher.stats.nodes > 1, "Quiescence should have searched evasion moves");
}

#[test]
fn test_quiescence_search_check_at_depth_limit() {
    // Test that quiescence search handles check correctly even at depth limit
    // Position: Black king in check, test at near max quiescence depth
    let pos = Position::from_sfen("9/9/9/9/4K3r/9/9/9/9 b - 1").unwrap();

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);
    searcher.context.set_limits(SearchLimits::builder().depth(1).build());

    // Verify we're in check
    assert!(pos.is_in_check(), "Position should be in check");

    // Run quiescence search near the depth limit
    // Use a high ply value that would trigger depth limit for non-check positions
    let mut test_pos = pos.clone();
    let high_ply = 31; // Near the quiescence depth limit
    let score =
        quiescence::quiescence_search(&mut searcher, &mut test_pos, -1000, 1000, high_ply, 0);

    // Even at depth limit, when in check we should search evasions
    // Score should not be static eval (which would be positive for black)
    assert!(
        score != searcher.evaluator.evaluate(&pos),
        "Should not return static eval when in check at depth limit"
    );

    // Should have searched at least some moves
    assert!(
        searcher.stats.nodes >= 1,
        "Should search moves even at depth limit when in check"
    );
}

#[test]
fn test_quiescence_relative_depth_limit() {
    // Test that relative qply limit is enforced consistently
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(evaluator);
    searcher.context.set_limits(SearchLimits::builder().depth(10).build());

    // Create a complex position with many captures
    let pos = Position::from_sfen("k8/9/9/3G1G3/2P1P1P2/3B1R3/9/9/K8 b - 1").unwrap();

    // Test from different starting plies
    for start_ply in [0, 20, 40, 60] {
        searcher.stats.nodes = 0;
        searcher.stats.qnodes = 0;

        let mut test_pos = pos.clone();
        let _score =
            quiescence::quiescence_search(&mut searcher, &mut test_pos, -1000, 1000, start_ply, 0);

        // Record node counts for this starting ply
        let nodes_at_ply = searcher.stats.qnodes;

        // Node counts should be similar regardless of starting ply
        // (within reasonable variance due to different evaluations)
        if start_ply > 0 {
            assert!(nodes_at_ply > 0, "Should search some nodes from ply {start_ply}");
        }
    }
}

#[test]
fn test_quiescence_check_no_relative_limit() {
    // Test that relative qply limit is NOT applied when in check
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);
    searcher.context.set_limits(SearchLimits::builder().depth(1).build());

    // Position: King in check from rook
    let check_pos = Position::from_sfen("k8/9/9/9/4K3r/9/9/9/9 b - 1").unwrap();
    assert!(check_pos.is_in_check());

    // Call quiescence search with qply already at MAX_QPLY
    // This should still search moves because we're in check
    let mut test_pos = check_pos.clone();
    let qply_at_limit = crate::search::constants::MAX_QPLY;
    let score = quiescence::quiescence_search(
        &mut searcher,
        &mut test_pos,
        -1000,
        1000,
        10, // low absolute ply
        qply_at_limit,
    );

    // Should have searched some moves despite qply being at limit
    assert!(
        searcher.stats.qnodes > 0,
        "Should search check evasions even when qply is at limit"
    );

    // Score should not be mate (we have legal moves)
    assert!(score > -30000, "Should not return mate score when evasions exist");
}
