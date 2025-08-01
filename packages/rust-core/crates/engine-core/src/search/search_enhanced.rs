//! Enhanced search engine wrapper for unified searcher
//!
//! This module provides backward compatibility by wrapping the unified searcher
//! with enhanced features enabled.

use crate::{
    evaluation::evaluate::Evaluator,
    search::{unified::UnifiedSearcher, SearchLimits},
    shogi::{Move, Position},
};
use std::sync::Arc;

// Re-export types that were previously defined here for backward compatibility
// GamePhase is now in time_management module

/// Search stack entry - now just a re-export for backward compatibility
#[derive(Clone, Default)]
pub struct SearchStack {
    /// Current move being searched
    pub current_move: Option<Move>,
    /// Static evaluation
    pub static_eval: i32,
    /// Killer moves
    pub killers: [Option<Move>; 2],
    /// Move count
    pub move_count: u32,
    /// PV node flag
    pub pv: bool,
    /// Null move tried flag
    pub null_move: bool,
    /// In check flag
    pub in_check: bool,
}

/// Enhanced searcher using unified searcher with pruning enabled
pub struct EnhancedSearcher<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Unified searcher with enhanced features (TT enabled, pruning enabled)
    searcher: UnifiedSearcher<E, true, true, 16>,
}

impl<E> EnhancedSearcher<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Create new enhanced searcher
    pub fn new(evaluator: E) -> Self {
        // Create unified searcher with TT enabled, pruning enabled, 16MB TT
        EnhancedSearcher {
            searcher: UnifiedSearcher::new(evaluator),
        }
    }

    /// Get current principal variation
    pub fn principal_variation(&self) -> &[Move] {
        self.searcher.principal_variation()
    }

    /// Search position with SearchLimits
    pub fn search_with_limits(
        &mut self,
        pos: &mut Position,
        limits: SearchLimits,
    ) -> (Option<Move>, i32) {
        // Use unified searcher
        let result = self.searcher.search(pos, limits);
        (result.best_move, result.score)
    }

    /// Get node count
    pub fn nodes(&self) -> u64 {
        self.searcher.nodes()
    }

    /// Set time manager callback (no-op for unified searcher)
    pub fn set_time_manager_callback<F>(&mut self, _cb: F)
    where
        F: Fn(Arc<crate::time_management::TimeManager>) + Send + Sync + 'static,
    {
        // No-op for backward compatibility
        // Time manager is handled internally by UnifiedSearcher
    }

    /// Get current depth (no-op, returns 0)
    pub fn current_depth(&self) -> u8 {
        // UnifiedSearcher doesn't expose current depth
        // Return 0 for backward compatibility
        0
    }

    /// Legacy search interface
    pub fn search(
        &mut self,
        pos: &mut Position,
        max_depth: i32,
        time_limit: Option<std::time::Duration>,
        node_limit: Option<u64>,
    ) -> (Option<Move>, i32) {
        use crate::search::SearchLimitsBuilder;
        use crate::time_management::TimeControl;

        let mut builder = SearchLimitsBuilder::default();

        // Set depth
        builder = builder.depth(max_depth as u8);

        // Set time control
        if let Some(duration) = time_limit {
            builder = builder.time_control(TimeControl::FixedTime {
                ms_per_move: duration.as_millis() as u64,
            });
        } else if let Some(nodes) = node_limit {
            builder = builder.time_control(TimeControl::FixedNodes { nodes });
        }

        // Set node limit
        if let Some(nodes) = node_limit {
            builder = builder.nodes(nodes);
        }

        let limits = builder.build();
        let result = self.searcher.search(pos, limits);
        (result.best_move, result.score)
    }
}

// Backward compatibility implementation
impl EnhancedSearcher<Arc<dyn Evaluator + Send + Sync>> {
    /// Create with specific TT size (ignored, always uses 16MB)
    pub fn new_with_tt_size(
        tt_size_mb: usize,
        evaluator: Arc<dyn Evaluator + Send + Sync>,
    ) -> Self {
        // Note: tt_size_mb is ignored for backward compatibility
        // UnifiedSearcher always uses 16MB for enhanced configuration
        let _ = tt_size_mb;
        EnhancedSearcher {
            searcher: UnifiedSearcher::new(evaluator),
        }
    }

    /// Set external stop flag
    pub fn set_stop_flag(&mut self, stop_flag: Arc<std::sync::atomic::AtomicBool>) {
        // This would need to be handled through SearchLimits.stop_flag
        // For now, this is a no-op for backward compatibility
        let _ = stop_flag;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::shogi::Position;

    #[test]
    fn test_enhanced_search_basic() {
        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = EnhancedSearcher::new(evaluator);
        let mut pos = Position::startpos();

        let (best_move, score) = searcher.search(&mut pos, 4, None, None);

        assert!(best_move.is_some());
        assert!(score.abs() < 1000); // Should be relatively balanced
    }

    #[test]
    fn test_backward_compatibility() {
        // Test that backward compatibility constructors work
        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = EnhancedSearcher::new_with_tt_size(32, evaluator); // TT size ignored
        let mut pos = Position::startpos();

        let (best_move, _) = searcher.search(&mut pos, 3, None, None);
        assert!(best_move.is_some());
    }

    #[test]
    fn test_nodes_count() {
        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = EnhancedSearcher::new(evaluator);
        let mut pos = Position::startpos();

        let (_, _) = searcher.search(&mut pos, 3, None, None);
        assert!(searcher.nodes() > 0);
    }
}
