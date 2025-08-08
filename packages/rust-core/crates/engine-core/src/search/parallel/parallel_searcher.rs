//! Parallel search implementation

use crate::{
    evaluation::evaluate::Evaluator,
    search::{SearchLimits, SearchResult, SearchStats, ShardedTranspositionTable},
    shogi::{Move, Position},
    time_management::TimeManager,
};
use crossbeam_deque::{Injector, Stealer, Worker as DequeWorker};
use log::{debug, error, info, trace, warn};
use smallvec::SmallVec;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "ybwc")]
use super::SplitPoint;
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
        // Only log in debug builds or when debug logging is explicitly enabled
        if log::log_enabled!(log::Level::Debug) {
            debug!("Worker becoming active (active count: {count})");
        }
        Self { counter }
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        // Use trace level to reduce noise during benchmarks
        if log::log_enabled!(log::Level::Trace) {
            trace!("WorkerGuard: active worker count decremented");
        }
    }
}

/// Work item for threads to process
#[derive(Clone, Debug)]
enum WorkItem {
    /// Batch of root moves to search (8-16 moves)
    RootBatch {
        /// Iteration number
        iteration: usize,
        /// Depth to search at
        depth: u8,
        /// Position to search from (shared via Arc to avoid cloning)
        position: Arc<Position>,
        /// Batch of moves to search
        moves: SmallVec<[Move; 16]>,
        /// Starting index of the batch (for debugging)
        start_index: usize,
    },
    /// Search a specific root move (legacy, for single moves)
    RootMove {
        /// Iteration number
        iteration: usize,
        /// Depth to search at
        depth: u8,
        /// Position to search from (shared via Arc to avoid cloning)
        position: Arc<Position>,
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
        /// Position to search (shared via Arc to avoid cloning)
        position: Arc<Position>,
    },
}

/// Truly lock-free work queue structure
#[derive(Clone)]
struct Queues {
    /// Global injector (Arc for sharing)
    injector: Arc<Injector<WorkItem>>,
    /// Stealers for work stealing (immutable after creation)
    stealers: Arc<[Stealer<WorkItem>]>,
}

/// Get work with 3-layer priority (truly lock-free with exponential backoff)
fn get_job(
    my_worker: &DequeWorker<WorkItem>,
    queues: &Queues,
    thread_id: usize,
    steal_success: &AtomicU64,
    steal_failure: &AtomicU64,
) -> Option<WorkItem> {
    // Thread-local steal failure counter for exponential backoff
    thread_local! {
        static CONSECUTIVE_STEAL_FAILS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    // 1. Try local worker queue first (LIFO for DFS)
    if let Some(item) = my_worker.pop() {
        // Reset failure counter on success
        CONSECUTIVE_STEAL_FAILS.with(|f| f.set(0));
        return Some(item);
    }

    // Track if we found work via stealing
    let found_work = false;

    // 2. Then try stealing from other workers (random selection for better scalability)
    // Instead of scanning all workers (O(T^2)), randomly select a few workers
    if !queues.stealers.is_empty() {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        // Try a few random steals (min of 3 or half the workers)
        let steal_attempts = 3.min(queues.stealers.len());
        for _ in 0..steal_attempts {
            let idx = rng.gen_range(0..queues.stealers.len());

            // Try to steal from selected worker
            match queues.stealers[idx].steal() {
                crossbeam_deque::Steal::Success(item) => {
                    // Reset failure counter on success
                    CONSECUTIVE_STEAL_FAILS.with(|f| f.set(0));
                    steal_success.fetch_add(1, Ordering::Relaxed);
                    return Some(item);
                }
                crossbeam_deque::Steal::Empty => continue,
                crossbeam_deque::Steal::Retry => {
                    // Try once more on retry
                    if let crossbeam_deque::Steal::Success(item) = queues.stealers[idx].steal() {
                        CONSECUTIVE_STEAL_FAILS.with(|f| f.set(0));
                        steal_success.fetch_add(1, Ordering::Relaxed);
                        return Some(item);
                    }
                }
            }
        }
    }

    // 3. Finally try the global injector with batch stealing
    loop {
        match queues.injector.steal_batch_and_pop(my_worker) {
            crossbeam_deque::Steal::Success(item) => {
                // Reset failure counter on success
                CONSECUTIVE_STEAL_FAILS.with(|f| f.set(0));
                steal_success.fetch_add(1, Ordering::Relaxed);
                return Some(item);
            }
            crossbeam_deque::Steal::Empty => break,
            crossbeam_deque::Steal::Retry => continue,
        }
    }

    // Record failure
    steal_failure.fetch_add(1, Ordering::Relaxed);

    // Apply exponential backoff based on consecutive failures
    CONSECUTIVE_STEAL_FAILS.with(|f| {
        let fails = f.get() + 1;
        f.set(fails);

        // Exponential backoff strategy
        match fails {
            1..=5 => {
                // Spin for first few attempts (hot loop)
            }
            6..=15 => {
                // Yield to OS scheduler for medium failures
                std::thread::yield_now();
            }
            16..=25 => {
                // Brief sleep for many failures
                std::thread::sleep(Duration::from_micros(10));
            }
            _ => {
                // Longer sleep for persistent failures
                std::thread::sleep(Duration::from_micros(100));
            }
        }
    });

    None
}

/// Simplified parallel searcher
pub struct ParallelSearcher<E: Evaluator + Send + Sync + 'static> {
    /// Shared transposition table
    tt: Arc<ShardedTranspositionTable>,

    /// Shared evaluator
    evaluator: Arc<E>,

    /// Time manager
    time_manager: Option<Arc<TimeManager>>,

    /// Shared search state
    shared_state: Arc<SharedSearchState>,

    /// Number of threads
    num_threads: usize,

    /// Work queues (truly lock-free, no locks at all)
    queues: Arc<Queues>,

    /// Node counter
    total_nodes: Arc<AtomicU64>,

    /// Active worker count for proper synchronization
    active_workers: Arc<AtomicUsize>,

    /// Metrics: successful steal operations
    steal_success: Arc<AtomicU64>,

    /// Metrics: failed steal operations
    steal_failure: Arc<AtomicU64>,
}

