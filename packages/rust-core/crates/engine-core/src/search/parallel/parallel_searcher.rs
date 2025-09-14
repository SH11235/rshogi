//! Parallel search implementation

use crate::{
    evaluation::evaluate::Evaluator,
    movegen::MoveGenerator,
    search::{SearchLimits, SearchResult, SearchStats, TranspositionTable},
    shogi::{Move, Position},
    time_management::{GamePhase, TimeManager},
};
use crossbeam_deque::{Injector, Stealer, Worker as DequeWorker};
use log::{debug, info, warn};
use smallvec::SmallVec;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use super::time_manager::{start_fail_safe_guard, start_time_manager};
use super::work_queue::{Queues, WorkItem};
use super::worker::{start_worker_with, WorkerConfig};

#[cfg(feature = "ybwc")]
use super::SplitPoint;
use super::{SearchThread, SharedSearchState};

/// Initial seed strategy parameters for parallel search work distribution
/// These control how many work items are initially pushed to the global injector
/// for better work stealing distribution across threads
const INITIAL_SEED_HELPERS: usize = 2;

/// Simplified parallel searcher
pub struct ParallelSearcher<E: Evaluator + Send + Sync + 'static> {
    /// Shared transposition table
    tt: Arc<TranspositionTable>,

    /// Shared evaluator
    evaluator: Arc<E>,

    /// Time manager (wrapped in Mutex for ponderhit dynamic creation)
    time_manager: Arc<Mutex<Option<Arc<TimeManager>>>>,

    /// Shared search state
    pub(super) shared_state: Arc<SharedSearchState>,

    /// Number of threads
    num_threads: usize,

    /// Work queues (truly lock-free, no locks at all)
    queues: Arc<Queues>,

    /// Active worker count for proper synchronization
    pub(super) active_workers: Arc<AtomicUsize>,

    /// Metrics: successful steal operations
    steal_success: Arc<AtomicU64>,

    /// Metrics: failed steal operations
    steal_failure: Arc<AtomicU64>,

    /// Handle for a TimeManager spawned after ponderhit (joined in search)
    post_ponder_tm_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,

    /// Outstanding work counter for accurate completion detection
    pub(super) pending_work_items: Arc<AtomicU64>,
}

