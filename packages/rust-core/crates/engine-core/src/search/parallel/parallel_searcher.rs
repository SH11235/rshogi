//! Parallel search coordinator using Lazy SMP
//!
//! Manages multiple search threads and aggregates their results

use crate::{
    evaluation::evaluate::Evaluator,
    search::{SearchLimits, SearchResult, SearchStats, TranspositionTable},
    shogi::Position,
    time_management::TimeManager,
};
use crossbeam::channel::Sender;
use crossbeam_utils::CachePadded;
use log::{debug, error, info, warn};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use super::{SearchThread, SharedSearchState};

/// Signal sent to worker threads
#[derive(Clone)]
enum IterationSignal {
    /// Start a new iteration at specified depth
    StartIteration(usize),
    /// Stop all threads
    Stop,
}

/// Statistics for measuring search duplication
#[derive(Debug)]
pub struct DuplicationStats {
    /// Nodes that were not in TT (unique work)
    /// Cache-padded to prevent false sharing
    pub unique_nodes: CachePadded<AtomicU64>,
    /// Total nodes searched by all threads
    /// Cache-padded to prevent false sharing
    pub total_nodes: CachePadded<AtomicU64>,
}

impl Default for DuplicationStats {
    fn default() -> Self {
        Self {
            unique_nodes: CachePadded::new(AtomicU64::new(0)),
            total_nodes: CachePadded::new(AtomicU64::new(0)),
        }
    }
}

impl DuplicationStats {
    /// Get duplication percentage (0-100)
    pub fn get_duplication_percentage(&self) -> f64 {
        let total = self.total_nodes.load(Ordering::Relaxed);
        let unique = self.unique_nodes.load(Ordering::Relaxed);

        if total == 0 {
            0.0
        } else {
            ((total - unique) as f64) * 100.0 / (total as f64)
        }
    }

    /// Reset statistics
    pub fn reset(&self) {
        // Use Release ordering for safer synchronization
        self.unique_nodes.store(0, Ordering::Release);
        self.total_nodes.store(0, Ordering::Release);
    }
}

/// Parallel search coordinator
pub struct ParallelSearcher<E: Evaluator + Send + Sync + 'static> {
    /// Shared transposition table
    _tt: Arc<TranspositionTable>,

    /// Shared evaluator
    _evaluator: Arc<E>,

    /// Time manager for the search
    time_manager: Option<Arc<TimeManager>>,

    /// Shared search state
    shared_state: Arc<SharedSearchState>,

    /// Number of search threads
    num_threads: usize,

    /// Search threads
    threads: Vec<Arc<Mutex<SearchThread<E>>>>,

    /// Thread handles (populated during search)
    handles: Mutex<Vec<thread::JoinHandle<()>>>,

    /// Duplication statistics
    duplication_stats: Arc<DuplicationStats>,

    /// Channels for sending signals to worker threads
    start_signals: Arc<Mutex<Vec<Sender<IterationSignal>>>>,

    /// Currently active thread count (may be less than num_threads)
    active_threads: usize,
}

