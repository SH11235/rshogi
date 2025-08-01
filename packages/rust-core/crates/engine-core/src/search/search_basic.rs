//! Basic search implementation using unified searcher
//!
//! This module provides backward compatibility by wrapping UnifiedSearcher

use crate::{
    evaluation::evaluate::Evaluator,
    search::{unified::UnifiedSearcher, SearchLimits, SearchResult},
    Position,
};
use std::sync::Arc;

/// Basic searcher configured for simple alpha-beta search
///
/// Uses UnifiedSearcher with:
/// - Transposition table: 8MB
/// - Advanced pruning: disabled
/// - Basic move ordering only
pub struct Searcher<E: Evaluator + Send + Sync + 'static> {
    inner: UnifiedSearcher<E, true, false, 8>,
}

impl<E: Evaluator + Send + Sync + 'static> Searcher<E> {
    /// Create a new basic searcher
    pub fn new(evaluator: E) -> Self {
        Self {
            inner: UnifiedSearcher::new(evaluator),
        }
    }

    /// Create a new basic searcher with Arc-wrapped evaluator
    pub fn with_arc(evaluator: Arc<E>) -> Self {
        Self {
            inner: UnifiedSearcher::with_arc(evaluator),
        }
    }

    /// Search for the best move
    pub fn search(&mut self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        self.inner.search(pos, limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::SearchLimitsBuilder;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_search_startpos() {
        let mut pos = Position::startpos();
        let evaluator = MaterialEvaluator;
        let limits = SearchLimitsBuilder::default().depth(3).build();
        let mut searcher = Searcher::new(evaluator);

        let result = searcher.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
        assert!(result.score.abs() < 1000); // Should be relatively balanced
    }

    #[test]
    fn test_search_with_stop_flag() {
        let pos = Position::startpos();
        let evaluator = MaterialEvaluator;
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Start search in a thread
        let mut pos_clone = pos.clone();
        let evaluator_clone = evaluator;
        let stop_flag_clone = stop_flag.clone();

        let handle = thread::spawn(move || {
            let limits = SearchLimitsBuilder::default()
                .depth(100) // Very deep search
                .stop_flag(stop_flag_clone)
                .build();
            let mut searcher = Searcher::new(evaluator_clone);
            searcher.search(&mut pos_clone, limits)
        });

        // Let it run for a bit longer to ensure search starts
        thread::sleep(Duration::from_millis(50));

        // Stop the search
        stop_flag.store(true, Ordering::Release);

        // Wait for thread to finish
        let result = handle.join().unwrap();

        // Should have found some move before stopping
        assert!(result.best_move.is_some());
    }

    #[test]
    fn test_fallback_move_quality() {
        // Position where most pieces are blocked
        let mut pos = Position::empty();
        use crate::{Color, Piece, PieceType, Square};

        // Kings
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));

        // Add a rook that can move
        pos.board
            .put_piece(Square::new(0, 7), Piece::new(PieceType::Rook, Color::Black));

        pos.board.rebuild_occupancy_bitboards();

        let evaluator = MaterialEvaluator;
        let limits = SearchLimitsBuilder::default().depth(3).build();
        let mut searcher = Searcher::new(evaluator);

        let result = searcher.search(&mut pos, limits);

        // Should find a rook move
        assert!(result.best_move.is_some());
    }

    #[test]
    fn test_search_with_arc() {
        // Test the with_arc() constructor for backward compatibility
        let mut pos = Position::startpos();
        let evaluator = Arc::new(MaterialEvaluator);
        let limits = SearchLimitsBuilder::default().depth(3).build();
        let mut searcher = Searcher::with_arc(evaluator);

        let result = searcher.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
        assert!(result.score.abs() < 1000); // Should be relatively balanced
    }
}
