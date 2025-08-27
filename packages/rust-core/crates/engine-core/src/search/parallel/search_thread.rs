//! Individual search thread for parallel search
//!
//! Each thread maintains its own local state while sharing critical data structures

use crate::{
    evaluation::evaluate::Evaluator,
    search::{
        history::{CounterMoveHistory, History},
        unified::{ordering::KillerTable, UnifiedSearcher},
        SearchLimits, SearchResult, SearchStats, TranspositionTable,
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
use std::time::{Duration, Instant};

use super::shared::SharedSearchState;
#[cfg(feature = "ybwc")]
use super::shared::SplitPoint;

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
fn calculate_flush_threshold(depth: u8) -> u64 {
    match depth {
        0..=6 => 25_000,    // Shallow search: flush more frequently
        7..=12 => 50_000,   // Medium search: current default
        13..=20 => 100_000, // Deep search: flush less frequently
        _ => 150_000,       // Very deep search: minimal flushing
    }
}

/// Local node counter - simplified for minimal overhead
/// Uses u64 to avoid overflow in long-running searches
struct LocalNodeCounter {
    /// Non-atomic local counter (only accessed by owning thread)
    count: u64,
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
        // Saturating add to prevent overflow
        self.count = self.count.saturating_add(nodes);
        let threshold = calculate_flush_threshold(self.current_depth);
        if self.count >= threshold {
            shared_state.add_nodes(self.count);
            self.count = 0;
        }
    }

    /// Force flush all pending nodes
    fn flush(&mut self, shared_state: &SharedSearchState) {
        if self.count > 0 {
            shared_state.add_nodes(self.count);
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
    pub searcher: UnifiedSearcher<E, true, true>,

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
        tt: Arc<TranspositionTable>,
        shared_state: Arc<SharedSearchState>,
    ) -> Self {
        // Create searcher with shared TT
        let mut searcher = UnifiedSearcher::with_shared_tt(evaluator, tt);

        // Set duplication stats from shared state
        searcher.set_duplication_stats(shared_state.duplication_stats.clone());

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

        // Perform the search (don't reset stats for parallel threads)
        let result = self.searcher.search_with_options(position, depth_limits, false);

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

    /// Search a specific root move
    pub fn search_root_move(
        &mut self,
        position: &mut Position,
        limits: &SearchLimits,
        depth: u8,
        root_move: Move,
    ) -> SearchResult {
        // Set depth for dynamic threshold calculation
        self.local_node_counter.set_depth(depth);

        // Update searcher's internal tables with thread-local versions
        self.searcher.set_history(self.local_history.clone());
        self.searcher.set_counter_moves(self.local_counter_moves.clone());

        // Create depth-limited search with shared stop flag
        let depth_limits = SearchLimits {
            depth: Some(depth),
            stop_flag: Some(self.shared_state.stop_flag.clone()),
            ..limits.clone()
        };

        // Apply the root move
        let undo_info = position.do_move(root_move);

        // Search from the resulting position
        let result = if depth > 1 {
            // Search with reduced depth (don't reset stats for parallel threads)
            let search_result = self.searcher.search_with_options(position, depth_limits, false);

            // Negate score (we searched from opponent's perspective)
            SearchResult {
                score: -search_result.score,
                stats: search_result.stats,
                best_move: Some(root_move),
                node_type: search_result.node_type,
                stop_info: search_result.stop_info, // Preserve stop info from sub-search
            }
        } else {
            // At depth 1, just evaluate the position
            let score = -self.searcher.evaluate(position);
            SearchResult {
                score,
                stats: SearchStats {
                    depth: 1,
                    nodes: 1,
                    qnodes: 0,
                    elapsed: Duration::from_millis(0),
                    pv: vec![root_move],
                    ..Default::default()
                },
                best_move: Some(root_move),
                node_type: crate::search::NodeType::Exact, // Depth 1 evaluation is exact
                stop_info: Some(crate::search::types::StopInfo::default()), // Consistent with other cases
            }
        };

        // Unmake the move
        position.undo_move(root_move, undo_info);

        // Update local tables from searcher
        self.local_history = self.searcher.get_history();
        self.local_counter_moves = self.searcher.get_counter_moves();

        // Update shared state if this is a better result
        self.shared_state
            .maybe_update_best(result.score, Some(root_move), depth, self.generation);

        // Force flush at end of search to ensure all nodes are counted
        self.flush_nodes();

        result
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
        let start = Instant::now();

        // Poll every 5ms to check stop flag (more frequently to avoid hanging)
        let poll_interval = Duration::from_millis(5);

        loop {
            // Park for short interval
            thread::park_timeout(poll_interval);

            // Check stop flag immediately after waking
            if self.shared_state.should_stop() {
                break;
            }

            // Check if we've waited long enough
            if start.elapsed() >= duration {
                break;
            }
        }
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

    /// Process work from a split point (YBWC)
    #[cfg(feature = "ybwc")]
    pub fn process_split_point(&mut self, split_point: &Arc<SplitPoint>) {
        // Increment active thread count for this split point
        split_point.add_thread();
        self.shared_state.increment_active_threads();

        // Process moves from the split point until no more work
        while !self.should_stop() {
            // Try to get next move from split point
            let Some(mv) = split_point.get_next_move() else {
                break;
            };

            // Clone position for this thread's search
            let mut pos = split_point.position.clone();

            // Apply the move
            let undo_info = pos.do_move(mv);

            // Search from the resulting position
            let limits = SearchLimits {
                depth: Some(split_point.depth.saturating_sub(1)),
                stop_flag: Some(self.shared_state.stop_flag.clone()),
                ..Default::default()
            };

            let result = self.searcher.search_with_options(&mut pos, limits, false);

            // Negate score (searched from opponent's perspective)
            let score = -result.score;

            // Undo the move
            pos.undo_move(mv, undo_info);

            // Update split point's best score if this is better
            if split_point.update_best(score, mv) {
                // Beta cutoff found - stop searching
                break;
            }

            // Report nodes periodically
            self.report_nodes();
        }

        // Clean up when done
        split_point.remove_thread();
        self.shared_state.decrement_active_threads();
        self.flush_nodes();
    }

    /// Search the PV move at a split point (YBWC)
    #[cfg(feature = "ybwc")]
    pub fn search_pv_at_split_point(
        &mut self,
        split_point: &Arc<SplitPoint>,
        pv_move: Move,
    ) -> i32 {
        self.shared_state.increment_active_threads();

        // Clone position for PV search
        let mut pos = split_point.position.clone();

        // Apply the PV move
        let undo_info = pos.do_move(pv_move);

        // Search from the resulting position
        let limits = SearchLimits {
            depth: Some(split_point.depth.saturating_sub(1)),
            stop_flag: Some(self.shared_state.stop_flag.clone()),
            ..Default::default()
        };

        let result = self.searcher.search_with_options(&mut pos, limits, false);

        // Negate score (searched from opponent's perspective)
        let score = -result.score;

        // Undo the move
        pos.undo_move(pv_move, undo_info);

        // Update split point's best score
        split_point.update_best(score, pv_move);

        // Mark PV as searched (signals other threads can start)
        split_point.mark_pv_searched();

        self.shared_state.decrement_active_threads();
        self.flush_nodes();

        score
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
        assert_eq!(thread.state.load(Ordering::Relaxed), ThreadState::Searching as u8);
    }

    #[test]
    fn test_thread_state_transitions() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared_state = Arc::new(SharedSearchState::new(stop_flag));

        let thread = SearchThread::new(1, evaluator, tt, shared_state);

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
            let thread =
                SearchThread::new(thread_id, evaluator.clone(), tt.clone(), shared_state.clone());

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