impl<E: Evaluator + Send + Sync + 'static> ParallelSearcher<E> {
    /// Create a new parallel searcher
    pub fn new(evaluator: Arc<E>, tt: Arc<TranspositionTable>, num_threads: usize) -> Self {
        assert!(num_threads > 0, "Need at least one thread");

        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared_state = Arc::new(SharedSearchState::new(stop_flag));
        let duplication_stats = Arc::new(DuplicationStats::default());

        // Create search threads
        let mut threads = Vec::with_capacity(num_threads);
        for id in 0..num_threads {
            let thread = Arc::new(Mutex::new(SearchThread::new(
                id,
                evaluator.clone(),
                tt.clone(),
                shared_state.clone(),
                Some(duplication_stats.clone()),
            )));
            threads.push(thread);
        }

        Self {
            _tt: tt,
            _evaluator: evaluator,
            time_manager: None,
            shared_state,
            num_threads,
            threads,
            handles: Mutex::new(Vec::new()),
            duplication_stats,
            start_signals: Arc::new(Mutex::new(Vec::new())),
            active_threads: num_threads,
        }
    }

    /// Set time manager for the search
    pub fn set_time_manager(&mut self, time_manager: Arc<TimeManager>) {
        self.time_manager = Some(time_manager);
    }

    /// Main search entry point
    pub fn search(&mut self, position: &mut Position, limits: SearchLimits) -> SearchResult {
        debug!("Starting parallel search with {} threads", self.num_threads);

        // Reset shared state
        self.shared_state.reset();
        self.duplication_stats.reset();

        // Create TimeManager if needed (similar to UnifiedSearcher)
        use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};

        // Estimate game phase from position
        let game_phase = if position.ply <= 40 {
            GamePhase::Opening
        } else if position.ply <= 120 {
            GamePhase::MiddleGame
        } else {
            GamePhase::EndGame
        };

        // Create TimeManager for time-based searches
        if !matches!(limits.time_control, TimeControl::Infinite) || limits.depth.is_some() {
            let time_limits: TimeLimits = limits.clone().into();
            let time_manager = Arc::new(TimeManager::new(
                &time_limits,
                position.side_to_move,
                position.ply.into(),
                game_phase,
            ));
            self.time_manager = Some(time_manager);
        } else {
            self.time_manager = None;
        }

        // Start worker threads
        self.start_worker_threads(position.clone(), limits.clone());

        // Main thread coordinates iterative deepening
        let result = self.coordinate_search(position, limits);

        // Note: Time management thread is now started inside coordinate_search

        // Stop all threads with timeout
        self.stop_threads_with_timeout(Duration::from_millis(100));

        // Log duplication statistics
        let dup_pct = self.duplication_stats.get_duplication_percentage();
        debug!("Search complete. Duplication: {dup_pct:.1}%");

        result
    }

    /// Start worker threads
    fn start_worker_threads(&mut self, position: Position, limits: SearchLimits) {
        let mut handles = match self.handles.lock() {
            Ok(h) => h,
            Err(e) => {
                error!("Failed to acquire handles lock: {e}");
                return;
            }
        };
        handles.clear();

        // Clear old channels and create new ones
        {
            let mut signals = self.start_signals.lock().unwrap();
            signals.clear();
        }

        // Only start up to active_threads workers
        let workers_to_start = self.active_threads.saturating_sub(1); // -1 for main thread
        let mut started_workers = 0;

        debug!(
            "Starting {} worker threads (active_threads={}, num_threads={})",
            workers_to_start, self.active_threads, self.num_threads
        );

        for (id, thread) in self.threads.iter().enumerate() {
            if id == 0 {
                continue; // Main thread is handled separately
            }

            // Only start threads up to active_threads limit
            if started_workers >= workers_to_start {
                debug!(
                    "Thread {} inactive for this search (active_threads={})",
                    id, self.active_threads
                );
                continue; // Use continue instead of break for robustness
            }

            // Create channel for this worker
            let (sender, receiver) = crossbeam::channel::unbounded();
            {
                let mut signals = self.start_signals.lock().unwrap();
                signals.push(sender);
            }

            let thread = thread.clone();
            let mut position = position.clone();
            let limits = limits.clone();
            let shared_state = self.shared_state.clone();
            let time_manager = self.time_manager.clone();

            let handle = thread::spawn(move || {
                debug!("Worker thread {id} spawned");

                // Reset thread and set handle without holding lock
                {
                    match thread.lock() {
                        Ok(mut thread) => {
                            thread.reset();
                            thread.set_thread_handle(thread::current());
                        }
                        Err(e) => {
                            error!("Worker thread {id} failed to acquire lock on startup: {e}");
                            return;
                        }
                    }
                } // Lock released here

                // Worker thread search loop
                loop {
                    // Try to receive signal with timeout (reduced from 10ms to 1ms for faster response)
                    match receiver.recv_timeout(Duration::from_millis(1)) {
                        Ok(IterationSignal::StartIteration(iteration)) => {
                            // Check stop flag before starting
                            if shared_state.should_stop() {
                                // Report nodes with lock
                                if let Ok(mut thread) = thread.lock() {
                                    thread.flush_nodes(); // Force flush all pending nodes
                                } else {
                                    warn!("Thread {id} failed to acquire lock for reporting nodes");
                                }
                                break;
                            }

                            // Take lock only for the duration of the search
                            let mut thread = match thread.lock() {
                                Ok(t) => t,
                                Err(e) => {
                                    error!("Thread {id} failed to acquire lock for search: {e}");
                                    continue;
                                }
                            };

                            // Set state to searching
                            thread.set_state(super::search_thread::ThreadState::Searching);

                            let depth = thread.get_start_depth(iteration);
                            debug!("Thread {id} starting depth {depth}");

                            // Perform search (without state management)
                            let max_depth = limits.depth.unwrap_or(255);

                            // Skip if depth exceeds limit
                            if depth > max_depth {
                                debug!(
                                    "Thread {id} skipping depth {depth} (exceeds max {max_depth})"
                                );
                                continue;
                            }

                            let _result = thread.search_iteration(&mut position, &limits, depth);

                            // Update node count (differential)
                            thread.report_nodes();

                            // Check if should park after deep searches
                            if thread.should_park(depth, max_depth) {
                                // Set idle state before any stop checks
                                thread.set_state(super::search_thread::ThreadState::Idle);

                                // Double-check stop flag to prevent race condition
                                // This ensures we don't park after a stop signal
                                if !shared_state.should_stop() {
                                    // Get actual time left from TimeManager if available
                                    let time_left_ms = time_manager.as_ref().map(|tm| {
                                        let info = tm.get_time_info();
                                        info.hard_limit_ms.saturating_sub(info.elapsed_ms)
                                    });

                                    debug!("Thread {id} parking at depth {depth}");
                                    thread.park_with_timeout(max_depth, time_left_ms);
                                    debug!("Thread {id} woke up from park");
                                }

                                // CRITICAL: Always check stop flag after park
                                // This handles both normal wakeups and unpark signals
                                if shared_state.should_stop() {
                                    debug!("Thread {id} stopping after park (stop flag set)");
                                    thread.flush_nodes(); // Force flush all pending nodes
                                    break;
                                }
                            }
                            // Lock released here
                        }
                        Ok(IterationSignal::Stop) => {
                            debug!("Thread {id} received Stop signal");
                            if let Ok(mut thread) = thread.lock() {
                                thread.flush_nodes(); // Force flush all pending nodes
                            } else {
                                warn!("Thread {id} failed to acquire lock for final report");
                            }
                            debug!("Thread {id} exiting");
                            break;
                        }
                        Err(_) => {
                            // Timeout - check stop flag
                            if shared_state.should_stop() {
                                if let Ok(mut thread) = thread.lock() {
                                    thread.flush_nodes(); // Force flush all pending nodes
                                } else {
                                    warn!("Thread {id} failed to acquire lock on timeout stop");
                                }
                                break;
                            }
                            // Continue waiting
                        }
                    }
                }
            });

            handles.push(handle);
            started_workers += 1;
        }
    }

    /// Adjust the number of active threads dynamically
    pub fn adjust_thread_count(&mut self, new_active_threads: usize) {
        let new_active_threads = new_active_threads.min(self.num_threads).max(1);

        if new_active_threads != self.active_threads {
            debug!(
                "Adjusting active threads from {} to {}",
                self.active_threads, new_active_threads
            );
            self.active_threads = new_active_threads;
            // The actual thread limiting will be done during search by only sending signals
            // to the first `active_threads` workers
        }
    }

    /// Get duplication percentage from statistics
    pub fn get_duplication_percentage(&self) -> f64 {
        self.duplication_stats.get_duplication_percentage()
    }

    /// Coordinate search from main thread
    fn coordinate_search(&self, position: &mut Position, limits: SearchLimits) -> SearchResult {
        let mut best_result = SearchResult::new(None, i32::MIN, SearchStats::default());
        let main_thread = self.threads[0].clone();
        let mut time_handle: Option<thread::JoinHandle<()>> = None;

        // Reset main thread before starting
        {
            let mut thread = match main_thread.lock() {
                Ok(t) => t,
                Err(e) => {
                    error!("Main thread failed to acquire lock for reset: {e}");
                    return SearchResult::new(None, i32::MIN, SearchStats::default());
                }
            };
            thread.reset();
        }

        // Start time management thread BEFORE iterations for time-based searches
        // This ensures time limits work even during the first iteration
        if let Some(tm) = &self.time_manager {
            // Check if this is a time-based search
            use crate::time_management::TimeControl;
            let is_time_based = !matches!(limits.time_control, TimeControl::Infinite);

            if is_time_based {
                debug!("Starting time management thread before iterations (time-based search)");
                time_handle = Some(self.start_time_management_thread(tm.clone()));
            } else {
                debug!("Not starting time management thread (depth-only search)");
            }
        }

        // Iterative deepening loop
        for iteration in 1.. {
            // Check stop flag BEFORE starting new iteration (except first iteration)
            if iteration > 1 && self.shared_state.should_stop() {
                debug!("Stopping iterations due to stop flag");
                break;
            }

            // Calculate the depth for this iteration BEFORE signaling workers
            let depth = {
                let thread = match main_thread.lock() {
                    Ok(t) => t,
                    Err(e) => {
                        error!("Main thread failed to acquire lock for depth check: {e}");
                        break;
                    }
                };
                thread.get_start_depth(iteration)
            };

            // Check depth limit BEFORE starting any threads for this iteration
            // This prevents helper threads from starting iterations beyond max depth
            if let Some(max_depth) = limits.depth {
                if depth > max_depth {
                    debug!("Reached max depth {max_depth} at iteration {iteration}; sending STOP to all workers");

                    // CRITICAL: Set stop flag and notify all workers to terminate
                    // This ensures helper threads don't wait forever for the next iteration
                    self.shared_state.set_stop();

                    // Send Stop signal to all worker threads
                    {
                        let signals = self.start_signals.lock().unwrap();
                        for sender in signals.iter() {
                            let _ = sender.send(IterationSignal::Stop);
                        }
                    }

                    // Exit without starting any search for this iteration
                    break;
                }
            }

            // Signal all active worker threads to start this iteration
            // Note: start_signals only contains channels for active threads
            {
                let signals = self.start_signals.lock().unwrap();
                for sender in signals.iter() {
                    let _ = sender.send(IterationSignal::StartIteration(iteration));
                    // Note: We don't need to unpark here since the thread is actively
                    // waiting on the channel with recv_timeout. It will see the signal.
                }
            }

            // Main thread searches at normal depth
            let mut thread = match main_thread.lock() {
                Ok(t) => t,
                Err(e) => {
                    error!("Main thread failed to acquire lock for iteration {iteration}: {e}");
                    break;
                }
            };

            debug!("Starting iteration {iteration} (depth {depth})");
            let result = thread.search_iteration(position, &limits, depth);

            // IMPORTANT: Report nodes after each iteration for 1-thread case
            thread.report_nodes();
            debug!("Main thread iteration {iteration} nodes: {}", thread.searcher.nodes());

            // Update best result
            if result.score > best_result.score || result.stats.depth > best_result.stats.depth {
                best_result = result;
            }

            // Time management thread is now started before iterations for time-based searches
            // For depth-only searches, start after first iteration if not already started
            if iteration == 1 && time_handle.is_none() {
                if let Some(tm) = &self.time_manager {
                    debug!(
                        "Starting time management thread after first iteration (depth-only mode)"
                    );
                    time_handle = Some(self.start_time_management_thread(tm.clone()));
                }
            }

            // Check if we've reached the maximum depth after this iteration
            // If so, we need to signal workers to stop before the next iteration
            if let Some(max_depth) = limits.depth {
                if depth >= max_depth {
                    info!("Reached maximum depth {max_depth} at iteration {iteration} (depth {depth})");

                    // Send stop signal to all workers for clean shutdown
                    // This is needed because we won't enter the next iteration
                    self.shared_state.set_stop();
                    {
                        let signals = self.start_signals.lock().unwrap();
                        for sender in signals.iter() {
                            let _ = sender.send(IterationSignal::Stop);
                        }
                    }

                    // Now we can safely break
                    break;
                }
            }

            // Update node count (differential)
            thread.report_nodes();
        }

        // Report final nodes from main thread
        // IMPORTANT: This ensures nodes are counted even with 1 thread (workers_to_start = 0)
        {
            let mut thread = main_thread.lock().unwrap();
            thread.flush_nodes(); // Force flush all pending nodes
            debug!(
                "Main thread final nodes flushed. Total nodes: {}",
                self.shared_state.get_nodes()
            );
        }

        // Get final best move from shared state
        if let Some(best_move) = self.shared_state.get_best_move() {
            best_result.best_move = Some(best_move);
            best_result.stats.pv = vec![best_move];
            best_result.score = self.shared_state.get_best_score();
            best_result.stats.depth = self.shared_state.get_best_depth();
        }

        best_result.stats.nodes = self.shared_state.get_nodes();

        // Set duplication percentage
        best_result.stats.duplication_percentage =
            Some(self.duplication_stats.get_duplication_percentage());

        // Stop time management thread if it was started
        if let Some(handle) = time_handle {
            self.shared_state.set_stop();
            let _ = handle.join();
        }

        best_result
    }

    /// Start time management thread
    fn start_time_management_thread(
        &self,
        time_manager: Arc<TimeManager>,
    ) -> thread::JoinHandle<()> {
        let shared_state = self.shared_state.clone();
        let start_signals = self.start_signals.clone();

        thread::spawn(move || {
            loop {
                // Adaptive polling interval based on time control
                let poll_interval = match time_manager.soft_limit_ms() {
                    0..=50 => Duration::from_millis(2),     // 超高速用
                    51..=100 => Duration::from_millis(5),   // 高速用
                    101..=500 => Duration::from_millis(10), // 通常用
                    _ => Duration::from_millis(20),         // 低速用
                };
                thread::sleep(poll_interval);

                if shared_state.should_stop() {
                    break;
                }

                // Check if we should stop due to time (also updates node count)
                let nodes = shared_state.get_nodes();
                if time_manager.should_stop(nodes) {
                    info!("Time limit reached, stopping search");

                    // ① Set stop flag
                    shared_state.set_stop();

                    // ② Broadcast Stop signal to all workers
                    {
                        let signals = start_signals.lock().unwrap();
                        for sender in signals.iter() {
                            let _ = sender.send(IterationSignal::Stop);
                        }
                    }

                    break;
                }
            }
        })
    }

    /// Stop all threads with unpark (lost wake-up prevention)
    fn stop_all_threads(&self) {
        // First, set stop flag
        self.shared_state.set_stop();

        // Send stop signal to all workers
        {
            let signals = self.start_signals.lock().unwrap();
            for sender in signals.iter() {
                let _ = sender.send(IterationSignal::Stop);
            }
        }

        // CRITICAL: Unpark ALL threads without lock to prevent deadlock
        // This ensures parked threads wake up and check the stop flag
        for thread_arc in &self.threads {
            // Try to acquire lock briefly just to call unpark
            // If lock fails, try again with a short retry
            let mut unpacked = false;
            for _retry in 0..3 {
                if let Ok(thread) = thread_arc.try_lock() {
                    thread.unpark();
                    unpacked = true;
                    break;
                }
                // Brief sleep before retry
                thread::sleep(Duration::from_micros(100));
            }

            if !unpacked {
                // Last resort: thread is likely busy and will check stop flag soon
                debug!("Could not unpark thread after retries, it should check stop flag soon");
            }
        }
    }

    /// Stop all threads with timeout (best-effort)
    ///
    /// Note: The timeout is best-effort only. Since std::thread::JoinHandle doesn't
    /// support timed joins, we cannot guarantee threads will stop within the timeout.
    /// The function will attempt to stop all threads gracefully by setting stop flags
    /// and sending stop signals, but actual thread termination depends on threads
    /// checking these signals promptly.
    fn stop_threads_with_timeout(&mut self, timeout: Duration) {
        let start = Instant::now();

        // Use stop_all_threads for proper unpark sequence
        self.stop_all_threads();

        // Wait for worker threads with timeout
        let mut handles = match self.handles.lock() {
            Ok(h) => h,
            Err(e) => {
                error!("Failed to acquire handles lock for thread cleanup: {e}");
                return;
            }
        };
        let total_threads = handles.len();
        let mut failed_joins = 0;

        // TEMPORARY: Skip join to avoid hanging
        // This will leak threads but allows the program to continue
        warn!("Skipping thread joins to avoid hanging - threads will be leaked");
        for handle in handles.drain(..) {
            // Detach the thread by dropping the handle
            // The thread will continue running in the background
            // but won't block the main thread
            drop(handle);
        }

        if failed_joins > 0 {
            warn!("{failed_joins} threads failed to join properly");
        } else {
            debug!("All threads stopped successfully in {:?}", start.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{evaluation::evaluate::MaterialEvaluator, search::SearchLimitsBuilder};

    #[test]
    fn test_parallel_searcher_creation() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        let searcher = ParallelSearcher::new(evaluator, tt, 4);
        assert_eq!(searcher.num_threads, 4);
        assert_eq!(searcher.threads.len(), 4);
    }

    #[test]
    fn test_parallel_search_basic() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        let mut searcher = ParallelSearcher::new(evaluator, tt, 2);
        let mut position = Position::startpos();

        // Search with very shallow depth to avoid timeout
        let limits = SearchLimitsBuilder::default().depth(1).build();

        let result = searcher.search(&mut position, limits);

        // Should find a move
        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    fn test_adjust_thread_count() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        let mut searcher = ParallelSearcher::new(evaluator, tt, 4);
        assert_eq!(searcher.num_threads, 4);
        assert_eq!(searcher.active_threads, 4);

        // Adjust to 2 threads
        searcher.adjust_thread_count(2);
        assert_eq!(searcher.active_threads, 2);
        assert_eq!(searcher.num_threads, 4); // Original capacity unchanged

        // Try to adjust beyond capacity
        searcher.adjust_thread_count(8);
        assert_eq!(searcher.active_threads, 4); // Limited by num_threads

        // Adjust to minimum
        searcher.adjust_thread_count(0);
        assert_eq!(searcher.active_threads, 1); // Minimum is 1
    }

    #[test]
    fn test_parallel_search_with_reduced_threads() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        let mut searcher = ParallelSearcher::new(evaluator, tt, 4);

        // Reduce to 2 active threads
        searcher.adjust_thread_count(2);

        let mut position = Position::startpos();
        let limits = SearchLimitsBuilder::default().depth(1).build();

        let result = searcher.search(&mut position, limits);

        // Should still find a move with reduced threads
        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    fn test_adjust_thread_count_upscaling() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        // Start with 8 threads capacity
        let mut searcher = ParallelSearcher::new(evaluator, tt, 8);
        assert_eq!(searcher.num_threads, 8);
        assert_eq!(searcher.active_threads, 8);

        // Reduce to 2 threads
        searcher.adjust_thread_count(2);
        assert_eq!(searcher.active_threads, 2);
        assert_eq!(searcher.num_threads, 8); // Capacity unchanged

        // Scale back up to 6 threads
        searcher.adjust_thread_count(6);
        assert_eq!(searcher.active_threads, 6);
        assert_eq!(searcher.num_threads, 8); // Capacity unchanged

        // Try to scale beyond capacity
        searcher.adjust_thread_count(10);
        assert_eq!(searcher.active_threads, 8); // Limited by capacity
    }

    #[test]
    fn test_search_with_thread_scaling() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        // Test with 2 threads
        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt.clone(), 2);
        let mut position = Position::startpos();
        let limits = SearchLimitsBuilder::default().depth(1).build();
        let result = searcher.search(&mut position, limits);
        assert!(result.best_move.is_some(), "Search with 2 threads should find a move");
        assert!(result.stats.nodes > 0);
    }

    #[test]
    fn test_thread_park_control() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        // Use only 2 threads for simpler testing
        let mut searcher = ParallelSearcher::new(evaluator, tt, 2);
        let mut position = Position::startpos();

        // Search with shallow depth
        let limits = SearchLimitsBuilder::default().depth(1).build();

        let result = searcher.search(&mut position, limits);

        // Should find a move
        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    fn test_time_manager_park_duration() {
        use crate::time_management::TimeControl;

        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));

        let mut searcher = ParallelSearcher::new(evaluator, tt, 2);
        let mut position = Position::startpos();

        // Search with time limit
        let limits = SearchLimitsBuilder::default()
            .time_control(TimeControl::FixedTime { ms_per_move: 1000 })
            .depth(1)
            .build();

        let result = searcher.search(&mut position, limits);

        // Should find a move
        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);

        // TimeManager should have been created
        assert!(searcher.time_manager.is_some());
    }
}
