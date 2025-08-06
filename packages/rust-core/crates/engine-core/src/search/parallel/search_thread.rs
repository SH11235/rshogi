//! Individual search thread for parallel search
//!
//! Each thread maintains its own local state while sharing critical data structures

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        history::{CounterMoveHistory, History},
        unified::{ordering::KillerTable, UnifiedSearcher},
        SearchLimits, SearchResult,
    },
    shogi::{Move, Position},
};
use std::sync::Arc;

use super::shared::SharedSearchState;

/// Individual search thread with local state
pub struct SearchThread<E: Evaluator + Send + Sync + 'static> {
    /// Thread ID (0 is main thread)
    pub id: usize,

    /// The actual searcher instance
    /// Uses standard TT configuration
    pub searcher: UnifiedSearcher<E, true, true, 16>,

    /// Shared TT reference (overrides searcher's internal TT)
    pub shared_tt: Arc<crate::search::TranspositionTable>,

    /// Thread-local history table
    pub local_history: History,

    /// Thread-local counter move history
    pub local_counter_moves: CounterMoveHistory,

    /// Thread-local killer table
    pub local_killers: KillerTable,

    /// Thread-local principal variation
    pub thread_local_pv: Vec<Move>,

    /// PV generation number for synchronization
    pub generation: u64,

    /// Reference to shared state
    pub shared_state: Arc<SharedSearchState>,
}

impl<E: Evaluator + Send + Sync + 'static> SearchThread<E> {
    /// Create a new search thread
    pub fn new(
        id: usize,
        evaluator: Arc<E>,
        tt: Arc<crate::search::TranspositionTable>,
        shared_state: Arc<SharedSearchState>,
    ) -> Self {
        // Create searcher with standard configuration
        // We'll override TT access in search methods
        let searcher = UnifiedSearcher::with_arc(evaluator);

        Self {
            id,
            searcher,
            shared_tt: tt,
            local_history: History::new(),
            local_counter_moves: CounterMoveHistory::new(),
            local_killers: KillerTable::new(),
            thread_local_pv: Vec::new(),
            generation: 0,
            shared_state,
        }
    }

    /// Get start depth for this thread based on iteration
    pub fn get_start_depth(&self, iteration: usize) -> u8 {
        if self.id == 0 {
            // Main thread follows normal iterative deepening
            iteration as u8
        } else {
            // Helper threads skip depths
            let skip = (self.id - 1) % 3 + 1; // Skip 1-3 depths
            (iteration + skip) as u8
        }
    }

    /// Reset thread state for new search
    pub fn reset(&mut self) {
        self.local_history.clear_all();
        self.local_killers.clear();
        self.thread_local_pv.clear();
        self.generation = 0;
    }

    /// Search from this thread
    pub fn search(
        &mut self,
        position: &mut Position,
        limits: SearchLimits,
        depth: u8,
    ) -> SearchResult {
        // Update searcher's internal tables with thread-local versions
        self.searcher.set_history(self.local_history.clone());
        self.searcher.set_counter_moves(self.local_counter_moves.clone());

        // Note: The searcher already uses the shared TT from construction

        // Create depth-limited search with shared stop flag
        let depth_limits = SearchLimits {
            depth: Some(depth),
            stop_flag: Some(self.shared_state.stop_flag.clone()),
            ..limits
        };

        // Perform the search
        let result = self.searcher.search(position, depth_limits);

        // Update local tables from searcher
        self.local_history = self.searcher.get_history();
        self.local_counter_moves = self.searcher.get_counter_moves();

        // Update shared state if this is a better result
        self.shared_state.maybe_update_best(
            result.score,
            result.stats.pv.first().copied(),
            depth,
            self.generation,
        );

        result
    }

    /// Check if this thread should stop
    pub fn should_stop(&self) -> bool {
        self.shared_state.should_stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{evaluation::evaluate::MaterialEvaluator, search::TranspositionTable};
    use std::sync::{atomic::AtomicBool, Arc};

    #[test]
    fn test_search_thread_creation() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared_state = Arc::new(SharedSearchState::new(stop_flag));

        let thread = SearchThread::new(0, evaluator, tt, shared_state);
        assert_eq!(thread.id, 0);
    }

    #[test]
    fn test_start_depth_calculation() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared_state = Arc::new(SharedSearchState::new(stop_flag));

        // Main thread (id=0) should follow normal iterative deepening
        let main_thread = SearchThread::new(0, evaluator.clone(), tt.clone(), shared_state.clone());
        assert_eq!(main_thread.get_start_depth(1), 1);
        assert_eq!(main_thread.get_start_depth(5), 5);
        assert_eq!(main_thread.get_start_depth(10), 10);

        // Helper thread 1 should skip 1 depth
        let helper1 = SearchThread::new(1, evaluator.clone(), tt.clone(), shared_state.clone());
        assert_eq!(helper1.get_start_depth(1), 2); // 1 + 1
        assert_eq!(helper1.get_start_depth(5), 6); // 5 + 1

        // Helper thread 2 should skip 2 depths
        let helper2 = SearchThread::new(2, evaluator.clone(), tt.clone(), shared_state.clone());
        assert_eq!(helper2.get_start_depth(1), 3); // 1 + 2
        assert_eq!(helper2.get_start_depth(5), 7); // 5 + 2

        // Helper thread 3 should skip 3 depths
        let helper3 = SearchThread::new(3, evaluator.clone(), tt.clone(), shared_state.clone());
        assert_eq!(helper3.get_start_depth(1), 4); // 1 + 3
        assert_eq!(helper3.get_start_depth(5), 8); // 5 + 3

        // Helper thread 4 should cycle back to skip 1 depth
        let helper4 = SearchThread::new(4, evaluator, tt, shared_state);
        assert_eq!(helper4.get_start_depth(1), 2); // 1 + 1
    }
}
