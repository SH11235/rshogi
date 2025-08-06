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
use log::{debug, info, warn};
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
        self.unique_nodes.store(0, Ordering::Relaxed);
        self.total_nodes.store(0, Ordering::Relaxed);
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
    start_signals: Vec<Sender<IterationSignal>>,

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
            start_signals: Vec::new(),
            active_threads: num_threads,
        }
    }

    /// Set time manager for the search
    pub fn set_time_manager(&mut self, time_manager: Arc<TimeManager>) {
        self.time_manager = Some(time_manager);
    }

    /// Main search entry point
    pub fn search(&mut self, position: &mut Position, limits: SearchLimits) -> SearchResult {
        info!("Starting parallel search with {} threads", self.num_threads);

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
        info!("Search complete. Duplication: {dup_pct:.1}%");

        result
    }

    /// Start worker threads
    fn start_worker_threads(&mut self, position: Position, limits: SearchLimits) {
        let mut handles = self.handles.lock().unwrap();
        handles.clear();

        // Clear old channels and create new ones
        self.start_signals.clear();

        // Only start up to active_threads workers
        let workers_to_start = self.active_threads.saturating_sub(1); // -1 for main thread

        for (id, thread) in self.threads.iter().enumerate() {
            if id == 0 {
                continue; // Main thread is handled separately
            }

            // Only start threads up to active_threads limit
            if id > workers_to_start {
                debug!(
                    "Thread {} inactive for this search (active_threads={})",
                    id, self.active_threads
                );
                break;
            }

            // Create channel for this worker
            let (sender, receiver) = crossbeam::channel::unbounded();
            self.start_signals.push(sender);

            let thread = thread.clone();
            let mut position = position.clone();
            let limits = limits.clone();
            let shared_state = self.shared_state.clone();

            let handle = thread::spawn(move || {
                let mut thread = thread.lock().unwrap();
                thread.reset();

                // Worker thread search loop
                loop {
                    // Try to receive signal with timeout
                    match receiver.recv_timeout(Duration::from_millis(10)) {
                        Ok(IterationSignal::StartIteration(iteration)) => {
                            // Check stop flag before starting
                            if shared_state.should_stop() {
                                thread.report_nodes();
                                break;
                            }

                            let depth = thread.get_start_depth(iteration);
                            debug!("Thread {id} starting depth {depth}");

                            let _result = thread.search(&mut position, limits.clone(), depth);

                            // Update node count (differential)
                            thread.report_nodes();
                        }
                        Ok(IterationSignal::Stop) => {
                            thread.report_nodes();
                            break;
                        }
                        Err(_) => {
                            // Timeout - check stop flag
                            if shared_state.should_stop() {
                                thread.report_nodes();
                                break;
                            }
                            // Continue waiting
                        }
                    }
                }
            });

            handles.push(handle);
        }
    }

    /// Adjust the number of active threads dynamically
    pub fn adjust_thread_count(&mut self, new_active_threads: usize) {
        let new_active_threads = new_active_threads.min(self.num_threads).max(1);

        if new_active_threads != self.active_threads {
            info!(
                "Adjusting active threads from {} to {}",
                self.active_threads, new_active_threads
            );
            self.active_threads = new_active_threads;
            // The actual thread limiting will be done during search by only sending signals
            // to the first `active_threads` workers
        }
    }

    /// Coordinate search from main thread
    fn coordinate_search(&self, position: &mut Position, limits: SearchLimits) -> SearchResult {
        let mut best_result = SearchResult::new(None, i32::MIN, SearchStats::default());
        let main_thread = self.threads[0].clone();
        let mut time_handle: Option<thread::JoinHandle<()>> = None;

        // Iterative deepening loop
        for iteration in 1.. {
            // Check stop flag BEFORE starting new iteration (except first iteration)
            if iteration > 1 && self.shared_state.should_stop() {
                break;
            }

            // Signal all active worker threads to start this iteration
            // Note: start_signals only contains channels for active threads
            for sender in &self.start_signals {
                let _ = sender.send(IterationSignal::StartIteration(iteration));
            }

            // Main thread searches at normal depth
            let mut thread = main_thread.lock().unwrap();
            let depth = thread.get_start_depth(iteration);

            info!("Starting iteration {iteration} (depth {depth})");
            let result = thread.search(position, limits.clone(), depth);

            // Update best result
            if result.score > best_result.score || result.stats.depth > best_result.stats.depth {
                best_result = result;
            }

            // Start time management thread after first iteration completes
            if iteration == 1 && time_handle.is_none() {
                if let Some(tm) = &self.time_manager {
                    time_handle = Some(self.start_time_management_thread(tm.clone()));
                }
            }

            // Check depth limit
            if let Some(max_depth) = limits.depth {
                if depth >= max_depth {
                    info!("Reached maximum depth {max_depth}");
                    break;
                }
            }

            // Update node count (differential)
            thread.report_nodes();
        }

        // Report final nodes from main thread
        let mut thread = main_thread.lock().unwrap();
        thread.report_nodes();

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
                    shared_state.set_stop();
                    // time_manager.force_stop() is redundant - removed
                    break;
                }
            }
        })
    }

    /// Stop all threads with timeout
    fn stop_threads_with_timeout(&mut self, timeout: Duration) {
        let start = Instant::now();

        // First, set stop flag
        self.shared_state.set_stop();

        // Send stop signal to all workers
        for sender in &self.start_signals {
            let _ = sender.send(IterationSignal::Stop);
        }

        // Wait for worker threads with timeout
        let mut handles = self.handles.lock().unwrap();
        let total_threads = handles.len();
        let mut failed_joins = 0;

        for (idx, handle) in handles.drain(..).enumerate() {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                warn!("Thread join timeout reached after {idx} threads");
                failed_joins = total_threads - idx;
                break;
            }

            // Unfortunately, std::thread::JoinHandle doesn't support timeout
            // In real implementation, we'd need a different approach
            // For now, just join normally
            if let Err(e) = handle.join() {
                warn!("Thread {idx} panicked: {e:?}");
                failed_joins += 1;
            }
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

        // Search with depth limit only
        let limits = SearchLimitsBuilder::default().depth(2).build();

        let result = searcher.search(&mut position, limits);

        // Should find a move
        assert!(!result.stats.pv.is_empty());
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
        let limits = SearchLimitsBuilder::default().depth(2).build();

        let result = searcher.search(&mut position, limits);

        // Should still find a move with reduced threads
        assert!(!result.stats.pv.is_empty());
        assert!(result.stats.nodes > 0);
    }
}
