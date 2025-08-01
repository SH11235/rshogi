//! Common test utilities for search engine tests
//!
//! Provides shared test fixtures and helper functions

use crate::{
    evaluation::evaluate::MaterialEvaluator,
    search::{SearchLimits, SearchLimitsBuilder, SearchResult},
    Color, Piece, PieceType, Position, Square,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

/// Standard test positions
pub struct TestPositions;

impl TestPositions {
    /// Standard starting position
    pub fn startpos() -> Position {
        Position::startpos()
    }

    /// Simple endgame position (kings only)
    pub fn endgame_simple() -> Position {
        let mut pos = Position::empty();
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
        pos.board.rebuild_occupancy_bitboards();
        pos
    }

    /// Midgame position with material imbalance
    pub fn midgame_imbalance() -> Position {
        Position::from_sfen("3g1ks2/5g3/2n1pp1p1/p3P1p2/1pP5P/P8/2N2PP2/6K2/L4G1NL b RSBPrslp 45")
            .expect("Valid SFEN")
    }

    /// Tactical position with immediate capture
    pub fn tactical_capture() -> Position {
        let mut pos = Position::empty();
        // Black king
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        // White king
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
        // Black rook that can capture white gold
        pos.board
            .put_piece(Square::new(7, 7), Piece::new(PieceType::Rook, Color::Black));
        // White gold to be captured
        pos.board
            .put_piece(Square::new(7, 1), Piece::new(PieceType::Gold, Color::White));
        pos.board.rebuild_occupancy_bitboards();
        pos
    }

    /// Position for testing check detection
    pub fn in_check_position() -> Position {
        let mut pos = Position::empty();
        // Black king in check from white rook
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(Square::new(4, 5), Piece::new(PieceType::Rook, Color::White));
        pos.board.rebuild_occupancy_bitboards();
        pos
    }
}

/// Helper to set up a searcher with default configuration
pub struct SearcherSetup;

impl SearcherSetup {
    /// Create a basic searcher with material evaluator
    pub fn basic_with_depth(depth: u8) -> (SearchLimits, Arc<MaterialEvaluator>) {
        let limits = SearchLimitsBuilder::default().depth(depth).build();
        let evaluator = Arc::new(MaterialEvaluator);
        (limits, evaluator)
    }

    /// Create a basic searcher with time limit
    pub fn basic_with_time(time_ms: u64) -> (SearchLimits, Arc<MaterialEvaluator>) {
        let limits = SearchLimitsBuilder::default().fixed_time_ms(time_ms).build();
        let evaluator = Arc::new(MaterialEvaluator);
        (limits, evaluator)
    }

    /// Create a searcher with stop flag
    pub fn with_stop_flag(depth: u8) -> (SearchLimits, Arc<MaterialEvaluator>, Arc<AtomicBool>) {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let limits =
            SearchLimitsBuilder::default().depth(depth).stop_flag(stop_flag.clone()).build();
        let evaluator = Arc::new(MaterialEvaluator);
        (limits, evaluator, stop_flag)
    }
}

/// Helper to set up stop flag with delayed trigger
pub fn setup_stop_flag_with_delay(delay_ms: u64) -> Arc<AtomicBool> {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(delay_ms));
        stop_flag_clone.store(true, Ordering::Release);
    });

    stop_flag
}

/// Common assertions for search results
pub struct SearchAssertions;

impl SearchAssertions {
    /// Basic validation that search produced a valid result
    pub fn assert_valid_result(result: &SearchResult) {
        assert!(result.best_move.is_some(), "Search should find a move");
        assert!(result.stats.nodes > 0, "Search should visit at least one node");
        // Elapsed time is always non-negative
    }

    /// Assert that score is within reasonable bounds
    pub fn assert_reasonable_score(result: &SearchResult) {
        assert!(result.score.abs() < 30000, "Score should not exceed mate bound");
    }

    /// Assert that search reached expected depth
    pub fn assert_minimum_depth(result: &SearchResult, min_depth: u8) {
        assert!(
            result.stats.depth >= min_depth,
            "Search depth {} should be at least {}",
            result.stats.depth,
            min_depth
        );
    }

    /// Assert that principal variation is consistent
    pub fn assert_pv_consistency(result: &SearchResult) {
        if let Some(best_move) = result.best_move {
            if !result.stats.pv.is_empty() {
                assert_eq!(
                    result.stats.pv[0], best_move,
                    "First move in PV should match best move"
                );
            }
        }
    }

    /// Assert that search stopped within time limit
    pub fn assert_time_limit_respected(result: &SearchResult, limit_ms: u64) {
        let elapsed_ms = result.stats.elapsed.as_millis() as u64;
        // Allow 10% margin for timing overhead
        let margin = limit_ms / 10;
        assert!(
            elapsed_ms <= limit_ms + margin,
            "Search time {elapsed_ms}ms exceeded limit {limit_ms}ms"
        );
    }
}

/// Performance measurement helpers
pub struct PerformanceMetrics;

impl PerformanceMetrics {
    /// Calculate nodes per second
    pub fn nodes_per_second(result: &SearchResult) -> f64 {
        let elapsed_secs = result.stats.elapsed.as_secs_f64();
        if elapsed_secs > 0.0 {
            result.stats.nodes as f64 / elapsed_secs
        } else {
            0.0
        }
    }

    /// Calculate effective branching factor
    pub fn effective_branching_factor(result: &SearchResult) -> f64 {
        if result.stats.depth > 0 {
            (result.stats.nodes as f64).powf(1.0 / result.stats.depth as f64)
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_helpers() {
        let pos = TestPositions::startpos();
        assert_eq!(pos.ply, 1);

        let endgame = TestPositions::endgame_simple();
        // Should have 2 kings
        let mut piece_count = 0;
        for i in 0..81 {
            if endgame.board.piece_on(Square(i)).is_some() {
                piece_count += 1;
            }
        }
        assert_eq!(piece_count, 2);

        let tactical = TestPositions::tactical_capture();
        // Should have 2 kings + 1 rook + 1 gold = 4 pieces
        let mut piece_count = 0;
        for i in 0..81 {
            if tactical.board.piece_on(Square(i)).is_some() {
                piece_count += 1;
            }
        }
        assert_eq!(piece_count, 4);
    }

    #[test]
    fn test_searcher_setup() {
        let (limits, evaluator) = SearcherSetup::basic_with_depth(5);
        assert_eq!(limits.depth, Some(5));
        assert!(Arc::strong_count(&evaluator) == 1);

        let (limits2, _, stop_flag) = SearcherSetup::with_stop_flag(10);
        assert_eq!(limits2.depth, Some(10));
        assert!(!stop_flag.load(Ordering::Acquire));
    }
}