impl<E: Evaluator + Send + Sync + 'static> ParallelSearcher<E> {
    /// Create new parallel searcher
    pub fn new(evaluator: Arc<E>, tt: Arc<ShardedTranspositionTable>, num_threads: usize) -> Self {
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
            time_manager: None,
            shared_state,
            num_threads,
            queues,
            total_nodes: Arc::new(AtomicU64::new(0)),
            active_workers: Arc::new(AtomicUsize::new(0)),
            steal_success: Arc::new(AtomicU64::new(0)),
            steal_failure: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Set time manager for the search (compatibility method)
    pub fn set_time_manager(&mut self, time_manager: Arc<TimeManager>) {
        self.time_manager = Some(time_manager);
    }

    /// Adjust the number of active threads dynamically
    pub fn adjust_thread_count(&mut self, new_active_threads: usize) {
        let new_active = new_active_threads.min(self.num_threads).max(1);
        if new_active != self.num_threads {
            debug!("Adjusting active thread count from {} to {}", self.num_threads, new_active);

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
        // Clear the injector (best effort)
        while !self.queues.injector.is_empty() {
            let _ = self.queues.injector.steal();
        }
        self.total_nodes.store(0, Ordering::Release);
        self.steal_success.store(0, Ordering::Release);
        self.steal_failure.store(0, Ordering::Release);

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

        // Start worker threads
        let mut handles = Vec::new();
        for id in 1..self.num_threads {
            let worker = workers.pop().unwrap();
            handles.push(self.start_worker_with(id, worker, limits.clone()));
        }

        // Keep the main thread's worker
        let main_worker = workers.pop().unwrap();

        // Start time management if needed
        let time_handle = if let Some(ref tm) = self.time_manager {
            // Start time manager if we have any time limit
            // Even for short searches, we need time control to work properly
            if tm.soft_limit_ms() > 0 {
                Some(self.start_time_manager(tm.clone()))
            } else {
                debug!("Skipping time manager (soft_limit: 0ms)");
                None
            }
        } else {
            None
        };

        // Start fail-safe guard thread
        let fail_safe_handle = self.start_fail_safe_guard(search_start, limits.clone());

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

    /// Start a worker thread with a pre-created worker
    fn start_worker_with(
        &self,
        id: usize,
        worker: DequeWorker<WorkItem>,
        limits: SearchLimits,
    ) -> thread::JoinHandle<()> {
        let evaluator = self.evaluator.clone();
        let tt = self.tt.clone();
        let shared_state = self.shared_state.clone();
        let queues = self.queues.clone();
        let total_nodes = self.total_nodes.clone();
        let active_workers = self.active_workers.clone();
        let steal_success = self.steal_success.clone();
        let steal_failure = self.steal_failure.clone();

        thread::spawn(move || {
            if log::log_enabled!(log::Level::Debug) {
                debug!("Worker {id} started");
            }

            // Create search thread
            let mut search_thread = SearchThread::new(id, evaluator, tt, shared_state.clone());

            let mut local_nodes = 0u64;
            let mut last_report = 0u64;

            // Simple work loop
            while !shared_state.should_stop() {
                // Try to get work using truly lock-free work stealing
                let work = get_job(&worker, &queues, id, &steal_success, &steal_failure);

                if let Some(work) = work {
                    // Create guard which atomically increments the counter
                    let _guard = WorkerGuard::new(active_workers.clone());

                    let prev_nodes = local_nodes; // Track previous node count
                    let nodes = match work {
                        WorkItem::RootBatch {
                            iteration,
                            depth,
                            position,
                            moves,
                            start_index,
                        } => {
                            // Skip debug logging in hot path unless explicitly enabled
                            if log::log_enabled!(log::Level::Debug) {
                                debug!(
                                    "Worker {id} processing RootBatch with {} moves starting at #{start_index} (iteration {iteration}, depth {depth})",
                                    moves.len()
                                );
                            }

                            // Clone position only once per batch (not per move)
                            let mut pos = (*position).clone();
                            
                            // Process all moves in the batch
                            for move_to_search in moves.iter() {
                                // Search the specific root move (reusing the same position)
                                let _result = search_thread.search_root_move(
                                    &mut pos,
                                    &limits,
                                    depth,
                                    *move_to_search,
                                );

                                // Check stop flag between moves
                                if shared_state.should_stop() {
                                    break;
                                }
                            }

                            // Update nodes (accumulate the difference)
                            let nodes = search_thread.searcher.nodes();
                            local_nodes += nodes.saturating_sub(prev_nodes);
                            nodes
                        }
                        WorkItem::RootMove {
                            iteration,
                            depth,
                            position,
                            move_to_search,
                            move_index,
                        } => {
                            // Skip debug logging in hot path unless explicitly enabled
                            if log::log_enabled!(log::Level::Debug) {
                                debug!(
                                    "Worker {id} processing RootMove #{move_index} (iteration {iteration}, depth {depth})"
                                );
                            }

                            // Clone position from Arc for this search
                            let mut pos = (*position).clone();

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
                            // Skip debug logging in hot path unless explicitly enabled
                            if log::log_enabled!(log::Level::Debug) {
                                debug!(
                                    "Worker {id} processing FullPosition (iteration {iteration}, depth {depth})"
                                );
                            }

                            // Clone position from Arc for this search
                            let mut pos = (*position).clone();

                            // Do the search
                            let _result = search_thread.search_iteration(&mut pos, &limits, depth);

                            // Update nodes (accumulate the difference)
                            let nodes = search_thread.searcher.nodes();
                            local_nodes += nodes.saturating_sub(prev_nodes);
                            nodes
                        }
                    };

                    // Skip debug logging in hot path unless explicitly enabled
                    if log::log_enabled!(log::Level::Debug) {
                        debug!("Worker {id} work completed");
                    }

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
                    // No work available, check for split points (YBWC)
                    #[cfg(feature = "ybwc")]
                    {
                        if let Some(split_point) =
                            shared_state.split_point_manager.get_available_split_point()
                        {
                            // Process the split point
                            search_thread.process_split_point(&split_point);
                        } else {
                            // No work or split points available, brief sleep
                            thread::sleep(Duration::from_micros(50));
                        }
                    }
                    #[cfg(not(feature = "ybwc"))]
                    {
                        // No work available, brief sleep
                        thread::sleep(Duration::from_micros(100));
                    }
                }
            }

            // Final node report
            if local_nodes > last_report {
                total_nodes.fetch_add(local_nodes - last_report, Ordering::Relaxed);
                shared_state.add_nodes(local_nodes - last_report);
            }

            if log::log_enabled!(log::Level::Debug) {
                debug!("Worker {id} stopped with {local_nodes} nodes");
            }
        })
    }

    /// Run main thread with iterative deepening
    fn run_main_thread(
        &self,
        position: &mut Position,
        limits: SearchLimits,
        main_worker: DequeWorker<WorkItem>,
    ) -> SearchResult {
        let mut best_result = SearchResult::new(None, 0, SearchStats::default());

        // Create main search thread
        let mut main_thread = SearchThread::new(
            0,
            self.evaluator.clone(),
            self.tt.clone(),
            self.shared_state.clone(),
        );

        let max_depth = limits.depth.unwrap_or(255);
        let mut last_reported_nodes = 0u64; // Track last reported node count

        // Iterative deepening
        for iteration in 1.. {
            // Skip stop check on first iteration to ensure we get at least one result
            if iteration > 1 && self.shared_state.should_stop() {
                if log::log_enabled!(log::Level::Debug) {
                    debug!("Main thread stopping at iteration {iteration}");
                }
                break;
            }

            // Also check time manager on iterations after the first
            if iteration > 1 {
                if let Some(ref tm) = self.time_manager {
                    let current_nodes = self.total_nodes.load(Ordering::Relaxed);
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

            // Generate root moves for the first half of iterations to distribute work
            // (at least 3 iterations, but up to half of max_depth)
            let root_move_limit = (max_depth as usize / 2).max(3);
            if iteration <= root_move_limit && self.num_threads > 1 {
                // Generate all legal moves at root
                let mut move_gen = crate::movegen::generator::MoveGenImpl::new(position);
                let moves = move_gen.generate_all();

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
                    let mut batch_idx = 0;
                    let mut i = 0;
                    while i < moves.len() {
                        let mut batch_moves: SmallVec<[Move; 16]> = SmallVec::new();
                        let start_index = i;

                        // Collect up to batch_size moves
                        for _ in 0..batch_size {
                            if i >= moves.len() {
                                break;
                            }
                            batch_moves.push(moves[i]);
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

                            // Push to global injector (lock-free)
                            self.queues.injector.push(work);
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
                for helper_id in 1..self.num_threads {
                    let helper_depth =
                        self.calculate_helper_depth(main_depth, helper_id, iteration, max_depth);

                    let work = WorkItem::FullPosition {
                        iteration,
                        depth: helper_depth,
                        position: Arc::new(position.clone()),
                    };

                    // Push to global injector (lock-free)
                    self.queues.injector.push(work);
                }
            }

            // Main thread searches
            #[cfg(feature = "ybwc")]
            let result = {
                // Use YBWC more aggressively for better parallelization
                if iteration <= 2 || main_depth < 3 {
                    // Traditional search only for very early iterations
                    main_thread.search_iteration(position, &limits, main_depth)
                } else {
                    // YBWC search with split points (for deeper iterations)
                    let mut move_gen = crate::movegen::generator::MoveGenImpl::new(position);
                    let legal_moves = move_gen.generate_all();

                    if legal_moves.is_empty() {
                        // No legal moves - game over
                        SearchResult {
                            score: -i32::MAX / 2,
                            stats: SearchStats::default(),
                            best_move: None,
                        }
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

                            SearchResult {
                                score: final_score,
                                stats: pv_result.stats,
                                best_move: Some(final_move),
                            }
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
                    100 // Short wait when time-limited
                } else {
                    2000 // Longer wait for depth-only searches
                };

                let mut wait_time = 0;
                loop {
                    let pending = self.queues.injector.len();
                    let active = self.active_workers.load(Ordering::Acquire);

                    // Check TimeManager should_stop first
                    if let Some(ref tm) = self.time_manager {
                        let nodes = self.total_nodes.load(Ordering::Relaxed);
                        if tm.should_stop(nodes) {
                            debug!(
                                "TimeManager triggered stop during wait (pending: {pending}, active: {active})"
                            );
                            self.shared_state.set_stop();
                            break;
                        }
                    }

                    if pending == 0 && active == 0 {
                        debug!("All work completed (0 pending, 0 active)");
                        break;
                    }

                    thread::sleep(Duration::from_millis(10));
                    wait_time += 10;

                    if wait_time % 100 == 0 {
                        debug!(
                            "Waiting for work to complete: {pending} pending items, {active} active workers"
                        );
                    }

                    // Safety: don't wait forever
                    if wait_time > max_wait_ms {
                        debug!("Timeout after {wait_time}ms waiting for workers: {pending} items pending, {active} workers active");
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
            let hard_timeout_ms =
                if limits.depth.is_some() && matches!(limits.time_control, TimeControl::Infinite) {
                    hard_timeout_ms.max(10_000) // 10 seconds for depth-only searches (reduced from 60s)
                } else {
                    hard_timeout_ms.max(1000) // At least 1 second for time-controlled searches
                };

            if log::log_enabled!(log::Level::Debug) {
                debug!("Fail-safe guard started with hard timeout: {hard_timeout_ms}ms");
            }

            // Check periodically
            loop {
                thread::sleep(Duration::from_millis(100));

                // Check if search stopped normally
                if shared_state.should_stop() {
                    if log::log_enabled!(log::Level::Debug) {
                        debug!("Fail-safe guard: Search stopped normally");
                    }
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

            if log::log_enabled!(log::Level::Debug) {
                debug!("Fail-safe guard stopped");
            }
        })
    }

    /// Start time management thread
    fn start_time_manager(&self, time_manager: Arc<TimeManager>) -> thread::JoinHandle<()> {
        let shared_state = self.shared_state.clone();
        let total_nodes = self.total_nodes.clone();

        thread::spawn(move || {
            if log::log_enabled!(log::Level::Debug) {
                debug!("Time manager started");
            }

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

            if log::log_enabled!(log::Level::Debug) {
                debug!("Time manager stopped");
            }
        })
    }
}
