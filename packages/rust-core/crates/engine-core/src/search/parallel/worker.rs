//! Worker thread implementation for parallel search

use super::work_queue::{get_job, Queues, WorkItem};
use super::{SearchThread, SharedSearchState};
use crate::{
    evaluation::evaluate::Evaluator,
    search::{SearchLimits, TranspositionTable},
};
use crossbeam_deque::Worker as DequeWorker;
use log::{debug, error};
use std::{
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

/// Configuration for a worker thread
pub struct WorkerConfig<E: Evaluator + Send + Sync + 'static> {
    pub log_id: usize,
    pub my_stealer_index: usize,
    pub worker: DequeWorker<WorkItem>,
    pub limits: SearchLimits,
    pub evaluator: Arc<E>,
    pub tt: Arc<TranspositionTable>,
    pub shared_state: Arc<SharedSearchState>,
    pub queues: Arc<Queues>,
    pub active_workers: Arc<AtomicUsize>,
    pub steal_success: Arc<AtomicU64>,
    pub steal_failure: Arc<AtomicU64>,
    pub pending_work_items: Arc<AtomicU64>,
}

/// RAII guard to ensure active worker count is decremented
pub struct WorkerGuard {
    counter: Arc<AtomicUsize>,
}

impl WorkerGuard {
    /// Create a new guard and atomically increment the counter
    pub fn new(counter: Arc<AtomicUsize>) -> Self {
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
            log::trace!("WorkerGuard: active worker count decremented");
        }
    }
}

/// RAII guard to ensure pending work counter is decremented exactly once
struct PendingWorkGuard {
    counter: Arc<AtomicU64>,
    active: bool,
}

impl PendingWorkGuard {
    fn new(counter: Arc<AtomicU64>) -> Self {
        Self {
            counter,
            active: true,
        }
    }

    /// Prevent decrement (if ownership of accounting was transferred)
    #[allow(dead_code)]
    fn disarm(mut self) {
        self.active = false;
    }
}

impl Drop for PendingWorkGuard {
    fn drop(&mut self) {
        if self.active {
            self.counter.fetch_sub(1, Ordering::AcqRel);
            if log::log_enabled!(log::Level::Trace) {
                log::trace!("PendingWorkGuard: pending work decremented");
            }
        }
    }
}

/// Start a worker thread with a pre-created worker
pub fn start_worker_with<E: Evaluator + Send + Sync + 'static>(
    config: WorkerConfig<E>,
) -> thread::JoinHandle<()> {
    // Create worker-specific limits without info_callback to prevent INFO flood
    // USI protocol: Only main thread should output INFO messages
    let mut worker_limits = config.limits.clone();
    worker_limits.info_callback = None;

    // Extract values from config for use in closure
    let log_id = config.log_id;
    let my_stealer_index = config.my_stealer_index;
    let worker = config.worker;
    let evaluator = config.evaluator;
    let tt = config.tt;
    let shared_state = config.shared_state;
    let queues = config.queues;
    let active_workers = config.active_workers;
    let steal_success = config.steal_success;
    let steal_failure = config.steal_failure;
    let pending_work_items = config.pending_work_items;

    thread::spawn(move || {
        use std::panic::{self, AssertUnwindSafe};

        let res = panic::catch_unwind(AssertUnwindSafe(|| {
            if log::log_enabled!(log::Level::Debug) {
                debug!("Worker {log_id} started");
            }

            // Create search thread
            let mut search_thread = SearchThread::new(log_id, evaluator, tt, shared_state.clone());

            // Simple work loop
            while !shared_state.should_stop() {
                // Try to get work using truly lock-free work stealing
                let work =
                    get_job(&worker, &queues, my_stealer_index, &steal_success, &steal_failure);

                if let Some(work) = work {
                    // Create guard which atomically increments the counter
                    // IMPORTANT: Guard must be created here (after work is obtained, before processing)
                    // to ensure proper active worker count even with early returns or panics
                    let _guard = WorkerGuard::new(active_workers.clone());

                    // Ensure pending work is decremented even if this thread panics while processing
                    let _pending_guard = PendingWorkGuard::new(pending_work_items.clone());

                    match work {
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
                                    "Worker {log_id} processing RootBatch with {} moves starting at #{start_index} (iteration {iteration}, depth {depth})",
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
                                    &worker_limits,
                                    depth,
                                    *move_to_search,
                                );

                                // Check stop flag between moves
                                if shared_state.should_stop() {
                                    break;
                                }
                            }
                        }
                        WorkItem::FullPosition {
                            iteration,
                            depth,
                            position,
                        } => {
                            // Skip debug logging in hot path unless explicitly enabled
                            if log::log_enabled!(log::Level::Debug) {
                                debug!(
                                    "Worker {log_id} processing FullPosition (iteration {iteration}, depth {depth})"
                                );
                            }

                            // Clone position from Arc for this search
                            let mut pos = (*position).clone();

                            // Do the search
                            let _result =
                                search_thread.search_iteration(&mut pos, &worker_limits, depth);
                        }
                    }

                    // Skip debug logging in hot path unless explicitly enabled
                    if log::log_enabled!(log::Level::Debug) {
                        debug!("Worker {log_id} work completed");
                    }

                    // Note: WorkerGuard will automatically decrement active_workers when dropped
                    // Note: SearchThread internally handles node counting and reporting to shared_state

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
                            // Create guard to track active worker count
                            let _guard = WorkerGuard::new(active_workers.clone());
                            // Process the split point
                            search_thread.process_split_point(&split_point);
                            // guard will be dropped here, decrementing active_workers
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

            // SearchThread automatically flushes nodes when work is completed
            // No need for manual node reporting here

            // After stop or normal loop exit, drain any remaining enqueued work items
            // without processing them to keep pending_work_items consistent.
            // This prevents leftover pending counts when stop is requested while work remains queued.
            loop {
                let pending_left = pending_work_items.load(Ordering::Acquire);
                if pending_left == 0 {
                    break;
                }
                match get_job(&worker, &queues, my_stealer_index, &steal_success, &steal_failure) {
                    Some(_item) => {
                        // Cancel the work item by accounting only
                        pending_work_items.fetch_sub(1, Ordering::AcqRel);
                        if log::log_enabled!(log::Level::Trace) {
                            log::trace!("Worker {log_id} drained one pending work item (remaining before: {pending_left})");
                        }
                        // Continue draining until queues appear empty or counter reaches zero
                        continue;
                    }
                    None => break,
                }
            }

            if log::log_enabled!(log::Level::Debug) {
                debug!("Worker {log_id} stopped");
            }
        }));

        if res.is_err() {
            // どれかワーカーが落ちたら全体停止フラグを立てる
            error!("Worker {log_id} panicked; requesting graceful stop");
            shared_state.set_stop();
        }
    })
}
