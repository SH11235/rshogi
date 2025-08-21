//! Transposition table operations for the unified searcher
//!
//! This module contains all TT-related operations that are compile-time optimized
//! based on the USE_TT const generic parameter.

use crate::{
    search::{
        adaptive_prefetcher::AdaptivePrefetcher,
        tt::{NodeType, TTEntry},
        ShardedTranspositionTable,
    },
    shogi::Move,
};
use std::sync::Arc;

/// Trait for transposition table operations
///
/// This trait is implemented by UnifiedSearcher and provides all TT-related operations
/// with compile-time optimization based on const generics.
pub trait TTOperations<const USE_TT: bool> {
    /// Get reference to the transposition table
    fn tt(&self) -> &Option<Arc<ShardedTranspositionTable>>;

    /// Get reference to the adaptive prefetcher
    fn adaptive_prefetcher(&self) -> &Option<AdaptivePrefetcher>;

    /// Check if prefetching is disabled
    fn is_prefetch_disabled(&self) -> bool;

    /// Probe transposition table (compile-time optimized)
    #[inline(always)]
    fn probe_tt(&self, hash: u64) -> Option<TTEntry> {
        if USE_TT {
            self.tt().as_ref()?.probe(hash)
        } else {
            None
        }
    }

    /// Store in transposition table (compile-time optimized)
    #[inline(always)]
    fn store_tt(
        &self,
        hash: u64,
        depth: u8,
        score: i32,
        node_type: NodeType,
        best_move: Option<Move>,
    ) {
        if USE_TT {
            if let Some(ref tt) = self.tt() {
                // Store entry (duplication tracking temporarily disabled)
                tt.store(hash, best_move, score as i16, 0, depth, node_type);

                // // Update duplication statistics based on store result
                // if let Some(ref stats) = self.duplication_stats {
                //     // Always increment total nodes when storing
                //     stats.total_nodes.fetch_add(1, Ordering::Relaxed);

                //     // Only increment unique if this was a new entry
                //     if is_new_entry {
                //         stats.unique_nodes.fetch_add(1, Ordering::Relaxed);
                //     }

                //     // Debug logging for duplication stats
                //     let total = stats.total_nodes.load(Ordering::Relaxed);
                //     let unique = stats.unique_nodes.load(Ordering::Relaxed);
                //     if total % 1000 == 0 {
                //         debug!(
                //             "DuplicationStats snapshot: unique={unique}, total={total}, dup%={:.1}",
                //             ((total - unique) as f64 * 100.0 / total as f64)
                //         );
                //     }
                // }  // Temporarily disabled
            }
        }
    }

    /// Prefetch transposition table entry (compile-time optimized)
    #[inline(always)]
    fn prefetch_tt(&self, hash: u64) {
        if USE_TT && !self.is_prefetch_disabled() {
            if let Some(ref tt) = self.tt() {
                tt.prefetch_l1(hash); // Use L1 cache for immediate access
            }
        }
    }

    /// Get TT statistics (for benchmarking)
    #[inline(always)]
    fn get_tt_stats(&self) -> Option<(f32, u64, u64)> {
        if USE_TT {
            if let Some(ref tt) = self.tt() {
                let hashfull = tt.hashfull() as f32 / 1000.0;
                // TODO: Add actual hit/miss stats from TT
                return Some((hashfull, 0, 0));
            }
        }
        None
    }

    /// Get adaptive prefetcher statistics (for benchmarking)
    #[inline(always)]
    fn get_prefetch_stats(&self) -> Option<(u64, u64)> {
        if USE_TT {
            if let Some(ref prefetcher) = self.adaptive_prefetcher() {
                let stats = prefetcher.stats();
                return Some((stats.hits, stats.misses));
            }
        }
        None
    }
}

/// Implementation of TTOperations for UnifiedSearcher
///
/// This is implemented in the main module to access private fields
impl<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize> TTOperations<USE_TT>
    for super::UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>
where
    E: crate::evaluation::evaluate::Evaluator + Send + Sync + 'static,
{
    #[inline(always)]
    fn tt(&self) -> &Option<Arc<ShardedTranspositionTable>> {
        &self.tt
    }

    #[inline(always)]
    fn adaptive_prefetcher(&self) -> &Option<AdaptivePrefetcher> {
        &self.adaptive_prefetcher
    }

    #[inline(always)]
    fn is_prefetch_disabled(&self) -> bool {
        self.disable_prefetch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{evaluation::evaluate::MaterialEvaluator, search::unified::UnifiedSearcher};

    #[test]
    fn test_tt_operations_with_tt_enabled() {
        let searcher = UnifiedSearcher::<_, true, false, 8>::new(MaterialEvaluator);

        // TT should be available
        assert!(searcher.tt().is_some());

        // Stats should return Some
        let stats = searcher.get_tt_stats();
        assert!(stats.is_some());

        // Prefetcher should be available
        assert!(searcher.adaptive_prefetcher().is_some());
    }

    #[test]
    fn test_tt_operations_with_tt_disabled() {
        let searcher = UnifiedSearcher::<_, false, false, 8>::new(MaterialEvaluator);

        // TT should not be available
        assert!(searcher.tt().is_none());

        // Stats should return None
        let stats = searcher.get_tt_stats();
        assert!(stats.is_none());

        // Prefetcher should not be available
        assert!(searcher.adaptive_prefetcher().is_none());
    }

    #[test]
    fn test_probe_tt_compile_time_optimization() {
        // With TT enabled
        let searcher_with_tt = UnifiedSearcher::<_, true, false, 8>::new(MaterialEvaluator);
        let result = searcher_with_tt.probe_tt(12345);
        // Should return None (empty table) but not panic
        assert!(result.is_none());

        // With TT disabled - should always return None
        let searcher_without_tt = UnifiedSearcher::<_, false, false, 8>::new(MaterialEvaluator);
        let result = searcher_without_tt.probe_tt(12345);
        assert!(result.is_none());
    }
}
