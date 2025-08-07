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
use crossbeam_utils::CachePadded;
use log::trace;
use smallvec::SmallVec;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use super::shared::SharedSearchState;

/// Thread state for park control
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(u8)]
pub enum ThreadState {
    Idle = 0,
    Searching = 1,
}

/// Calculate flush threshold based on search depth
/// Deeper searches use larger thresholds to reduce flush frequency
#[inline]
fn calculate_flush_threshold(depth: u8) -> u32 {
    match depth {
        0..=6 => 25_000,    // Shallow search: flush more frequently
        7..=12 => 50_000,   // Medium search: current default
        13..=20 => 100_000, // Deep search: flush less frequently
        _ => 150_000,       // Very deep search: minimal flushing
    }
}

/// Local node counter - simplified for minimal overhead
/// Uses u32 for efficient operations on 32-bit architectures
struct LocalNodeCounter {
    /// Non-atomic local counter (only accessed by owning thread)
    count: u32,
    /// Current search depth for dynamic threshold calculation
    current_depth: u8,
}

impl LocalNodeCounter {
    fn new() -> Self {
        Self {
            count: 0,
            current_depth: 0,
        }
    }

    /// Set the current search depth for dynamic threshold calculation
    #[inline]
    fn set_depth(&mut self, depth: u8) {
        self.current_depth = depth;
    }

    /// Add nodes and flush if threshold reached
    #[inline(always)]
    fn add(&mut self, nodes: u64, shared_state: &SharedSearchState) {
        // Saturating add to prevent overflow on u32
        self.count = self.count.saturating_add(nodes as u32);
        let threshold = calculate_flush_threshold(self.current_depth);
        if self.count >= threshold {
            shared_state.add_nodes(self.count as u64);
            self.count = 0;
        }
    }

    /// Force flush all pending nodes
    fn flush(&mut self, shared_state: &SharedSearchState) {
        if self.count > 0 {
            shared_state.add_nodes(self.count as u64);
            self.count = 0;
        }
    }
}

/// Calculate appropriate park duration based on search depth and time constraints
fn calculate_park_duration(max_depth: u8, time_left_ms: Option<u64>) -> Duration {
    // If very little time left, use minimal park duration
    if let Some(time) = time_left_ms {
        if time < 1000 {
            return Duration::from_micros(200); // 0.2ms for bullet/blitz
        }
    }

    // Otherwise, base duration on search depth
    match max_depth {
        0..=8 => Duration::from_micros(200),  // Shallow search: 0.2ms
        9..=12 => Duration::from_micros(500), // Medium search: 0.5ms
        _ => Duration::from_millis(2),        // Deep search: 2ms
    }
}

/// Individual search thread with local state
pub struct SearchThread<E: Evaluator + Send + Sync + 'static> {
    /// Thread ID (0 is main thread)
    pub id: usize,

    /// The actual searcher instance
    /// Uses shared TT from parallel coordinator
    pub searcher: UnifiedSearcher<E, true, true, 16>,

    /// Thread-local history table
    pub local_history: History,

    /// Thread-local counter move history
    pub local_counter_moves: CounterMoveHistory,

    /// Thread-local killer table
    pub local_killers: KillerTable,

    /// Thread-local principal variation (typically 4 moves or less)
    pub thread_local_pv: SmallVec<[Move; 4]>,

    /// PV generation number for synchronization
    pub generation: u64,

    /// Reference to shared state
    pub shared_state: Arc<SharedSearchState>,

    /// Last reported node count (for differential updates)
    pub last_nodes: u64,

    /// Local node counter for reduced contention (cache padded to avoid false sharing)
    /// IMPORTANT: When adding new fields, ensure they don't share cache lines with this field
    local_node_counter: CachePadded<LocalNodeCounter>,

    /// Thread state for park control
    pub state: Arc<AtomicU8>,

    /// Thread handle for unpark operations
    pub thread_handle: Option<thread::Thread>,
}

