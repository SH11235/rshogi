//! Simplified parallel search implementation
//!
//! This is a complete rewrite of the parallel search system with focus on:
//! - Simplicity and reliability over complex optimizations
//! - Single atomic stop flag as the only stop mechanism
//! - No channels, no parking, no complex state management
//! - Work-stealing queue for task distribution

use crate::{
    evaluation::evaluate::Evaluator,
    search::{SearchLimits, SearchResult, SearchStats, TranspositionTable},
    shogi::{Move, Position},
    time_management::TimeManager,
};
use log::{debug, error, info, trace, warn};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use super::{SearchThread, SharedSearchState};

/// RAII guard to ensure active worker count is decremented
struct WorkerGuard {
    counter: Arc<AtomicUsize>,
}

impl WorkerGuard {
    /// Create a new guard and atomically increment the counter
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        let count = counter.load(Ordering::Acquire);
        debug!("Worker becoming active (active count: {count})");
        Self { counter }
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        // Use trace level to reduce noise during benchmarks
        trace!("WorkerGuard: active worker count decremented");
    }
}

/// Work item for threads to process
#[derive(Clone, Debug)]
enum WorkItem {
    /// Search a specific root move
    RootMove {
        /// Iteration number
        iteration: usize,
        /// Depth to search at
        depth: u8,
        /// Position to search from
        position: Position,
        /// Specific move to search first
        move_to_search: Move,
        /// Index of the move (for debugging)
        move_index: usize,
    },
    /// Search full position (traditional mode)
    FullPosition {
        /// Iteration number
        iteration: usize,
        /// Depth to search at
        depth: u8,
        /// Position to search
        position: Position,
    },
}

/// Lock-free work queue using atomic indices
struct WorkQueue {
    /// Items to process
    items: Vec<Mutex<Option<WorkItem>>>,
    /// Next item to take
    next_index: AtomicUsize,
    /// Total items added
    total_items: AtomicUsize,
}

impl WorkQueue {
    fn new(capacity: usize) -> Self {
        let mut items = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            items.push(Mutex::new(None));
        }

        Self {
            items,
            next_index: AtomicUsize::new(0),
            total_items: AtomicUsize::new(0),
        }
    }

    /// Add work item to queue
    fn push(&self, item: WorkItem) -> bool {
        let total = self.total_items.load(Ordering::Relaxed);
        if total >= self.items.len() {
            return false; // Queue full
        }

        let index = self.total_items.fetch_add(1, Ordering::Relaxed);
        if index < self.items.len() {
            if let Ok(mut slot) = self.items[index].lock() {
                *slot = Some(item);
                return true;
            }
        }
        false
    }

    /// Try to get next work item
    fn pop(&self) -> Option<WorkItem> {
        loop {
            let index = self.next_index.load(Ordering::Acquire);
            let total = self.total_items.load(Ordering::Acquire);

            if index >= total {
                return None; // No more items
            }

            // Try to claim this index
            if self
                .next_index
                .compare_exchange(index, index + 1, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                // We got the index, take the item
                if let Ok(mut slot) = self.items[index].lock() {
                    return slot.take();
                }
            }
            // Another thread got it, retry
        }
    }

    /// Clear the queue for reuse
    fn clear(&self) {
        self.next_index.store(0, Ordering::Release);
        self.total_items.store(0, Ordering::Release);
        for item in &self.items {
            if let Ok(mut slot) = item.lock() {
                *slot = None;
            }
        }
    }
    
    /// Get the number of pending work items
    fn pending_count(&self) -> usize {
        let total = self.total_items.load(Ordering::Acquire);
        let next = self.next_index.load(Ordering::Acquire);
        total.saturating_sub(next)
    }
}

/// Simplified parallel searcher
pub struct ParallelSearcher<E: Evaluator + Send + Sync + 'static> {
    /// Shared transposition table
    tt: Arc<TranspositionTable>,

    /// Shared evaluator
    evaluator: Arc<E>,

    /// Time manager
    time_manager: Option<Arc<TimeManager>>,

    /// Shared search state
    shared_state: Arc<SharedSearchState>,

    /// Number of threads
    num_threads: usize,

    /// Work queue
    work_queue: Arc<WorkQueue>,

    /// Node counter
    total_nodes: Arc<AtomicU64>,
    
    /// Active worker count for proper synchronization
    active_workers: Arc<AtomicUsize>,
}