impl<E: Evaluator + Send + Sync + 'static> ParallelSearcher<E> {
    /// Handle ponderhit time management setup
    fn handle_ponderhit_time_management(&self, position: &Position, limits: &SearchLimits) {
        if let crate::time_management::TimeControl::Ponder(ref inner) = limits.time_control {
            if let Some(ref flag) = limits.ponder_hit_flag {
                if flag.load(Ordering::Acquire) {
                    let mut tm_guard = self.time_manager.lock().unwrap();
                    if tm_guard.is_none() {
                        // Create TimeManager from inner time control
                        let game_phase = if position.ply <= 40 {
                            GamePhase::Opening
                        } else if position.ply <= 120 {
                            GamePhase::MiddleGame
                        } else {
                            GamePhase::EndGame
                        };

                        // Convert inner TimeControl to SearchLimits for TimeManager creation
                        let inner_limits = SearchLimits {
                            time_control: (**inner).clone(),
                            moves_to_go: limits.moves_to_go,
                            depth: limits.depth,
                            nodes: limits.nodes,
                            qnodes_limit: limits.qnodes_limit,
                            time_parameters: limits.time_parameters,
                            stop_flag: limits.stop_flag.clone(),
                            info_callback: None, // Don't need callback for TimeManager
                            iteration_callback: None,
                            ponder_hit_flag: None,
                            qnodes_counter: limits.qnodes_counter.clone(),
                            immediate_eval_at_depth_zero: limits.immediate_eval_at_depth_zero,
                            multipv: limits.multipv,
                        };

                        // Convert to TimeLimits
                        let time_limits: crate::time_management::TimeLimits = inner_limits.into();

                        // Create new TimeManager
                        let tm = Arc::new(TimeManager::new(
                            &time_limits,
                            position.side_to_move,
                            position.ply.into(),
                            game_phase,
                        ));

                        let soft_limit = tm.soft_limit_ms();
                        debug!("TimeManager created on ponderhit (soft limit: {soft_limit}ms)");

                        // Start time manager thread and remember its handle to join later
                        let tm_handle = start_time_manager(tm.clone(), self.shared_state.clone());
                        {
                            let mut h = self.post_ponder_tm_handle.lock().unwrap();
                            // If somehow already set, overwrite (shouldn't happen with is_none() guard)
                            *h = Some(tm_handle);
                        }

                        *tm_guard = Some(tm);
                    }
                }
            }
        }
    }

    /// Distribute work to helper threads
    fn distribute_work_to_helpers(
        &self,
        position: &Position,
        iteration: usize,
        main_depth: u8,
        max_depth: u8,
        main_worker: Option<&DequeWorker<WorkItem>>,
    ) {
        // Generate root moves for the first half of iterations to distribute work
        // (at least 3 iterations, but up to half of max_depth)
        let root_move_limit = (max_depth as usize / 2).max(3);
        if iteration <= root_move_limit && self.num_threads > 1 {
            // Generate all legal moves at root
            let move_gen = MoveGenerator::new();
            let moves = match move_gen.generate_all(position) {
                Ok(moves) => moves,
                Err(_) => {
                    // King not found - should not happen in valid position
                    warn!("Failed to generate moves in parallel search");
                    return;
                }
            };

            if !moves.is_empty() {
                if log::log_enabled!(log::Level::Debug) {
                    debug!(
                        "Distributing {} root moves to {} helper threads (iteration {})",
                        moves.len(),
                        self.num_threads - 1,
                        iteration
                    );
                }

                // Use RootBatch for better efficiency (8-16 moves per batch)
                let batch_size = if moves.len() > 100 {
                    16 // Large batches for many moves
                } else if moves.len() > 40 {
                    12 // Medium batches
                } else {
                    8 // Smaller batches for fewer moves
                };

                // Create batches of root moves manually
                let mut i = 0;
                let mut batch_idx = 0;
                while i < moves.len() {
                    let mut batch_moves: SmallVec<[Move; 16]> = SmallVec::new();
                    let start_index = i;

                    // Collect up to batch_size moves
                    for _ in 0..batch_size {
                        if i >= moves.len() {
                            break;
                        }
                        batch_moves.push(moves.as_slice()[i]);
                        i += 1;
                    }

                    if !batch_moves.is_empty() {
                        // For root moves, use slightly shallower depth to avoid long-running tasks
                        let helper_depth = main_depth.saturating_sub(1).max(1);

                        // Create batch work item (wrap position in Arc for efficient sharing)
                        let work = WorkItem::RootBatch {
                            iteration,
                            depth: helper_depth,
                            position: Arc::new(position.clone()),
                            moves: batch_moves,
                            start_index,
                        };

                        // Increment pending work counter
                        self.pending_work_items.fetch_add(1, Ordering::AcqRel);

                        // Use main worker's local queue when available, with initial seeding to injector
                        if let Some(worker) = main_worker {
                            // Initial seeding: for small thread counts, seed aggressively to injector
                            // - num_threads <= 2: seed all batches to injector（ローカルデック残留を避ける）
                            // - otherwise: seed up to threads*2（最大16）
                            let seed_batches = if self.num_threads <= 2 {
                                usize::MAX
                            } else {
                                (self.num_threads * 2).min(16)
                            };
                            if batch_idx < seed_batches {
                                self.queues.injector.push(work);
                            } else {
                                // NOTE: DequeWorker::push はオーナースレッド（=メイン）からのみ呼ぶこと。
                                // 他スレッドからは Stealer で盗む運用に統一する。
                                worker.push(work);
                            }
                        } else {
                            // Fallback to injector if no main worker
                            self.queues.injector.push(work);
                        }

                        batch_idx += 1;
                    }
                }
            }
        } else {
            // Fall back to traditional full position search for deeper iterations
            if log::log_enabled!(log::Level::Debug) {
                debug!(
                    "Using FullPosition mode for iteration {iteration} (beyond limit {root_move_limit})"
                );
            }
            // Calculate how many helpers to seed to injector for initial distribution
            let seed_helpers = (self.num_threads.saturating_sub(1)).min(INITIAL_SEED_HELPERS);

            for helper_id in 1..self.num_threads {
                let helper_depth =
                    self.calculate_helper_depth(main_depth, helper_id, iteration, max_depth);

                let work = WorkItem::FullPosition {
                    iteration,
                    depth: helper_depth,
                    position: Arc::new(position.clone()),
                };

                // Increment pending work counter
                self.pending_work_items.fetch_add(1, Ordering::AcqRel);

                // Use main worker's local queue when available, with initial seeding to injector
                if let Some(worker) = main_worker {
                    // For FullPosition mode, also use initial seeding strategy
                    if helper_id <= seed_helpers {
                        self.queues.injector.push(work);
                    } else {
                        // NOTE: DequeWorker::push はオーナースレッド（=メイン）からのみ呼ぶこと。
                        // 他スレッドからは Stealer で盗む運用に統一する。
                        worker.push(work);
                    }
                } else {
                    // Fallback to injector if no main worker
                    self.queues.injector.push(work);
                }
            }
        }
    }

    /// Send heartbeat info if enough time or nodes have passed
    fn send_heartbeat_if_needed(
        &self,
        limits: &SearchLimits,
        search_start: Instant,
        last_heartbeat: &mut Instant,
        last_heartbeat_nodes: &mut u64,
        best_result: &SearchResult,
    ) {
        const HEARTBEAT_INTERVAL_MS: u64 = 1500;
        const HEARTBEAT_NODE_THRESHOLD: u64 = 1_000_000;

        if let Some(ref callback) = limits.info_callback {
            let total_nodes = self.shared_state.get_nodes();
            let elapsed_since_heartbeat = last_heartbeat.elapsed();
            let nodes_since_heartbeat = total_nodes.saturating_sub(*last_heartbeat_nodes);

            if elapsed_since_heartbeat >= Duration::from_millis(HEARTBEAT_INTERVAL_MS)
                || nodes_since_heartbeat >= HEARTBEAT_NODE_THRESHOLD
            {
                // Send heartbeat with current best result
                let elapsed = search_start.elapsed();
                debug!(
                    "Sending heartbeat (elapsed: {elapsed_since_heartbeat:?}, nodes: {total_nodes})"
                );
                // Ensure PV is not empty for heartbeat
                let pv_to_send = if best_result.stats.pv.is_empty() {
                    // Use best move if available, otherwise skip heartbeat
                    if let Some(best_move) = best_result.best_move {
                        vec![best_move]
                    } else {
                        // Skip heartbeat if no PV available
                        Vec::new()
                    }
                } else {
                    best_result.stats.pv.clone()
                };

                if !pv_to_send.is_empty() {
                    callback(
                        best_result.stats.depth.max(1),
                        best_result.score,
                        total_nodes,
                        elapsed,
                        &pv_to_send,
                        best_result.node_type,
                    );
                }
                *last_heartbeat = Instant::now();
                *last_heartbeat_nodes = total_nodes;
            }
        }
    }

    /// Wait for workers to complete their work
    fn wait_for_workers_completion(
        &self,
        search_start: Instant,
        limits: &SearchLimits,
        last_heartbeat: &mut Instant,
        best_result: &SearchResult,
    ) -> u64 {
        // Track last heartbeat nodes for this wait loop
        let mut wait_loop_last_nodes: u64 = 0;

        // Calculate dynamic timeout based on remaining soft limit
        let max_wait_ms = {
            let tm_opt = { self.time_manager.lock().unwrap().clone() };
            if let Some(ref tm) = tm_opt {
                let elapsed_ms = search_start.elapsed().as_millis() as u64;
                let soft_limit = tm.soft_limit_ms();
                if soft_limit > elapsed_ms {
                    let remaining = soft_limit - elapsed_ms;
                    // Use 1/10 of remaining time or 300ms, whichever is smaller
                    (remaining / 10).min(300)
                } else {
                    100 // Minimal wait if already past soft limit
                }
            } else {
                2000 // Longer wait for depth-only searches
            }
        };

        let mut wait_time = 0;
        let mut consecutive_zero = 0;
        loop {
            // Use pending_work_items for accurate completion detection
            let pending = self.pending_work_items.load(Ordering::Acquire);
            let active = self.active_workers.load(Ordering::Acquire);

            // Check TimeManager should_stop first
            {
                let tm_opt = { self.time_manager.lock().unwrap().clone() };
                if let Some(ref tm) = tm_opt {
                    let nodes = self.shared_state.get_nodes();
                    if tm.should_stop(nodes) {
                        debug!(
                            "TimeManager triggered stop during wait (pending: {pending}, active: {active})"
                        );
                        self.shared_state.set_stop();
                        break;
                    }
                }
            }

            if pending == 0 && active == 0 {
                consecutive_zero += 1;
                if consecutive_zero >= 2 {
                    debug!("All work completed (0 pending, 0 active) confirmed twice");
                    break;
                }
            } else {
                consecutive_zero = 0;
            }

            thread::sleep(Duration::from_millis(10));
            wait_time += 10;

            if wait_time % 100 == 0 {
                debug!(
                    "Waiting for work to complete: {pending} pending items, {active} active workers"
                );

                // Send heartbeat during wait
                self.send_heartbeat_if_needed(
                    limits,
                    search_start,
                    last_heartbeat,
                    &mut wait_loop_last_nodes,
                    best_result,
                );
            }

            // Safety: don't wait forever
            if wait_time > max_wait_ms {
                debug!("Timeout after {wait_time}ms waiting for workers: {pending} items pending, {active} workers active");
                break;
            }
        }

        wait_time
    }

    /// Create new parallel searcher
    pub fn new(evaluator: Arc<E>, tt: Arc<TranspositionTable>, num_threads: usize) -> Self {
        assert!(num_threads > 0, "Need at least one thread");

        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared_state = Arc::new(SharedSearchState::with_threads(stop_flag, num_threads));

        // Create truly lock-free work queues (no initialization needed yet)
        debug!("Creating placeholder Queues for {num_threads} threads");
        // This will be replaced in search() with the actual queues
        let injector = Arc::new(Injector::new());
        let stealers: Arc<[Stealer<WorkItem>]> = Arc::from([]);
        let queues = Arc::new(Queues { injector, stealers });

        Self {
            tt,
            evaluator,
            time_manager: Arc::new(Mutex::new(None)),
            shared_state,
            num_threads,
            queues,
            active_workers: Arc::new(AtomicUsize::new(0)),
            steal_success: Arc::new(AtomicU64::new(0)),
            steal_failure: Arc::new(AtomicU64::new(0)),
            post_ponder_tm_handle: Arc::new(Mutex::new(None)),
            pending_work_items: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Set time manager for the search (compatibility method)
    pub fn set_time_manager(&mut self, time_manager: Arc<TimeManager>) {
        *self.time_manager.lock().unwrap() = Some(time_manager);
    }

    /// Adjust the number of active threads dynamically
    pub fn adjust_thread_count(&mut self, new_active_threads: usize) {
        let new_active = new_active_threads.min(self.num_threads).max(1);
        if new_active != self.num_threads {
            debug!("Adjusting active thread count from {} to {}", self.num_threads, new_active);
            self.num_threads = new_active;

            // Update the total threads in shared state for utilization calculation
            if let Some(shared_state) = Arc::get_mut(&mut self.shared_state) {
                shared_state.total_threads = new_active;
            } else {
                // Can't get mutable reference, this is expected during search
                // The adjustment will take effect on the next search
                debug!("Thread count adjustment deferred (shared state in use)");
            }
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
    fn calculate_helper_depth(
        &self,
        main_depth: u8,
        helper_id: usize,
        iteration: usize,
        max_depth: u8,
    ) -> u8 {
        // Base offset to reduce depth for some helpers (YBWC-like variation)
        // Also vary by iteration to prevent all threads from searching the same depth
        let base_offset = ((helper_id / 2) + (iteration % 3)) as u8;

        // Small random-like variation based on helper_id and iteration
        // This creates more diversity in search depths across iterations
        let random_offset = if (helper_id + iteration) % 4 == 0 {
            1
        } else {
            0
        };

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
        self.steal_success.store(0, Ordering::Release);
        self.steal_failure.store(0, Ordering::Release);
        self.pending_work_items.store(0, Ordering::Release);

        // Create limits with shared qnodes counter from shared state
        let mut limits = limits;
        limits.qnodes_counter = Some(self.shared_state.get_qnodes_counter());

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
        // Note: Ponder mode should NOT have time management
        if !matches!(limits.time_control, TimeControl::Infinite | TimeControl::Ponder(_)) {
            let time_limits: TimeLimits = limits.clone().into();
            let time_manager = Arc::new(TimeManager::new(
                &time_limits,
                position.side_to_move,
                position.ply.into(),
                game_phase,
            ));
            let soft_limit = time_manager.soft_limit_ms();
            *self.time_manager.lock().unwrap() = Some(time_manager);
            debug!("TimeManager created with soft limit: {soft_limit}ms");
        } else {
            *self.time_manager.lock().unwrap() = None;
            let reason = match limits.time_control {
                TimeControl::Infinite => "infinite time control",
                TimeControl::Ponder(_) => "ponder mode (no time management during ponder)",
                _ => unreachable!(),
            };
            debug!("TimeManager disabled ({reason})");
        }

        // Create all workers and collect stealers at startup (once-only initialization)
        let injector = Arc::new(Injector::new());
        let mut workers = Vec::with_capacity(self.num_threads);
        let mut stealers = Vec::with_capacity(self.num_threads);

        for _ in 0..self.num_threads {
            let worker = DequeWorker::new_lifo();
            stealers.push(worker.stealer());
            workers.push(worker);
        }

        // Create immutable Queues structure (no locks needed)
        let stealers: Arc<[Stealer<WorkItem>]> = Arc::from(stealers);
        let queues = Arc::new(Queues {
            injector: injector.clone(),
            stealers: stealers.clone(),
        });

        // Replace the placeholder queues
        self.queues = queues.clone();

        // Main thread is index 0
        let main_index = 0;
        let main_worker = workers.remove(main_index);

        // Start worker threads with correct stealer indices
        let mut handles = Vec::new();
        for (i, worker) in workers.into_iter().enumerate() {
            let my_stealer_index = i + 1; // Since main thread took index 0
            let log_id = my_stealer_index; // Use same ID for logging
            let config = WorkerConfig {
                log_id,
                my_stealer_index,
                worker,
                limits: limits.clone(),
                evaluator: self.evaluator.clone(),
                tt: self.tt.clone(),
                shared_state: self.shared_state.clone(),
                queues: self.queues.clone(),
                active_workers: self.active_workers.clone(),
                steal_success: self.steal_success.clone(),
                steal_failure: self.steal_failure.clone(),
                pending_work_items: self.pending_work_items.clone(),
            };
            handles.push(start_worker_with(config));
        }

        // Start time management if needed
        let time_handle = {
            let tm_guard = self.time_manager.lock().unwrap();
            if let Some(ref tm) = *tm_guard {
                // Start time manager if we have any time limit
                // Even for short searches, we need time control to work properly
                if tm.soft_limit_ms() > 0 {
                    Some(start_time_manager(tm.clone(), self.shared_state.clone()))
                } else {
                    debug!("Skipping time manager (soft_limit: 0ms)");
                    None
                }
            } else {
                None
            }
        };

        // Start fail-safe guard thread
        let fail_safe_handle =
            start_fail_safe_guard(search_start, limits.clone(), self.shared_state.clone());

        // Main thread does iterative deepening and generates work
        let result = self.run_main_thread(position, limits, main_worker);

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

        // If we spawned a TimeManager after ponderhit, join it too
        {
            let mut h = self.post_ponder_tm_handle.lock().unwrap();
            if let Some(handle) = h.take() {
                let _ = handle.join();
            }
        }

        // Log steal metrics
        let steal_success_count = self.steal_success.load(Ordering::Relaxed);
        let steal_failure_count = self.steal_failure.load(Ordering::Relaxed);
        let total_steals = steal_success_count + steal_failure_count;
        if total_steals > 0 {
            let success_rate = (steal_success_count as f64 / total_steals as f64) * 100.0;
            info!(
                "Steal metrics: success={steal_success_count}, failure={steal_failure_count}, rate={success_rate:.1}%"
            );
        }

        result
    }

    /// Run main thread with iterative deepening
    fn run_main_thread(
        &self,
        position: &mut Position,
        limits: SearchLimits,
        main_worker: DequeWorker<WorkItem>,
    ) -> SearchResult {
        // Record search start time for info callback
        let search_start = Instant::now();

        let mut best_result = SearchResult::new(None, 0, SearchStats::default());

        // Create main search thread
        let mut main_thread = SearchThread::new(
            0,
            self.evaluator.clone(),
            self.tt.clone(),
            self.shared_state.clone(),
        );

        let max_depth = limits.depth.unwrap_or(255);

        // Heartbeat tracking for periodic info callbacks
        let mut last_heartbeat = Instant::now();
        let mut last_heartbeat_nodes = 0u64;

        // Iterative deepening
        for iteration in 1.. {
            // Check for ponderhit and create TimeManager dynamically if needed
            self.handle_ponderhit_time_management(position, &limits);

            // Skip stop check on first iteration to ensure we get at least one result
            if iteration > 1 && self.shared_state.should_stop() {
                if log::log_enabled!(log::Level::Debug) {
                    debug!("Main thread stopping at iteration {iteration}");
                }
                break;
            }

            // Also check time manager on iterations after the first
            if iteration > 1 {
                // Narrow lock scope by cloning Arc
                let tm_opt = { self.time_manager.lock().unwrap().clone() };
                if let Some(ref tm) = tm_opt {
                    let current_nodes = self.shared_state.get_nodes();
                    if tm.should_stop(current_nodes) {
                        if log::log_enabled!(log::Level::Debug) {
                            debug!(
                                "Main thread stopping at iteration {iteration} due to time limit"
                            );
                        }
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
            if log::log_enabled!(log::Level::Debug) {
                debug!("Starting iteration {iteration} (depth {main_depth})");
            }

            // Distribute work to helper threads
            self.distribute_work_to_helpers(
                position,
                iteration,
                main_depth,
                max_depth,
                Some(&main_worker),
            );

            // Main thread searches
            #[cfg(feature = "ybwc")]
            let result = {
                // Use YBWC more aggressively for better parallelization
                if iteration <= 2 || main_depth < 3 {
                    // Traditional search only for very early iterations
                    main_thread.search_iteration(position, &limits, main_depth)
                } else {
                    // YBWC search with split points (for deeper iterations)
                    let move_gen = MoveGenerator::new();
                    let legal_moves = match move_gen.generate_all(position) {
                        Ok(moves) => moves,
                        Err(_) => {
                            // King not found - should not happen in valid position
                            return SearchResult::compose(
                                None,
                                -SEARCH_INF,
                                SearchStats::default(),
                                NodeType::Exact,
                                Some(StopInfo::default()),
                                None,
                            );
                        }
                    };

                    if legal_moves.is_empty() {
                        // No legal moves - game over
                        SearchResult::compose(
                            None,
                            -i32::MAX / 2,
                            SearchStats::default(),
                            NodeType::Exact,
                            Some(StopInfo::default()),
                            None,
                        )
                    } else {
                        // Convert MoveList to Vec<Move>
                        let legal_moves_vec: Vec<Move> = legal_moves.iter().copied().collect();

                        // Get the best move from previous iteration for move ordering
                        let pv_move = self
                            .shared_state
                            .get_best_move()
                            .filter(|m| legal_moves_vec.contains(m))
                            .unwrap_or(legal_moves[0]);

                        // Search PV move first
                        let pv_result =
                            main_thread.search_root_move(position, &limits, main_depth, pv_move);

                        // Prepare siblings for split point
                        let mut remaining_moves = legal_moves_vec;
                        remaining_moves.retain(|&m| m != pv_move);

                        if !remaining_moves.is_empty() && pv_result.score < i32::MAX / 4 {
                            // Create split point for remaining moves
                            // NOTE: 現在は Position のみを共有。差分Accのスナップショット配布は
                            // Evaluator ラッパ（NNUE）側の on_set_position で子ルート同期し、
                            // 以降のノードは do/undo フック対で差分適用（常時有効）。
                            // 将来的に snapshot/restore の配布を導入する場合は、SplitPoint に
                            // Acc を持たせ、SearchThread 側で restore_single_at() を呼ぶ設計に拡張可能。
                            let split_point = SplitPoint::new(
                                position.clone(),
                                main_depth,
                                pv_result.score - 100, // alpha with margin
                                i32::MAX / 2,          // beta
                                remaining_moves,
                            );

                            // Mark PV as already searched
                            split_point.mark_pv_searched();
                            split_point.update_best(pv_result.score, pv_move);

                            // Add to split point manager
                            let sp_arc =
                                self.shared_state.split_point_manager.add_split_point(split_point);

                            // Main thread also participates
                            main_thread.process_split_point(&sp_arc);

                            // Get final best score from split point
                            let final_score = sp_arc.best_score.load(Ordering::Acquire);
                            let final_move =
                                Move::from_u16(sp_arc.best_move.load(Ordering::Acquire) as u16);

                            SearchResult::compose(
                                Some(final_move),
                                final_score,
                                pv_result.stats,
                                NodeType::Exact,
                                Some(StopInfo::default()),
                                None,
                            )
                        } else {
                            // Use PV result if no siblings or beta cutoff
                            pv_result
                        }
                    }
                }
            };

            #[cfg(not(feature = "ybwc"))]
            let result = {
                // Simple traditional search without YBWC
                main_thread.search_iteration(position, &limits, main_depth)
            };

            // Clean up completed split points
            #[cfg(feature = "ybwc")]
            self.shared_state.split_point_manager.cleanup_completed();

            if log::log_enabled!(log::Level::Debug) {
                debug!(
                    "Iteration {} completed in {:?} with score {} (depth {}, {} nodes)",
                    iteration,
                    iter_start.elapsed(),
                    result.score,
                    result.stats.depth,
                    result.stats.nodes
                );
            }

            // Update best result (always update if we don't have a move yet)
            if best_result.best_move.is_none()
                || result.score > best_result.score
                || (result.score == best_result.score
                    && result.stats.depth > best_result.stats.depth)
            {
                best_result = result.clone();
            }

            // SearchThread internally handles node counting and reporting to shared_state
            // Get total nodes from shared_state for info callbacks
            let total_nodes_from_shared = self.shared_state.get_nodes();

            // Report progress via info callback
            if let Some(ref callback) = limits.info_callback {
                // Use nodes from shared_state which already aggregates all threads
                let total_nodes = total_nodes_from_shared;
                // Use elapsed time from search start
                let elapsed = search_start.elapsed();
                // Report current iteration result (ensure PV is non-empty)
                let pv_to_send = if result.stats.pv.is_empty() {
                    // Fall back to single-move PV from best_move if available
                    if let Some(best_move) = result.best_move {
                        vec![best_move]
                    } else {
                        Vec::new()
                    }
                } else {
                    result.stats.pv.clone()
                };

                if !pv_to_send.is_empty() {
                    debug!("Calling info callback for iteration {iteration} (depth {main_depth})");
                    callback(
                        main_depth,
                        result.score,
                        total_nodes,
                        elapsed,
                        &pv_to_send,
                        result.node_type,
                    );
                }
            } else {
                debug!("No info callback available for iteration {iteration}");
            }

            // Report committed iteration via iteration_callback if available
            if let Some(ref iter_cb) = limits.iteration_callback {
                // Use nodes from shared_state which already aggregates all threads
                let total_nodes = total_nodes_from_shared;
                // Use elapsed time from search start
                let elapsed = search_start.elapsed();
                // Ensure PV is non-empty, fallback to single-move PV from best_move if needed
                let pv_to_send = if result.stats.pv.is_empty() {
                    if let Some(best_move) = result.best_move {
                        vec![best_move]
                    } else {
                        Vec::new()
                    }
                } else {
                    result.stats.pv.clone()
                };

                if !pv_to_send.is_empty() {
                    let committed = crate::search::CommittedIteration {
                        depth: result.stats.depth.max(main_depth),
                        seldepth: result.stats.seldepth,
                        score: result.score,
                        pv: pv_to_send,
                        node_type: result.node_type,
                        nodes: total_nodes,
                        elapsed,
                    };
                    iter_cb(&committed);
                }
            }

            // Send heartbeat info if enough time or nodes have passed
            self.send_heartbeat_if_needed(
                &limits,
                search_start,
                &mut last_heartbeat,
                &mut last_heartbeat_nodes,
                &best_result,
            );

            // Check depth limit
            if main_depth >= max_depth {
                info!("Main thread reached maximum depth {max_depth}, waiting for workers to complete...");

                // Wait for workers to complete their work
                let wait_time = self.wait_for_workers_completion(
                    search_start,
                    &limits,
                    &mut last_heartbeat,
                    &best_result,
                );

                // Give workers a bit more time to update shared state
                if wait_time < 2000 {
                    thread::sleep(Duration::from_millis(50));
                }

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

        best_result.stats.nodes = self.shared_state.get_nodes();
        best_result.stats.qnodes = self.shared_state.get_qnodes();

        // Ensure we always have a move (fallback to first legal move if needed)
        if best_result.best_move.is_none() {
            warn!(
                "No best move found despite searching {} nodes, using fallback",
                best_result.stats.nodes
            );
            // Generate legal moves and use the first one as fallback
            let mg = MoveGenerator::new();
            if let Ok(moves) = mg.generate_all(position) {
                if !moves.is_empty() {
                    let fallback_move = moves.as_slice()[0];
                    best_result.best_move = Some(fallback_move);
                    best_result.stats.depth = best_result.stats.depth.max(1);
                    best_result.stats.pv = vec![fallback_move];
                    warn!("Fallback bestmove used: {fallback_move}");
                }
            }
        }

        best_result
    }
}