impl<E: Evaluator + Send + Sync + 'static> SearchThread<E> {
    /// Create a new search thread
    pub fn new(
        id: usize,
        evaluator: Arc<E>,
        tt: Arc<crate::search::TranspositionTable>,
        shared_state: Arc<SharedSearchState>,
        duplication_stats: Option<Arc<super::DuplicationStats>>,
    ) -> Self {
        // Create searcher with shared TT
        let mut searcher = UnifiedSearcher::with_shared_tt(evaluator, tt);

        // Set duplication stats if provided
        if let Some(stats) = duplication_stats {
            searcher.set_duplication_stats(stats);
        }

        Self {
            id,
            searcher,
            local_history: History::new(),
            local_counter_moves: CounterMoveHistory::new(),
            local_killers: KillerTable::new(),
            thread_local_pv: SmallVec::new(),
            generation: 0,
            shared_state,
            last_nodes: 0,
            local_node_counter: CachePadded::new(LocalNodeCounter::new()),
            state: Arc::new(AtomicU8::new(ThreadState::Searching as u8)),
            thread_handle: None,
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
        self.last_nodes = 0;
        // Force flush any pending nodes before reset
        self.local_node_counter.flush(&self.shared_state);
        self.local_node_counter = CachePadded::new(LocalNodeCounter::new());
    }

    /// Search from this thread
    pub fn search(
        &mut self,
        position: &mut Position,
        limits: SearchLimits,
        depth: u8,
    ) -> SearchResult {
        // Set depth for dynamic threshold calculation
        self.local_node_counter.set_depth(depth);

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

        // Force flush at end of search to ensure all nodes are counted
        // Note: flush_nodes() internally calls report_nodes() first
        self.flush_nodes();

        // TODO: Sync local_history with shared_state.history
        // Currently each thread maintains independent history tables.
        // Sharing history information between threads could improve move ordering
        // and overall search efficiency. This requires:
        // 1. Periodic sync from local History to SharedHistory
        // 2. Reading from SharedHistory at the start of each iteration

        result
    }

    /// Check if this thread should stop
    pub fn should_stop(&self) -> bool {
        self.shared_state.should_stop()
    }

    /// Report node count difference to shared state
    pub fn report_nodes(&mut self) {
        let current_nodes = self.searcher.nodes();
        let diff = current_nodes.saturating_sub(self.last_nodes);
        if diff > 0 {
            // Add to local counter and auto-flush if threshold reached
            self.local_node_counter.add(diff, &self.shared_state);
            self.last_nodes = current_nodes;
            trace!("Thread {} reported {} nodes (total: {})", self.id, diff, current_nodes);
        }
    }

    /// Force flush all pending nodes to shared state
    pub fn flush_nodes(&mut self) {
        // First update local counter with any unreported nodes
        self.report_nodes();

        // Then force flush to shared state
        self.local_node_counter.flush(&self.shared_state);
    }

    /// Perform search iteration (pure search without state management)
    pub fn search_iteration(
        &mut self,
        position: &mut Position,
        limits: &SearchLimits,
        depth: u8,
    ) -> SearchResult {
        // Clone limits only when needed for internal searcher
        self.search(position, limits.clone(), depth)
    }

    /// Check if this thread should park based on depth
    pub fn should_park(&self, depth: u8, max_depth: u8) -> bool {
        // Only park if:
        // 1. This is a helper thread (not main thread)
        // 2. We've reached close to max depth
        // 3. But not if max_depth is very shallow (<=6) to avoid parking issues
        // This ensures depth 6 and below never park
        self.id > 0 && depth >= max_depth.saturating_sub(1) && max_depth > 6
    }

    /// Park thread with appropriate duration
    pub fn park_with_timeout(&self, max_depth: u8, time_left_ms: Option<u64>) {
        let duration = calculate_park_duration(max_depth, time_left_ms);
        thread::park_timeout(duration);
    }

    /// Set thread state
    pub fn set_state(&self, state: ThreadState) {
        self.state.store(state as u8, Ordering::Release);
    }

    /// Set thread handle for unpark operations
    pub fn set_thread_handle(&mut self, handle: thread::Thread) {
        self.thread_handle = Some(handle);
    }

    /// Unpark this thread if it's parked
    pub fn unpark(&self) {
        if let Some(ref handle) = self.thread_handle {
            handle.unpark();
        }
    }

    /// Check if thread is idle
    pub fn is_idle(&self) -> bool {
        self.state.load(Ordering::Acquire) == ThreadState::Idle as u8
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

        let thread = SearchThread::new(0, evaluator, tt, shared_state, None);
        assert_eq!(thread.id, 0);
        assert_eq!(thread.state.load(Ordering::Relaxed), ThreadState::Searching as u8);
    }

    #[test]
    fn test_thread_state_transitions() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared_state = Arc::new(SharedSearchState::new(stop_flag));

        let thread = SearchThread::new(1, evaluator, tt, shared_state, None);

        // Initially searching
        assert_eq!(thread.state.load(Ordering::Relaxed), ThreadState::Searching as u8);

        // Transition to idle
        thread.state.store(ThreadState::Idle as u8, Ordering::Release);
        assert!(thread.is_idle());

        // Back to searching
        thread.state.store(ThreadState::Searching as u8, Ordering::Release);
        assert!(!thread.is_idle());
    }

    #[test]
    fn test_start_depth_calculation() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared_state = Arc::new(SharedSearchState::new(stop_flag));

        // Test each thread ID separately to avoid stack overflow in release builds
        for thread_id in 0..5 {
            let thread = SearchThread::new(
                thread_id,
                evaluator.clone(),
                tt.clone(),
                shared_state.clone(),
                None,
            );

            // Calculate expected skip based on thread ID
            let skip = if thread_id == 0 {
                0 // Main thread: no skip
            } else {
                ((thread_id - 1) % 3) + 1 // Helpers: skip 1-3 cyclically
            };

            // Test various depths
            for base_depth in [1, 5, 10] {
                let expected = base_depth + skip;
                assert_eq!(
                    thread.get_start_depth(base_depth),
                    expected as u8,
                    "Thread {thread_id} with base depth {base_depth} should return {expected}"
                );
            }
        } // thread is dropped here, freeing stack memory
    }
}