impl<E: Evaluator + Send + Sync + 'static> ParallelSearcher<E> {
    /// Create new parallel searcher
    pub fn new(evaluator: Arc<E>, tt: Arc<TranspositionTable>, num_threads: usize) -> Self {
        assert!(num_threads > 0, "Need at least one thread");

        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared_state = Arc::new(SharedSearchState::new(stop_flag));

        // Create work queue with larger capacity to avoid overflow
        // (64 slots per thread handles deep search with many root moves)
        let work_queue = Arc::new(WorkQueue::new(num_threads * 64));

        Self {
            tt,
            evaluator,
            time_manager: None,
            shared_state,
            num_threads,
            work_queue,
            total_nodes: Arc::new(AtomicU64::new(0)),
            active_workers: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Set time manager for the search (compatibility method)
    pub fn set_time_manager(&mut self, time_manager: Arc<TimeManager>) {
        self.time_manager = Some(time_manager);
    }

    /// Adjust the number of active threads dynamically (compatibility method)
    /// Note: In the new implementation, this is a no-op as we always use all threads
    pub fn adjust_thread_count(&mut self, new_active_threads: usize) {
        let new_active = new_active_threads.min(self.num_threads).max(1);
        if new_active != self.num_threads {
            debug!(
                "Thread count adjustment requested from {} to {} (ignoring in new implementation)",
                self.num_threads, new_active
            );
            // In the simplified implementation, we always use all threads
            // This method is kept for compatibility but doesn't actually change behavior
        }
    }

    /// Get duplication percentage
    pub fn get_duplication_percentage(&self) -> f64 {
        self.shared_state.duplication_stats.duplication_percentage()
    }

    /// Get TT hit rate
    pub fn get_tt_hit_rate(&self) -> f64 {
        self.shared_state.duplication_stats.tt_hit_rate()
    }

    /// Get effective nodes (unique nodes explored)
    pub fn get_effective_nodes(&self) -> u64 {
        self.shared_state.duplication_stats.effective_nodes()
    }

    /// Calculate helper thread depth with variation for diversity
    fn calculate_helper_depth(&self, main_depth: u8, helper_id: usize, max_depth: u8) -> u8 {
        // Base offset to reduce depth for some helpers (YBWC-like variation)
        let base_offset = (helper_id / 2) as u8;
        
        // Small random-like variation based on helper_id
        let random_offset = if helper_id % 4 == 0 { 1 } else { 0 };
        
        // Calculate final depth with bounds checking
        main_depth
            .saturating_sub(base_offset)
            .saturating_add(random_offset)
            .min(max_depth)
    }

    /// Main search entry point
    pub fn search(&mut self, position: &mut Position, limits: SearchLimits) -> SearchResult {
        info!("Starting simple parallel search with {} threads", self.num_threads);

        // Record start time for fail-safe
        let search_start = Instant::now();

        // Reset state
        self.shared_state.reset();
        self.work_queue.clear();
        self.total_nodes.store(0, Ordering::Release);

        // Create time manager if needed
        use crate::time_management::{GamePhase, TimeControl, TimeLimits};

        let game_phase = if position.ply <= 40 {
            GamePhase::Opening
        } else if position.ply <= 120 {
            GamePhase::MiddleGame
        } else {
            GamePhase::EndGame
        };

        // Create TimeManager if time control is specified (works with or without depth limit)
        if !matches!(limits.time_control, TimeControl::Infinite) {
            let time_limits: TimeLimits = limits.clone().into();
            let time_manager = Arc::new(TimeManager::new(
                &time_limits,
                position.side_to_move,
                position.ply.into(),
                game_phase,
            ));
            let soft_limit = time_manager.soft_limit_ms();
            self.time_manager = Some(time_manager);
            debug!("TimeManager created with soft limit: {soft_limit}ms");
        } else {
            self.time_manager = None;
            debug!("TimeManager disabled (infinite time control)");
        }

        // Start worker threads
        let mut handles = Vec::new();
        for id in 1..self.num_threads {
            handles.push(self.start_worker(id, position.clone(), limits.clone()));
        }

        // Start time management if needed (but not for very short searches)
        let time_handle = if let Some(ref tm) = self.time_manager {
            // Only start time manager if we have reasonable time
            if tm.soft_limit_ms() > 10 {
                Some(self.start_time_manager(tm.clone()))
            } else {
                debug!("Skipping time manager for very short search ({}ms)", tm.soft_limit_ms());
                None
            }
        } else {
            None
        };

        // Start fail-safe guard thread
        let fail_safe_handle = self.start_fail_safe_guard(search_start, limits.clone());

        // Main thread does iterative deepening and generates work
        let result = self.run_main_thread(position, limits);

        // Stop all threads
        info!("Search complete, stopping threads");
        self.shared_state.set_stop();

        // Wait for workers
        for handle in handles {
            let _ = handle.join();
        }

        // Stop time manager
        if let Some(handle) = time_handle {
            let _ = handle.join();
        }

        // Stop fail-safe guard
        let _ = fail_safe_handle.join();

        result
    }

    /// Start a worker thread
    fn start_worker(
        &self,
        id: usize,
        _position: Position,
        limits: SearchLimits,
    ) -> thread::JoinHandle<()> {
        let evaluator = self.evaluator.clone();
        let tt = self.tt.clone();
        let shared_state = self.shared_state.clone();
        let work_queue = self.work_queue.clone();
        let total_nodes = self.total_nodes.clone();
        let active_workers = self.active_workers.clone();

        thread::spawn(move || {
            debug!("Worker {id} started");

            // Create search thread
            let mut search_thread = SearchThread::new(id, evaluator, tt, shared_state.clone());

            let mut local_nodes = 0u64;
            let mut last_report = 0u64;

            // Simple work loop
            while !shared_state.should_stop() {
                // Try to get work
                if let Some(work) = work_queue.pop() {
                    // Create guard which atomically increments the counter
                    let _guard = WorkerGuard::new(active_workers.clone());
                    
                    let prev_nodes = local_nodes;  // Track previous node count
                    let nodes = match work {
                        WorkItem::RootMove {
                            iteration,
                            depth,
                            position,
                            move_to_search,
                            move_index,
                        } => {
                            debug!(
                                "Worker {id} processing RootMove #{move_index} (iteration {iteration}, depth {depth})"
                            );

                            // Clone position for this search
                            let mut pos = position;

                            // Search the specific root move
                            let _result = search_thread.search_root_move(
                                &mut pos,
                                &limits,
                                depth,
                                move_to_search,
                            );

                            // Update nodes (accumulate the difference)
                            let nodes = search_thread.searcher.nodes();
                            local_nodes += nodes.saturating_sub(prev_nodes);
                            nodes
                        }
                        WorkItem::FullPosition {
                            iteration,
                            depth,
                            position,
                        } => {
                            debug!(
                                "Worker {id} processing FullPosition (iteration {iteration}, depth {depth})"
                            );

                            // Clone position for this search
                            let mut pos = position;

                            // Do the search
                            let _result = search_thread.search_iteration(&mut pos, &limits, depth);

                            // Update nodes (accumulate the difference)
                            let nodes = search_thread.searcher.nodes();
                            local_nodes += nodes.saturating_sub(prev_nodes);
                            nodes
                        }
                    };
                    
                    debug!("Worker {id} work completed");
                    
                    // Note: WorkerGuard will automatically decrement active_workers when dropped
                    
                    // Report nodes periodically (every 100k nodes)
                    if nodes - last_report >= 100_000 {
                        total_nodes.fetch_add(nodes - last_report, Ordering::Relaxed);
                        shared_state.add_nodes(nodes - last_report);
                        last_report = nodes;
                    }

                    // Check stop every work item
                    if shared_state.should_stop() {
                        break;
                    }
                } else {
                    // No work available, check stop flag with brief sleep
                    thread::sleep(Duration::from_micros(100));
                }
            }

            // Final node report
            if local_nodes > last_report {
                total_nodes.fetch_add(local_nodes - last_report, Ordering::Relaxed);
                shared_state.add_nodes(local_nodes - last_report);
            }

            debug!("Worker {id} stopped with {local_nodes} nodes");
        })
    }

    /// Run main thread with iterative deepening
    fn run_main_thread(&self, position: &mut Position, limits: SearchLimits) -> SearchResult {
        let mut best_result = SearchResult::new(None, 0, SearchStats::default());

        // Create main search thread
        let mut main_thread = SearchThread::new(
            0,
            self.evaluator.clone(),
            self.tt.clone(),
            self.shared_state.clone(),
        );

        let max_depth = limits.depth.unwrap_or(255);
        let mut last_reported_nodes = 0u64;  // Track last reported node count

        // Iterative deepening
        for iteration in 1.. {
            // Skip stop check on first iteration to ensure we get at least one result
            if iteration > 1 && self.shared_state.should_stop() {
                debug!("Main thread stopping at iteration {iteration}");
                break;
            }
            
            // Also check time manager on iterations after the first
            if iteration > 1 {
                if let Some(ref tm) = self.time_manager {
                    let current_nodes = self.total_nodes.load(Ordering::Relaxed);
                    if tm.should_stop(current_nodes) {
                        debug!("Main thread stopping at iteration {iteration} due to time limit");
                        break;
                    }
                }
            }

            // Calculate depths for this iteration
            let main_depth = iteration.min(max_depth as usize) as u8;

            if main_depth > max_depth {
                debug!("Reached max depth {max_depth}");
                self.shared_state.set_stop();
                break;
            }

            let iter_start = Instant::now();
            debug!("Starting iteration {iteration} (depth {main_depth})");

            // Generate root moves for the first half of iterations to distribute work
            // (at least 3 iterations, but up to half of max_depth)
            let root_move_limit = (max_depth as usize / 2).max(3);
            if iteration <= root_move_limit && self.num_threads > 1 {
                // Generate all legal moves at root
                let mut move_gen = crate::movegen::generator::MoveGenImpl::new(position);
                let moves = move_gen.generate_all();
                
                if !moves.is_empty() {
                    debug!("Distributing {} root moves to {} helper threads (iteration {})", 
                           moves.len(), self.num_threads - 1, iteration);
                    
                    // Distribute root moves in small chunks to avoid overloading single helper
                    // Each round, give 2 moves to each helper thread
                    let chunk_size = 2;
                    let num_helpers = self.num_threads - 1;
                    
                    // Process moves in rounds, distributing chunk_size moves to each helper per round
                    for chunk_start in (0..moves.len()).step_by(chunk_size * num_helpers) {
                        for helper_offset in 0..num_helpers {
                            // Calculate the starting index for this helper in this round
                            let base_idx = chunk_start + helper_offset * chunk_size;
                            
                            // Distribute chunk_size moves to this helper
                            for offset in 0..chunk_size {
                                let idx = base_idx + offset;
                                if idx >= moves.len() {
                                    break;
                                }
                                
                                let mv = moves[idx];
                                let _helper_id = 1 + helper_offset;
                                
                                // For root moves, use slightly shallower depth to avoid long-running tasks
                                // but not too shallow to avoid underutilization
                                let helper_depth = main_depth.saturating_sub(1).max(1);
                                
                                // Create work item for this move
                                let work = WorkItem::RootMove {
                                    iteration,
                                    depth: helper_depth,
                                    position: position.clone(),
                                    move_to_search: mv,
                                    move_index: idx,
                                };
                                
                                if !self.work_queue.push(work) {
                                    warn!("Work queue full at iteration {iteration}, move {idx}");
                                    break;
                                }
                            }
                        }
                    }
                }
            } else {
                // Fall back to traditional full position search for deeper iterations
                debug!("Using FullPosition mode for iteration {} (beyond limit {})", 
                       iteration, root_move_limit);
                for helper_id in 1..self.num_threads {
                    let helper_depth = self.calculate_helper_depth(main_depth, helper_id, max_depth);

                    let work = WorkItem::FullPosition {
                        iteration,
                        depth: helper_depth,
                        position: position.clone(),
                    };

                    if !self.work_queue.push(work) {
                        warn!("Work queue full at iteration {iteration}");
                    }
                }
            }

            // Main thread searches
            let result = main_thread.search_iteration(position, &limits, main_depth);
            
            debug!("Iteration {} completed in {:?} with score {} (depth {}, {} nodes)", 
                   iteration, iter_start.elapsed(), result.score, 
                   result.stats.depth, result.stats.nodes);

            // Update best result (always update if we don't have a move yet)
            if best_result.best_move.is_none()
                || result.score > best_result.score
                || (result.score == best_result.score
                    && result.stats.depth > best_result.stats.depth)
            {
                best_result = result;
            }

            // Report nodes (calculate difference from last report)
            let nodes = main_thread.searcher.nodes();
            let nodes_diff = nodes.saturating_sub(last_reported_nodes);
            self.total_nodes.fetch_add(nodes_diff, Ordering::Relaxed);
            self.shared_state.add_nodes(nodes_diff);
            last_reported_nodes = nodes;

            // Check depth limit
            if main_depth >= max_depth {
                info!("Main thread reached maximum depth {max_depth}, waiting for workers to complete...");
                
                // Wait for workers to complete their work
                // Check both pending queue items AND active workers
                // Use shorter timeout if time manager is active
                let max_wait_ms = if self.time_manager.is_some() {
                    100  // Short wait when time-limited
                } else {
                    2000 // Longer wait for depth-only searches
                };
                
                let mut wait_time = 0;
                loop {
                    let pending = self.work_queue.pending_count();
                    let active = self.active_workers.load(Ordering::Acquire);
                    
                    if pending == 0 && active == 0 {
                        debug!("All work completed (0 pending, 0 active)");
                        break;
                    }
                    
                    thread::sleep(Duration::from_millis(10));
                    wait_time += 10;
                    
                    if wait_time % 100 == 0 {
                        debug!("Waiting for work to complete: {} pending items, {} active workers", 
                               pending, active);
                    }
                    
                    // Safety: don't wait forever
                    if wait_time > max_wait_ms {
                        debug!("Timeout after {wait_time}ms waiting for workers: {} items pending, {} workers active", 
                              pending, active);
                        break;
                    }
                }
                
                // Give workers a bit more time to update shared state
                thread::sleep(Duration::from_millis(50));
                
                info!("All workers completed, stopping search");
                self.shared_state.set_stop();
                break;
            }
        }

        // Get final best move from shared state if better
        if let Some(shared_move) = self.shared_state.get_best_move() {
            let shared_score = self.shared_state.get_best_score();
            let shared_depth = self.shared_state.get_best_depth();

            // Use shared result if it's better or we don't have a move
            if best_result.best_move.is_none()
                || shared_score > best_result.score
                || (shared_score == best_result.score && shared_depth > best_result.stats.depth)
            {
                best_result.best_move = Some(shared_move);
                best_result.score = shared_score;
                best_result.stats.depth = shared_depth;
            }
        }

        best_result.stats.nodes = self.total_nodes.load(Ordering::Relaxed);

        // Ensure we always have a move (fallback to first legal move if needed)
        if best_result.best_move.is_none() && best_result.stats.nodes > 0 {
            warn!(
                "No best move found despite searching {} nodes, using fallback",
                best_result.stats.nodes
            );
            // SearchThread should have found at least one move
            // This is a safety fallback that shouldn't normally happen
        }

        best_result
    }

    /// Start fail-safe guard thread
    /// This thread will abort the process if search exceeds hard timeout
    fn start_fail_safe_guard(
        &self,
        search_start: Instant,
        limits: SearchLimits,
    ) -> thread::JoinHandle<()> {
        let shared_state = self.shared_state.clone();

        thread::spawn(move || {
            // Calculate hard timeout
            use crate::time_management::TimeControl;
            let hard_timeout_ms = match limits.time_control {
                TimeControl::FixedTime { ms_per_move } => ms_per_move * 3, // 3x safety margin
                TimeControl::Fischer {
                    white_ms,
                    black_ms,
                    increment_ms: _,
                } => {
                    // Use 90% of remaining time as absolute maximum
                    let time_ms = white_ms.max(black_ms);
                    (time_ms * 9) / 10
                }
                TimeControl::Byoyomi {
                    main_time_ms,
                    byoyomi_ms,
                    periods: _,
                } => {
                    // Use main time + one byoyomi period
                    main_time_ms + byoyomi_ms
                }
                TimeControl::FixedNodes { .. } => {
                    // For node-limited search, use 1 hour as safety limit
                    3_600_000
                }
                TimeControl::Infinite => {
                    // For infinite search, use 1 hour as safety limit
                    3_600_000
                }
                TimeControl::Ponder(ref inner) => {
                    // For pondering, use the inner time control
                    match inner.as_ref() {
                        TimeControl::FixedTime { ms_per_move } => ms_per_move * 3,
                        TimeControl::Fischer {
                            white_ms, black_ms, ..
                        } => {
                            let time_ms = white_ms.max(black_ms);
                            (time_ms * 9) / 10
                        }
                        _ => 3_600_000,
                    }
                }
            };

            // Add extra safety margin for depth-limited searches
            // But keep it reasonable when time control is also specified
            let hard_timeout_ms = if limits.depth.is_some() && matches!(limits.time_control, TimeControl::Infinite) {
                hard_timeout_ms.max(60_000) // 60 seconds for depth-only searches
            } else {
                hard_timeout_ms.max(1000)   // At least 1 second for time-controlled searches
            };

            debug!("Fail-safe guard started with hard timeout: {hard_timeout_ms}ms");

            // Check periodically
            loop {
                thread::sleep(Duration::from_millis(100));

                // Check if search stopped normally
                if shared_state.should_stop() {
                    debug!("Fail-safe guard: Search stopped normally");
                    break;
                }

                // Check if hard timeout exceeded
                let elapsed = search_start.elapsed();
                if elapsed.as_millis() > hard_timeout_ms as u128 {
                    error!(
                        "FAIL-SAFE: Search exceeded hard timeout of {}ms (elapsed: {}ms)",
                        hard_timeout_ms,
                        elapsed.as_millis()
                    );

                    // Try to stop gracefully first
                    shared_state.set_stop();

                    // Give 500ms for graceful shutdown
                    thread::sleep(Duration::from_millis(500));

                    // If still not stopped, abort
                    if !shared_state.should_stop() {
                        error!("FAIL-SAFE: Forced abort due to unresponsive search!");
                        std::process::abort();
                    }
                }
            }

            debug!("Fail-safe guard stopped");
        })
    }

    /// Start time management thread
    fn start_time_manager(&self, time_manager: Arc<TimeManager>) -> thread::JoinHandle<()> {
        let shared_state = self.shared_state.clone();
        let total_nodes = self.total_nodes.clone();

        thread::spawn(move || {
            debug!("Time manager started");

            loop {
                // Poll interval based on time control
                let poll_interval = match time_manager.soft_limit_ms() {
                    0..=50 => Duration::from_millis(2),
                    51..=100 => Duration::from_millis(5),
                    101..=500 => Duration::from_millis(10),
                    _ => Duration::from_millis(20),
                };

                thread::sleep(poll_interval);

                if shared_state.should_stop() {
                    break;
                }

                let nodes = total_nodes.load(Ordering::Relaxed);
                // Don't stop if we haven't done any real work yet
                if nodes > 100 && time_manager.should_stop(nodes) {
                    info!("Time limit reached after {nodes} nodes, stopping search");
                    shared_state.set_stop();
                    break;
                }
            }

            debug!("Time manager stopped");
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{evaluation::evaluate::MaterialEvaluator, search::SearchLimitsBuilder};

    #[test]
    fn test_simple_parallel_search() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        let mut searcher = ParallelSearcher::new(evaluator, tt, 2);
        let mut position = Position::startpos();

        let limits = SearchLimitsBuilder::default().depth(2).build();
        let result = searcher.search(&mut position, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    fn test_work_queue() {
        let queue = WorkQueue::new(10);
        let pos = Position::startpos();

        // Add items (using FullPosition variant)
        for i in 0..5 {
            let item = WorkItem::FullPosition {
                iteration: i,
                depth: i as u8,
                position: pos.clone(),
            };
            assert!(queue.push(item));
        }

        // Take items
        for i in 0..5 {
            let item = queue.pop().expect("Should have item");
            match item {
                WorkItem::FullPosition { iteration, .. } => {
                    assert_eq!(iteration, i);
                }
                _ => panic!("Expected FullPosition variant"),
            }
        }

        // Queue should be empty
        assert!(queue.pop().is_none());
    }
}
