//! Builder pattern for UnifiedSearcher initialization
//!
//! This module provides a fluent API for constructing UnifiedSearcher instances
//! with various configurations, reducing code duplication in constructors.

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        adaptive_prefetcher::AdaptivePrefetcher, history::History,
        parallel::shared::DuplicationStats, types::SearchStack, SearchStats, TranspositionTable,
    },
};
use std::sync::{Arc, Mutex};

use super::{aspiration::AspirationWindow, context, core, ordering, UnifiedSearcher};

/// Builder for UnifiedSearcher instances
pub struct UnifiedSearcherBuilder<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    evaluator: Arc<E>,
    tt: Option<Arc<TranspositionTable>>,
    history: Option<Arc<Mutex<History>>>,
    duplication_stats: Option<Arc<DuplicationStats>>,
    disable_prefetch: bool,
    tt_size_mb: usize,
}

impl<E> UnifiedSearcherBuilder<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Create a new builder with the given evaluator
    pub fn new(evaluator: E) -> Self {
        Self {
            evaluator: Arc::new(evaluator),
            tt: None,
            history: None,
            duplication_stats: None,
            disable_prefetch: false,
            tt_size_mb: 16, // Default TT size
        }
    }

    /// Create a new builder with an Arc-wrapped evaluator
    pub fn with_arc(evaluator: Arc<E>) -> Self {
        Self {
            evaluator,
            tt: None,
            history: None,
            duplication_stats: None,
            disable_prefetch: false,
            tt_size_mb: 16, // Default TT size
        }
    }

    /// Set a shared transposition table
    pub fn with_shared_tt(mut self, tt: Arc<TranspositionTable>) -> Self {
        self.tt = Some(tt);
        self
    }

    /// Set a shared history table
    pub fn with_shared_history(mut self, history: Arc<Mutex<History>>) -> Self {
        self.history = Some(history);
        self
    }

    /// Set duplication statistics for parallel search
    pub fn with_duplication_stats(mut self, stats: Arc<DuplicationStats>) -> Self {
        self.duplication_stats = Some(stats);
        self
    }

    /// Disable prefetching (for benchmarking)
    pub fn disable_prefetch(mut self, disable: bool) -> Self {
        self.disable_prefetch = disable;
        self
    }

    /// Set transposition table size in MB
    pub fn with_tt_size(mut self, size_mb: usize) -> Self {
        self.tt_size_mb = size_mb;
        self
    }

    /// Build the UnifiedSearcher instance
    pub fn build<const USE_TT: bool, const USE_PRUNING: bool>(
        self,
    ) -> UnifiedSearcher<E, USE_TT, USE_PRUNING> {
        // Create or use shared history
        let history = self.history.unwrap_or_else(|| Arc::new(Mutex::new(History::new())));

        // Create or use shared TT
        let tt = if USE_TT {
            Some(self.tt.unwrap_or_else(|| Arc::new(TranspositionTable::new(self.tt_size_mb))))
        } else {
            None
        };

        // Pre-allocate search stack
        let search_stack = Self::create_search_stack();

        // Create adaptive prefetcher if TT is enabled
        let adaptive_prefetcher = if USE_TT {
            Some(AdaptivePrefetcher::new())
        } else {
            None
        };

        UnifiedSearcher {
            evaluator: self.evaluator,
            tt,
            history: history.clone(),
            ordering: ordering::MoveOrdering::new(history),
            pv_table: core::PVTable::new(),
            stats: SearchStats::default(),
            context: context::SearchContext::new(),
            time_manager: None,
            search_stack,
            aspiration_window: AspirationWindow::new(),
            disable_prefetch: self.disable_prefetch,
            adaptive_prefetcher,
            duplication_stats: self.duplication_stats,
            previous_pv: Vec::new(),
            prev_root_hash: None,
        }
    }

    /// Create pre-allocated search stack
    fn create_search_stack() -> Vec<SearchStack> {
        // Pre-allocate search stack for maximum search depth
        // This is a small amount of memory (8KB) and avoids dynamic allocation during search
        let mut search_stack = Vec::with_capacity(crate::search::constants::MAX_PLY + 1);
        for ply in 0..=crate::search::constants::MAX_PLY {
            search_stack.push(SearchStack::new(ply as u16));
        }
        search_stack
    }
}

/// Convenience constructors for UnifiedSearcher
impl<E, const USE_TT: bool, const USE_PRUNING: bool> UnifiedSearcher<E, USE_TT, USE_PRUNING>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Create a new unified searcher using the builder
    pub fn new(evaluator: E) -> Self {
        UnifiedSearcherBuilder::new(evaluator).build()
    }

    /// Create a new unified searcher with specific TT size
    pub fn new_with_tt_size(evaluator: E, tt_size_mb: usize) -> Self {
        UnifiedSearcherBuilder::new(evaluator).with_tt_size(tt_size_mb).build()
    }

    /// Create a new unified searcher with an already Arc-wrapped evaluator
    pub fn with_arc(evaluator: Arc<E>) -> Self {
        UnifiedSearcherBuilder::with_arc(evaluator).build()
    }

    /// Create a new unified searcher with shared transposition table
    pub fn with_shared_tt(evaluator: Arc<E>, tt: Arc<TranspositionTable>) -> Self {
        UnifiedSearcherBuilder::with_arc(evaluator).with_shared_tt(tt).build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{evaluation::evaluate::MaterialEvaluator, search::unified::TTOperations};

    #[test]
    fn test_builder_basic() {
        let evaluator = MaterialEvaluator;
        let searcher: UnifiedSearcher<_, true, false> =
            UnifiedSearcherBuilder::new(evaluator).build();
        assert_eq!(searcher.nodes(), 0);
    }

    #[test]
    fn test_builder_with_shared_tt() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        let searcher1: UnifiedSearcher<_, true, false> =
            UnifiedSearcherBuilder::with_arc(evaluator.clone())
                .with_shared_tt(tt.clone())
                .build();

        let searcher2: UnifiedSearcher<_, true, false> =
            UnifiedSearcherBuilder::with_arc(evaluator).with_shared_tt(tt.clone()).build();

        // Both searchers should have the same TT instance
        assert!(Arc::ptr_eq(searcher1.tt().as_ref().unwrap(), searcher2.tt().as_ref().unwrap()));
    }

    #[test]
    fn test_builder_disable_prefetch() {
        let evaluator = MaterialEvaluator;
        let searcher: UnifiedSearcher<_, true, false> =
            UnifiedSearcherBuilder::new(evaluator).disable_prefetch(true).build();

        assert!(searcher.is_prefetch_disabled());
    }

    #[test]
    fn test_convenience_constructors() {
        let evaluator = MaterialEvaluator;

        // Test new()
        let searcher1 = UnifiedSearcher::<_, true, false>::new(evaluator);
        assert_eq!(searcher1.nodes(), 0);

        // Test new_with_tt_size()
        let searcher2 = UnifiedSearcher::<_, true, false>::new_with_tt_size(evaluator, 32);
        assert_eq!(searcher2.nodes(), 0);

        // Test with_arc()
        let evaluator = Arc::new(MaterialEvaluator);
        let searcher3 = UnifiedSearcher::<_, true, false>::with_arc(evaluator.clone());
        assert_eq!(searcher3.nodes(), 0);

        // Test with_shared_tt()
        let tt = Arc::new(TranspositionTable::new(8));
        let searcher4 = UnifiedSearcher::<_, true, false>::with_shared_tt(evaluator, tt);
        assert_eq!(searcher4.nodes(), 0);
    }
}
