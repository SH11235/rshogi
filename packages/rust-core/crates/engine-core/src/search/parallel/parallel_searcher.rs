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
use log::{debug, info};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
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
#[derive(Debug, Default)]
pub struct DuplicationStats {
    /// Nodes that were not in TT (unique work)
    pub unique_nodes: AtomicU64,
    /// Total nodes searched by all threads
    pub total_nodes: AtomicU64,
}

impl DuplicationStats {
    /// Get duplication percentage (0-100)
    pub fn get_duplication_percentage(&self) -> f64 {
        let total = self.total_nodes.load(Ordering::Relaxed);
        let unique = self.unique_nodes.load(Ordering::Relaxed);

        if total == 0 {
            0.0
        } else {
            ((total - unique) as f64 / total as f64) * 100.0
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

        // Start time management thread if we have time limits
        let time_handle = self
            .time_manager
            .as_ref()
            .map(|tm| self.start_time_management_thread(tm.clone()));

        // Start worker threads
        self.start_worker_threads(position.clone(), limits.clone());

        // Main thread coordinates iterative deepening
        let result = self.coordinate_search(position, limits);

        // Stop all threads
        self.shared_state.set_stop();

        // Send stop signal to all workers
        for sender in &self.start_signals {
            let _ = sender.send(IterationSignal::Stop);
        }

        // Wait for worker threads
        let mut handles = self.handles.lock().unwrap();
        for handle in handles.drain(..) {
            let _ = handle.join();
        }

        // Stop time management thread
        if let Some(handle) = time_handle {
            let _ = handle.join();
        }

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

        for (id, thread) in self.threads.iter().enumerate() {
            if id == 0 {
                continue; // Main thread is handled separately
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

    /// Coordinate search from main thread
    fn coordinate_search(&self, position: &mut Position, limits: SearchLimits) -> SearchResult {
        let mut best_result = SearchResult::new(None, i32::MIN, SearchStats::default());
        let main_thread = self.threads[0].clone();

        // Iterative deepening loop
        for iteration in 1.. {
            // Check stop flag BEFORE starting new iteration
            if self.shared_state.should_stop() {
                break;
            }

            // Signal all worker threads to start this iteration
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
            best_result.stats.pv = vec![best_move];
            best_result.score = self.shared_state.get_best_score();
            best_result.stats.depth = self.shared_state.get_best_depth();
        }

        best_result.stats.nodes = self.shared_state.get_nodes();

        // Set duplication percentage
        best_result.stats.duplication_percentage =
            Some(self.duplication_stats.get_duplication_percentage());

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

        // Very short search with time limit to avoid infinite loop
        let limits = SearchLimitsBuilder::default().depth(2).fixed_time_ms(50).build();

        let result = searcher.search(&mut position, limits);

        // Should find a move
        assert!(!result.stats.pv.is_empty());
        assert!(result.stats.nodes > 0);
    }
}
