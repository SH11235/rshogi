//! Unified search engine with compile-time feature configuration
//!
//! This module implements a single search engine that can be configured
//! at compile time to use different features, eliminating runtime overhead.

pub mod context;
pub mod core;
pub mod ordering;
pub mod pruning;

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        history::History,
        tt::{NodeType, TranspositionTable},
        SearchLimits, SearchResult, SearchStats,
    },
    shogi::{Move, Position},
};
use std::{sync::Arc, time::Instant};

/// Unified searcher with compile-time feature configuration
///
/// # Type Parameters
/// - `E`: The evaluator type (e.g., MaterialEvaluator, NnueEvaluator)
/// - `USE_TT`: Whether to use transposition table
/// - `USE_PRUNING`: Whether to use advanced pruning techniques
/// - `TT_SIZE_MB`: Transposition table size in megabytes
///
/// # Examples
/// ```
/// // Basic searcher with minimal features
/// type BasicSearcher = UnifiedSearcher<MaterialEvaluator, true, false, 8>;
///
/// // Enhanced searcher with all features
/// type EnhancedSearcher = UnifiedSearcher<NnueEvaluator, true, true, 16>;
/// ```
pub struct UnifiedSearcher<
    E,
    const USE_TT: bool = true,
    const USE_PRUNING: bool = true,
    const TT_SIZE_MB: usize = 16,
> where
    E: Evaluator + Send + Sync + 'static,
{
    /// The evaluation function (internally Arc-wrapped for efficient sharing)
    evaluator: Arc<E>,

    /// Transposition table (conditionally compiled)
    tt: Option<TranspositionTable>,

    /// Move ordering history
    history: History,

    /// Move ordering module
    ordering: ordering::MoveOrdering,

    /// Principal variation table
    pv_table: core::PVTable,

    /// Search statistics
    stats: SearchStats,

    /// Search context
    context: context::SearchContext,
}

impl<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>
    UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Create a new unified searcher
    pub fn new(evaluator: E) -> Self {
        let mut history = History::new();
        let history_ptr = &mut history as *mut History;

        Self {
            evaluator: Arc::new(evaluator),
            tt: if USE_TT {
                Some(TranspositionTable::new(TT_SIZE_MB))
            } else {
                None
            },
            history,
            ordering: ordering::MoveOrdering::new(history_ptr),
            pv_table: core::PVTable::new(),
            stats: SearchStats::default(),
            context: context::SearchContext::new(),
        }
    }

    /// Create a new unified searcher with an already Arc-wrapped evaluator
    pub fn with_arc(evaluator: Arc<E>) -> Self {
        let mut history = History::new();
        let history_ptr = &mut history as *mut History;

        Self {
            evaluator,
            tt: if USE_TT {
                Some(TranspositionTable::new(TT_SIZE_MB))
            } else {
                None
            },
            history,
            ordering: ordering::MoveOrdering::new(history_ptr),
            pv_table: core::PVTable::new(),
            stats: SearchStats::default(),
            context: context::SearchContext::new(),
        }
    }

    /// Main search entry point
    pub fn search(&mut self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        // Reset search state
        self.stats = SearchStats::default();
        self.context.reset();
        self.pv_table.clear();

        let start_time = Instant::now();

        // Initialize search context with limits
        self.context.set_limits(limits);

        // Iterative deepening
        let mut best_move = None;
        let mut best_score = 0;
        let mut depth = 1;

        while depth <= self.context.max_depth() && !self.context.should_stop() {
            // Search at current depth
            let (score, pv) = self.search_root(pos, depth);

            if !self.context.should_stop() {
                best_score = score;
                if !pv.is_empty() {
                    best_move = Some(pv[0]);
                    self.pv_table.update_from_line(&pv);
                }

                // Update statistics
                self.stats.depth = depth;
                self.stats.pv = pv.clone();

                // Call info callback if available
                if let Some(callback) = self.context.info_callback() {
                    callback(depth, score, self.stats.nodes, self.context.elapsed(), &pv);
                }
            }

            depth += 1;
        }

        self.stats.elapsed = start_time.elapsed();

        SearchResult {
            best_move,
            score: best_score,
            stats: self.stats.clone(),
        }
    }

    /// Search from the root position
    fn search_root(&mut self, pos: &mut Position, depth: u8) -> (i32, Vec<Move>) {
        // Implementation will be added in core module
        core::search_root(self, pos, depth)
    }

    /// Get current node count
    pub fn nodes(&self) -> u64 {
        self.stats.nodes
    }

    /// Get principal variation
    pub fn principal_variation(&self) -> &[Move] {
        self.pv_table.get_line(0)
    }

    /// Get current search depth
    pub fn current_depth(&self) -> u8 {
        self.stats.depth
    }

    /// Probe transposition table (compile-time optimized)
    #[inline(always)]
    pub(crate) fn probe_tt(&self, hash: u64) -> Option<crate::search::tt::TTEntry> {
        if USE_TT {
            self.tt.as_ref()?.probe(hash)
        } else {
            None
        }
    }

    /// Store in transposition table (compile-time optimized)
    #[inline(always)]
    pub(crate) fn store_tt(
        &self,
        hash: u64,
        depth: u8,
        score: i32,
        node_type: NodeType,
        best_move: Option<Move>,
    ) {
        if USE_TT {
            if let Some(ref tt) = self.tt {
                tt.store(hash, best_move, score as i16, 0, depth, node_type);
            }
        }
    }
}

/// Type aliases for common configurations
pub type BasicSearcher =
    UnifiedSearcher<crate::evaluation::evaluate::MaterialEvaluator, true, false, 8>;
pub type EnhancedSearcher<E> = UnifiedSearcher<E, true, true, 16>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;

    #[test]
    fn test_unified_searcher_creation() {
        let evaluator = MaterialEvaluator;
        let searcher = UnifiedSearcher::<_, true, false, 8>::new(evaluator);
        assert_eq!(searcher.nodes(), 0);
    }

    #[test]
    fn test_compile_time_features() {
        // Test that const generic parameters work correctly
        // We can directly use the const parameters in the type
        type BasicConfig = UnifiedSearcher<MaterialEvaluator, true, false, 8>;
        type EnhancedConfig = UnifiedSearcher<MaterialEvaluator, true, true, 16>;

        // These tests verify the type system works correctly with const generics
        // The actual behavior is tested in search tests
        let basic_eval = MaterialEvaluator;
        let _basic = BasicConfig::new(basic_eval);

        let enhanced_eval = MaterialEvaluator;
        let _enhanced = EnhancedConfig::new(enhanced_eval);
    }
}
