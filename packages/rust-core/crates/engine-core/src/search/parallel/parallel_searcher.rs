//! Parallel search implementation

use crate::{
    evaluation::evaluate::Evaluator,
    movegen::MoveGenerator,
    search::{
        limits::FallbackDeadlines,
        types::{InfoStringCallback, StopInfo, TerminationReason},
        SearchLimits, SearchResult, SearchStats, TranspositionTable,
    },
    shogi::{Move, Position},
    time_management::{GamePhase, TimeControl, TimeManager},
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
use crate::search::parallel::util::{
    compute_dynamic_hygiene_max, compute_finalize_window_ms, compute_hard_guard_ms,
    compute_hygiene_wait_budget, HYGIENE_WAIT_MAX_MS, HYGIENE_WAIT_STEP_MS,
};

#[cfg(feature = "ybwc")]
use super::SplitPoint;
use super::{EngineStopBridge, SearchThread, SharedSearchState};

/// Parameters for finalizing search due to time limit
#[derive(Debug, Clone)]
struct TimeLimitFinalization {
    /// Current best search result snapshot
    best_snapshot: SearchResult,
    /// Elapsed time in milliseconds
    elapsed_ms: u64,
    /// Current node count
    current_nodes: u64,
    /// Soft time limit in milliseconds (if available)
    soft_limit_ms: u64,
    /// Hard time limit in milliseconds
    hard_limit_ms: u64,
    /// Planned time limit in milliseconds
    planned_limit_ms: u64,
    /// Whether hard timeout was reached
    hard_timeout: bool,
    /// Label for diagnostic purposes
    label: String,
}

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

    /// Maximum configurable threads for this searcher
    max_threads: usize,

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

    /// Bridge for propagating immediate stop requests without locking the engine
    stop_bridge: Arc<EngineStopBridge>,
}

impl<E: Evaluator + Send + Sync + 'static> ParallelSearcher<E> {
    #[inline]
    fn broadcast_stop(&self, external_stop: Option<&Arc<AtomicBool>>) {
        let already_has_reason = self.shared_state.stop_info.get().is_some();

        if already_has_reason {
            // 既に StopInfo が設定済み（例: TimeLimit）の場合は、理由を維持しつつ停止のみ伝播。
            self.shared_state.set_stop();
            self.shared_state.close_work_queues();
            return;
        }

        if external_stop.is_some() {
            if let Some(flag) = external_stop {
                flag.store(true, Ordering::Release);
            }
            let tm_snapshot = { self.time_manager.lock().unwrap().clone() };
            let elapsed_ms = tm_snapshot.as_ref().map(|tm| tm.elapsed_ms()).unwrap_or(0);
            let soft_ms = tm_snapshot.as_ref().map(|tm| tm.soft_limit_ms()).unwrap_or(0);
            let hard_ms = tm_snapshot.as_ref().map(|tm| tm.hard_limit_ms()).unwrap_or(0);
            let nodes = self.shared_state.get_nodes();
            let depth = self.shared_state.get_best_depth();
            let stop_info = StopInfo {
                reason: TerminationReason::UserStop,
                elapsed_ms,
                nodes,
                depth_reached: depth,
                hard_timeout: false,
                soft_limit_ms: soft_ms,
                hard_limit_ms: hard_ms,
            };
            self.shared_state.set_stop_with_reason(stop_info);
        } else {
            self.shared_state.set_stop();
        }
        self.shared_state.close_work_queues();
    }

    #[inline]
    fn emit_info_string<S: AsRef<str>>(&self, limits: &SearchLimits, message: S) {
        if let Some(cb) = &limits.info_string_callback {
            cb(message.as_ref());
        } else {
            log::info!("info string {}", message.as_ref());
        }
    }

    fn finalize_fallback_deadline(
        &self,
        position: &mut Position,
        best_result: SearchResult,
        search_start: Instant,
        deadlines: FallbackDeadlines,
        hard: bool,
    ) -> SearchResult {
        use crate::search::types::{StopInfo, TerminationReason};

        let elapsed_ms = search_start.elapsed().as_millis() as u64;
        let nodes = self.shared_state.get_nodes();
        let depth = self.shared_state.get_best_depth();
        let stop_info = StopInfo {
            reason: TerminationReason::TimeLimit,
            elapsed_ms,
            nodes,
            depth_reached: depth,
            hard_timeout: hard,
            soft_limit_ms: deadlines.soft_limit_ms,
            hard_limit_ms: deadlines.hard_limit_ms,
        };

        self.shared_state.set_stop_with_reason(stop_info);
        // 内部要因（TimeLimit）による停止なので external stop は立てず、停止のみ伝播。
        self.broadcast_stop(None);
        self.prepare_final_result(position, best_result)
    }

    #[inline]
    fn wait_for_active_clear(&self, max_wait_ms: u64) -> (u64, usize) {
        if max_wait_ms == 0 {
            return (0, self.active_workers.load(Ordering::Acquire));
        }

        let mut waited = 0;
        while waited < max_wait_ms {
            let active = self.active_workers.load(Ordering::Acquire);
            if active == 0 {
                return (waited, active);
            }
            thread::sleep(Duration::from_millis(HYGIENE_WAIT_STEP_MS));
            waited += HYGIENE_WAIT_STEP_MS;
        }

        let active = self.active_workers.load(Ordering::Acquire);
        (max_wait_ms, active)
    }

    fn resolve_residual_workers(&self, _limits: &SearchLimits) -> (u64, usize, u64) {
        let expected_generation = self.shared_state.generation();
        self.shared_state.set_stop();
        self.shared_state.close_work_queues();

        let tm_snapshot = { self.time_manager.lock().unwrap().clone() };
        let wait_budget_ms = if let Some(ref tm) = tm_snapshot {
            // Use dynamic hygiene max based on remaining time
            let dynamic_max = compute_dynamic_hygiene_max(
                tm.elapsed_ms(),
                tm.hard_limit_ms(),
                tm.scheduled_end_ms(),
            );
            compute_hygiene_wait_budget(
                tm.elapsed_ms(),
                tm.hard_limit_ms(),
                tm.scheduled_end_ms(),
                dynamic_max,
            )
            .max(HYGIENE_WAIT_STEP_MS)
        } else {
            HYGIENE_WAIT_MAX_MS
        };

        let mut waited = 0;
        while waited < wait_budget_ms {
            let active = self.active_workers.load(Ordering::Acquire);
            let pending = self.pending_work_items.load(Ordering::Acquire);
            if active == 0 && pending == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(HYGIENE_WAIT_STEP_MS));
            waited += HYGIENE_WAIT_STEP_MS;
        }

        let remaining_active = self.active_workers.load(Ordering::Acquire);
        let remaining_pending = self.pending_work_items.load(Ordering::Acquire);
        debug_assert_eq!(
            expected_generation,
            self.shared_state.generation(),
            "SharedSearchState generation changed during residual worker resolution"
        );
        (waited, remaining_active, remaining_pending)
    }

    fn join_handles_blocking<I>(handles: I, label: &str, info_cb: Option<&InfoStringCallback>)
    where
        I: IntoIterator<Item = thread::JoinHandle<()>>,
    {
        let total_start = Instant::now();
        for handle in handles {
            let start = Instant::now();
            match handle.join() {
                Ok(()) => {
                    let elapsed = start.elapsed();
                    if elapsed >= Duration::from_millis(20) {
                        log::info!(
                            "diag join_complete label={} waited_ms={}",
                            label,
                            elapsed.as_millis()
                        );
                    }
                }
                Err(payload) => {
                    let panic_str = {
                        let any_ref = payload.as_ref();
                        if let Some(s) = any_ref.downcast_ref::<&str>() {
                            Some((*s).to_string())
                        } else {
                            any_ref.downcast_ref::<String>().cloned()
                        }
                    };
                    match panic_str {
                        Some(msg) => {
                            log::warn!("join_error label={} panic='{}'", label, msg);
                        }
                        None => {
                            log::warn!("join_error label={} panic=<non-string>", label);
                        }
                    }
                }
            }
        }
        let total_elapsed = total_start.elapsed().as_millis();
        // USI 側のコールバックがある場合は確実に info string として出力
        if let Some(cb) = info_cb {
            cb(&format!("join_all_complete label={} waited_total_ms={}", label, total_elapsed));
        } else {
            // ない場合は logger 経由で診断出力
            log::info!(
                "info string join_all_complete label={} waited_total_ms={}",
                label,
                total_elapsed
            );
        }
    }

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
                            session_id: limits.session_id, // Propagate session_id
                            stop_flag: limits.stop_flag.clone(),
                            info_callback: None, // Don't need callback for TimeManager
                            info_string_callback: None,
                            iteration_callback: None,
                            ponder_hit_flag: None,
                            qnodes_counter: limits.qnodes_counter.clone(),
                            immediate_eval_at_depth_zero: limits.immediate_eval_at_depth_zero,
                            multipv: limits.multipv,
                            enable_fail_safe: limits.enable_fail_safe,
                            fallback_deadlines: limits.fallback_deadlines,
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
                        let tm_handle = start_time_manager(
                            tm.clone(),
                            self.shared_state.clone(),
                            self.stop_bridge.clone(),
                        );
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
        if self.shared_state.should_stop() {
            return;
        }
        if self.shared_state.work_queues_closed() {
            return;
        }
        // Guard: avoid enqueueing more work if time has expired
        if let Some(tm) = &*self.time_manager.lock().unwrap() {
            let elapsed = tm.elapsed_ms();
            let hard = tm.hard_limit_ms();
            let planned = tm.scheduled_end_ms();
            if (hard > 0 && hard < u64::MAX && elapsed >= hard)
                || (planned > 0 && planned < u64::MAX && elapsed >= planned)
            {
                return;
            }
        }
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
                // Snapshot TM once; re-check only elapsed inside loop
                let tm_opt = { self.time_manager.lock().unwrap().clone() };
                while i < moves.len() {
                    if self.shared_state.should_stop() {
                        break;
                    }
                    if let Some(ref tm) = tm_opt {
                        let e = tm.elapsed_ms();
                        let h = tm.hard_limit_ms();
                        let p = tm.scheduled_end_ms();
                        if (h > 0 && h < u64::MAX && e >= h) || (p > 0 && p < u64::MAX && e >= p) {
                            break;
                        }
                        let guard_hard = compute_finalize_window_ms(h);
                        let guard_planned = if p > 0 && p < u64::MAX {
                            compute_finalize_window_ms(p)
                        } else {
                            0
                        };
                        if (h > 0 && h < u64::MAX && e.saturating_add(guard_hard) >= h)
                            || (p > 0 && p < u64::MAX && e.saturating_add(guard_planned) >= p)
                        {
                            if log::log_enabled!(log::Level::Debug) {
                                log::debug!(
                                    "diag batch_guard_tripped elapsed={}ms hard={}ms planned={}ms",
                                    e,
                                    h,
                                    if p == u64::MAX { 0 } else { p }
                                );
                            }
                            break;
                        }
                    }
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
                        if self.shared_state.work_queues_closed() {
                            break;
                        }
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

                    // Log enqueue summary for this iteration
                    if batch_idx > 0 {
                        let pending_now = self.pending_work_items.load(Ordering::Acquire);
                        let gen = self.shared_state.generation();
                        if log::log_enabled!(log::Level::Debug) {
                            debug!(
                                "enqueue_root_batches gen={} iter={} batch_count={} pending={}",
                                gen, iteration, batch_idx, pending_now
                            );
                        }
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

            let tm_opt = { self.time_manager.lock().unwrap().clone() };
            for helper_id in 1..self.num_threads {
                if self.shared_state.should_stop() {
                    break;
                }
                if let Some(ref tm) = tm_opt {
                    let e = tm.elapsed_ms();
                    let h = tm.hard_limit_ms();
                    let p = tm.scheduled_end_ms();
                    if (h > 0 && h < u64::MAX && e >= h) || (p > 0 && p < u64::MAX && e >= p) {
                        break;
                    }
                    let guard_hard = compute_finalize_window_ms(h);
                    let guard_planned = if p > 0 && p < u64::MAX {
                        compute_finalize_window_ms(p)
                    } else {
                        0
                    };
                    if (h > 0 && h < u64::MAX && e.saturating_add(guard_hard) >= h)
                        || (p > 0 && p < u64::MAX && e.saturating_add(guard_planned) >= p)
                    {
                        if log::log_enabled!(log::Level::Debug) {
                            log::debug!(
                                "diag batch_guard_tripped elapsed={}ms hard={}ms planned={}ms",
                                e,
                                h,
                                if p == u64::MAX { 0 } else { p }
                            );
                        }
                        break;
                    }
                }
                let helper_depth =
                    self.calculate_helper_depth(main_depth, helper_id, iteration, max_depth);

                let work = WorkItem::FullPosition {
                    iteration,
                    depth: helper_depth,
                    position: Arc::new(position.clone()),
                };

                if self.shared_state.work_queues_closed() {
                    let gen = self.shared_state.generation();
                    info!(
                        "drop_enqueue reason=closed gen={} iter={} helper={}",
                        gen, iteration, helper_id
                    );
                    break;
                }

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

            // Log enqueue summary for FullPosition mode
            let pending_now = self.pending_work_items.load(Ordering::Acquire);
            let gen = self.shared_state.generation();
            info!(
                "enqueue_full_position gen={} iter={} helpers={} pending={}",
                gen,
                iteration,
                self.num_threads - 1,
                pending_now
            );
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
                let mut pv_to_send = if best_result.stats.pv.is_empty() {
                    if let Some(best_move) = best_result.best_move {
                        vec![best_move]
                    } else {
                        Vec::new()
                    }
                } else {
                    best_result.stats.pv.clone()
                };
                if pv_to_send.is_empty() {
                    if let Some(m) = self.shared_state.get_best_move() {
                        pv_to_send.push(m);
                    }
                }

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

                // Also emit a compact heartbeat via info_string for diagnostics/GUI
                if let Some(ref cb2) = limits.info_string_callback {
                    let depth = self.shared_state.get_best_depth().max(1);
                    let elapsed_ms = search_start.elapsed().as_millis();
                    let sid = self.shared_state.generation();
                    let pv0_usi = pv_to_send
                        .first()
                        .map(crate::usi::move_to_usi)
                        .unwrap_or_else(|| "-".to_string());
                    cb2(&format!(
                        "hb sid={} depth={} nodes={} elapsed_ms={} pv0={}",
                        sid, depth, total_nodes, elapsed_ms, pv0_usi
                    ));
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
                let elapsed_ms = tm.elapsed_ms();
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
            let stop = self.shared_state.should_stop();
            let tm_present = { self.time_manager.lock().unwrap().is_some() };

            // 時間管理ありの stop モードでは、active==0 を満たした時点で即抜け
            if stop && tm_present && active == 0 {
                debug!(
                    "Stop mode: all workers inactive; finishing wait regardless of pending={pending}"
                );
                self.emit_info_string(
                    limits,
                    format!(
                        "wait_skip_pending=1 pending={} elapsed_ms={}",
                        pending,
                        search_start.elapsed().as_millis()
                    ),
                );
                #[cfg(feature = "diagnostics")]
                {
                    info!(
                        "info string wait_skip_pending=1 pending={} elapsed_ms={}",
                        pending,
                        search_start.elapsed().as_millis()
                    );
                }
                break;
            }

            // Check TimeManager should_stop first
            {
                let tm_opt = { self.time_manager.lock().unwrap().clone() };
                if let Some(ref tm) = tm_opt {
                    let nodes = self.shared_state.get_nodes();
                    if tm.should_stop(nodes) {
                        debug!(
                            "TimeManager triggered stop during wait (pending: {pending}, active: {active})"
                        );
                        let stop_info = StopInfo {
                            reason: TerminationReason::TimeLimit,
                            elapsed_ms: tm.elapsed_ms(),
                            nodes,
                            depth_reached: self.shared_state.get_best_depth(),
                            hard_timeout: false,
                            soft_limit_ms: tm.soft_limit_ms(),
                            hard_limit_ms: tm.hard_limit_ms(),
                        };
                        self.shared_state.set_stop_with_reason(stop_info);
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
    pub fn new(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        num_threads: usize,
        stop_bridge: Arc<EngineStopBridge>,
    ) -> Self {
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
            max_threads: num_threads,
            queues,
            active_workers: Arc::new(AtomicUsize::new(0)),
            steal_success: Arc::new(AtomicU64::new(0)),
            steal_failure: Arc::new(AtomicU64::new(0)),
            post_ponder_tm_handle: Arc::new(Mutex::new(None)),
            pending_work_items: Arc::new(AtomicU64::new(0)),
            stop_bridge,
        }
    }

    /// Set time manager for the search (compatibility method)
    pub fn set_time_manager(&mut self, time_manager: Arc<TimeManager>) {
        *self.time_manager.lock().unwrap() = Some(time_manager);
    }

    /// Adjust the number of active threads dynamically
    pub fn adjust_thread_count(&mut self, new_active_threads: usize) {
        let new_active = new_active_threads.min(self.max_threads).max(1);
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
        let random_offset = if (helper_id + iteration).is_multiple_of(4) {
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

        let residual_active = self.active_workers.load(Ordering::Acquire);
        let residual_pending = self.pending_work_items.load(Ordering::Acquire);
        let residual_finalized = self.shared_state.is_finalized_early();
        if residual_active != 0 || residual_pending != 0 {
            let sid = self.shared_state.generation();
            warn!(
                "Residual workers detected before new search: sid={} active={} pending={} finalized_early={}",
                sid,
                residual_active,
                residual_pending,
                residual_finalized
            );
            self.emit_info_string(
                &limits,
                format!(
                    "search_residual_workers=1 sid={} active={} pending={} finalized_early={}",
                    sid, residual_active, residual_pending, residual_finalized as u8
                ),
            );

            let (waited_ms, remaining_active, remaining_pending) =
                self.resolve_residual_workers(&limits);

            self.emit_info_string(
                &limits,
                format!(
                    "search_residual_workers_resolved=1 sid={} waited_ms={} remaining_active={} remaining_pending={}",
                    sid,
                    waited_ms,
                    remaining_active,
                    remaining_pending
                ),
            );

            if remaining_active != 0 {
                warn!(
                    "Residual workers persisted after hygiene wait: active={} pending={}",
                    remaining_active, remaining_pending
                );
            }
        }

        #[cfg(debug_assertions)]
        if limits.stop_flag.is_none() {
            log::warn!(
                "limits.stop_flag not wired; parallel stop propagation may be delayed in tests"
            );
        }

        // Record start time for fail-safe
        let search_start = Instant::now();

        // Reset pending work accounting with a fresh counter per session
        self.pending_work_items = Arc::new(AtomicU64::new(0));

        // Wire external stop_flag (from USI) into SharedSearchState for this search session.
        // Without this, GUI-issued `stop` does not propagate to parallel workers,
        // and the frontend may time out and fall back to fast finalize.

        // IMPORTANT: Each ParallelSearcher instance is created fresh per search by Engine,
        // so generation always starts at 0. We must ALWAYS wire the ext_stop.
        if let Some(ext_stop) = limits.stop_flag.clone() {
            // Log pre-reset state
            let pre_gen = self.shared_state.generation();
            let pre_ext_stop = ext_stop.load(Ordering::Acquire);
            let pre_shared_stop = self.shared_state.should_stop();
            let pre_queues_closed = self.shared_state.work_queues_closed();
            let needs_rewire = !Arc::ptr_eq(&self.shared_state.stop_flag, &ext_stop);
            let searcher_addr = self as *const _ as usize;
            self.emit_info_string(
                &limits,
                format!(
                    "pre_reset sid={} gen={} ext_stop={} shared_stop={} queues_closed={} needs_rewire={} searcher_addr=0x{:x}",
                    limits.session_id, pre_gen, pre_ext_stop, pre_shared_stop, pre_queues_closed, needs_rewire as u8, searcher_addr
                ),
            );

            // IMPORTANT: Ensure ext_stop is false before resetting
            ext_stop.store(false, Ordering::Release);

            // Only recreate SharedSearchState if we need to wire a different stop flag
            // Otherwise, reuse existing SharedSearchState to keep workers alive
            if needs_rewire {
                // Recreate shared_state with the provided stop flag
                self.shared_state = Arc::new(SharedSearchState::with_threads(
                    Arc::clone(&ext_stop),
                    self.num_threads,
                ));
            }

            // reset() increments generation and clears counters
            // This will cause old workers (if any) to exit via generation mismatch
            self.shared_state.reset();
            self.shared_state.reopen_work_queues();

            // Log post-reset state
            let post_gen = self.shared_state.generation();
            let post_shared_stop = self.shared_state.should_stop();
            let post_queues_closed = self.shared_state.work_queues_closed();
            let arc_eq = Arc::ptr_eq(&ext_stop, &self.shared_state.stop_flag) as u8;
            self.emit_info_string(
                &limits,
                format!(
                    "post_reset sid={} gen={} shared_stop={} queues_closed={} arc_eq={}",
                    limits.session_id, post_gen, post_shared_stop, post_queues_closed, arc_eq
                ),
            );

            self.stop_bridge.publish_session(
                &self.shared_state,
                &self.pending_work_items,
                Some(&ext_stop),
                limits.session_id, // Use session_id from limits (set by Engine)
            );

            self.emit_info_string(
                &limits,
                format!(
                    "session_published sid={} gen={} finalizer_present=1",
                    limits.session_id, post_gen
                ),
            );
        } else {
            // Log pre-reset state
            let pre_gen = self.shared_state.generation();
            let pre_shared_stop = self.shared_state.should_stop();
            let pre_queues_closed = self.shared_state.work_queues_closed();
            self.emit_info_string(
                &limits,
                format!(
                    "pre_reset sid={} gen={} shared_stop={} queues_closed={} no_ext_stop=1",
                    limits.session_id, pre_gen, pre_shared_stop, pre_queues_closed
                ),
            );

            // Ensure clean state when no external flag is provided.
            self.shared_state.reset();
            self.shared_state.reopen_work_queues();

            // Log post-reset state
            let post_gen = self.shared_state.generation();
            let post_shared_stop = self.shared_state.should_stop();
            let post_queues_closed = self.shared_state.work_queues_closed();
            self.emit_info_string(
                &limits,
                format!(
                    "post_reset sid={} gen={} shared_stop={} queues_closed={} no_ext_stop=1",
                    limits.session_id, post_gen, post_shared_stop, post_queues_closed
                ),
            );

            self.stop_bridge.publish_session(
                &self.shared_state,
                &self.pending_work_items,
                Some(&self.shared_state.stop_flag),
                limits.session_id, // Use session_id from limits (set by Engine)
            );

            self.emit_info_string(
                &limits,
                format!(
                    "session_published sid={} gen={} finalizer_present=0",
                    limits.session_id, post_gen
                ),
            );
        }

        // Reset counters for this session
        self.steal_success.store(0, Ordering::Release);
        self.steal_failure.store(0, Ordering::Release);

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
        // Snapshot shared TimeManager (if any) for workers
        let shared_tm_snapshot = { self.time_manager.lock().unwrap().clone() };
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
                time_manager: shared_tm_snapshot.clone(),
                shared_state: self.shared_state.clone(),
                queues: self.queues.clone(),
                active_workers: self.active_workers.clone(),
                steal_success: self.steal_success.clone(),
                steal_failure: self.steal_failure.clone(),
                pending_work_items: self.pending_work_items.clone(),
            };
            handles.push(start_worker_with(config));
        }

        // Log worker spawn completion
        self.emit_info_string(
            &limits,
            format!(
                "workers_spawned sid={} gen={} count={} should_stop={}",
                limits.session_id,
                self.shared_state.generation(),
                handles.len(),
                self.shared_state.should_stop()
            ),
        );

        // Start time management if needed
        let mut time_handle = {
            let tm_guard = self.time_manager.lock().unwrap();
            if let Some(ref tm) = *tm_guard {
                // Start time manager if we have any time limit
                // Even for short searches, we need time control to work properly
                if tm.soft_limit_ms() > 0 {
                    Some(start_time_manager(
                        tm.clone(),
                        self.shared_state.clone(),
                        self.stop_bridge.clone(),
                    ))
                } else {
                    debug!("Skipping time manager (soft_limit: 0ms)");
                    None
                }
            } else {
                None
            }
        };

        // Start fail-safe guard thread
        let mut fail_safe_handle = if limits.enable_fail_safe {
            Some(start_fail_safe_guard(search_start, limits.clone(), self.shared_state.clone()))
        } else {
            None
        };

        // Preserve info string callback for post-search diagnostics before moving limits
        let info_string_cb = limits.info_string_callback.clone();

        // Main thread does iterative deepening and generates work
        let result = self.run_main_thread(position, limits, main_worker);

        let early_finalized = self.shared_state.is_finalized_early();

        // Stop all threads
        info!(
            "Search complete{}; stopping threads",
            if early_finalized {
                " (early finalize)"
            } else {
                ""
            }
        );
        self.shared_state.set_stop();
        self.shared_state.close_work_queues();

        let mut worker_handles = handles;

        if early_finalized {
            let stop_info_snapshot = self.shared_state.stop_info.get();
            let tm_snapshot = { self.time_manager.lock().unwrap().clone() };
            let (elapsed_ms, hard_limit_ms) = stop_info_snapshot
                .as_ref()
                .map(|info| (info.elapsed_ms, info.hard_limit_ms))
                .or_else(|| tm_snapshot.as_ref().map(|tm| (tm.elapsed_ms(), tm.hard_limit_ms())))
                .unwrap_or((0, u64::MAX));
            let planned_limit_ms =
                tm_snapshot.as_ref().map(|tm| tm.scheduled_end_ms()).unwrap_or(u64::MAX);

            // Use dynamic hygiene max based on remaining time
            let dynamic_max =
                compute_dynamic_hygiene_max(elapsed_ms, hard_limit_ms, planned_limit_ms);
            let max_wait_ms = compute_hygiene_wait_budget(
                elapsed_ms,
                hard_limit_ms,
                planned_limit_ms,
                dynamic_max,
            );

            let (waited_ms, remaining_active) = self.wait_for_active_clear(max_wait_ms);
            let remaining_pending = self.pending_work_items.load(Ordering::Acquire);

            if let Some(cb) = info_string_cb.as_ref() {
                cb(&format!(
                    "fast_finalize_hygiene waited_ms={} remaining_active={} remaining_pending={}",
                    waited_ms, remaining_active, remaining_pending
                ));
            }

            if remaining_active == 0 {
                let drain = worker_handles.drain(..);
                Self::join_handles_blocking(drain, "worker_fast_finalize", info_string_cb.as_ref());
            } else {
                Self::join_handles_blocking(
                    worker_handles,
                    "worker_fast_finalize",
                    info_string_cb.as_ref(),
                );
            }
            self.active_workers.store(0, Ordering::Release);
            // 全ワーカ join 後：未取得の WorkItem は破棄（仕様）。カウンタを最終的に 0 へ揃える。
            self.pending_work_items.store(0, Ordering::Release);

            if let Some(handle) = time_handle.take() {
                if let Err(err) = handle.join() {
                    log::warn!("time_manager_join_error_fast_finalize err={:?}", err);
                }
            }
            if let Some(handle) = fail_safe_handle.take() {
                if let Err(err) = handle.join() {
                    log::warn!("fail_safe_join_error_fast_finalize err={:?}", err);
                }
            }
            if let Some(handle) = self.post_ponder_tm_handle.lock().unwrap().take() {
                if let Err(err) = handle.join() {
                    log::warn!("post_ponder_tm_join_error_fast_finalize err={:?}", err);
                }
            }

            *self.time_manager.lock().unwrap() = None;
        } else {
            Self::join_handles_blocking(worker_handles, "worker_finalize", info_string_cb.as_ref());
            self.active_workers.store(0, Ordering::Release);
            // 通常終了でも未取得 WorkItem が残る場合があるため、最終的に 0 へ揃える（テスト仕様）
            self.pending_work_items.store(0, Ordering::Release);
            if let Some(handle) = time_handle.take() {
                if let Err(err) = handle.join() {
                    log::warn!("time_manager_join_error err={:?}", err);
                }
            }
            if let Some(handle) = fail_safe_handle.take() {
                if let Err(err) = handle.join() {
                    log::warn!("fail_safe_join_error err={:?}", err);
                }
            }
            if let Some(handle) = self.post_ponder_tm_handle.lock().unwrap().take() {
                if let Err(err) = handle.join() {
                    log::warn!("post_ponder_tm_join_error err={:?}", err);
                }
            }
            *self.time_manager.lock().unwrap() = None;
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

        self.stop_bridge.clear();

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
        main_thread.generation = self.shared_state.generation();
        // Attach shared TimeManager to main thread searcher if available
        if let Some(tm) = { self.time_manager.lock().unwrap().clone() } {
            main_thread.attach_time_manager(tm);
        }

        let max_depth = limits.depth.unwrap_or(255);
        let fallback_deadlines = limits.fallback_deadlines;

        // Publish an initial minimal snapshot so USI can fast-finalize immediately if needed
        self.shared_state.publish_minimal_snapshot(position.zobrist_hash(), 0);

        // Heartbeat tracking for periodic info callbacks
        let mut last_heartbeat = Instant::now();
        let mut last_heartbeat_nodes = 0u64;
        let mut warned_pv_mismatch = false;

        // Iterative deepening
        for iteration in 1.. {
            if iteration == 1 {
                let stop_flag_value = limits
                    .stop_flag
                    .as_ref()
                    .map(|flag| flag.load(Ordering::Acquire))
                    .unwrap_or(false);
                let shared_stop = self.shared_state.should_stop();
                let pending = self.pending_work_items.load(Ordering::Acquire);
                let active = self.active_workers.load(Ordering::Acquire);
                self.emit_info_string(
                    &limits,
                    format!(
                        "id_loop_start sid={} gen={} stop_flag={} shared_stop={} pending={} active={}",
                        limits.session_id,
                        self.shared_state.generation(),
                        stop_flag_value,
                        shared_stop,
                        pending,
                        active
                    ),
                );
            }
            if limits
                .stop_flag
                .as_ref()
                .map(|flag| flag.load(Ordering::Acquire))
                .unwrap_or(false)
            {
                self.broadcast_stop(limits.stop_flag.as_ref());
                return self.prepare_final_result(position, best_result);
            }

            // Check for ponderhit and create TimeManager dynamically if needed
            self.handle_ponderhit_time_management(position, &limits);
            // If TM was just created on ponderhit, attach it to the main thread searcher (idempotent)
            if let Some(tm) = { self.time_manager.lock().unwrap().clone() } {
                main_thread.attach_time_manager(tm);
            }

            let tm_opt = { self.time_manager.lock().unwrap().clone() };
            let is_ponder = matches!(limits.time_control, TimeControl::Ponder(_));
            if tm_opt.is_none() && !is_ponder {
                if let Some(deadlines) = fallback_deadlines {
                    let now = Instant::now();
                    if now >= deadlines.hard_deadline {
                        self.emit_info_string(
                            &limits,
                            format!(
                                "fallback_deadline_trigger=hard elapsed_ms={} nodes={}",
                                search_start.elapsed().as_millis(),
                                self.shared_state.get_nodes()
                            ),
                        );
                        return self.finalize_fallback_deadline(
                            position,
                            best_result,
                            search_start,
                            deadlines,
                            true,
                        );
                    } else if let Some(soft_deadline) = deadlines.soft_deadline {
                        if now >= soft_deadline {
                            self.emit_info_string(
                                &limits,
                                format!(
                                    "fallback_deadline_trigger=soft elapsed_ms={} nodes={}",
                                    search_start.elapsed().as_millis(),
                                    self.shared_state.get_nodes()
                                ),
                            );
                            return self.finalize_fallback_deadline(
                                position,
                                best_result,
                                search_start,
                                deadlines,
                                false,
                            );
                        }
                    }
                }
            }

            if let Some(ref tm) = tm_opt {
                let elapsed_ms = tm.elapsed_ms();
                let current_nodes = self.shared_state.get_nodes();
                if let Some(action) =
                    self.assess_time_limit(tm, elapsed_ms, current_nodes, &best_result)
                {
                    tm.force_stop();
                    return self.finalize_time_limit(
                        position,
                        action,
                        limits.info_string_callback.as_ref(),
                    );
                }
            }

            if self.shared_state.should_stop() {
                if let Some(ref tm) = tm_opt {
                    let elapsed_ms = tm.elapsed_ms();
                    let current_nodes = self.shared_state.get_nodes();
                    if let Some(action) =
                        self.assess_time_limit(tm, elapsed_ms, current_nodes, &best_result)
                    {
                        tm.force_stop();
                        return self.finalize_time_limit(
                            position,
                            action,
                            limits.info_string_callback.as_ref(),
                        );
                    }
                }

                let user_stop_now =
                    limits.stop_flag.as_ref().map(|f| f.load(Ordering::Acquire)).unwrap_or(false);
                if user_stop_now {
                    self.broadcast_stop(limits.stop_flag.as_ref());
                    return self.prepare_final_result(position, best_result);
                } else {
                    self.shared_state.set_stop();
                    self.shared_state.close_work_queues();
                    return self.prepare_final_result(position, best_result);
                }
            }

            // Calculate depths for this iteration
            let main_depth = iteration.min(max_depth as usize) as u8;

            let iter_start = Instant::now();
            if log::log_enabled!(log::Level::Debug) {
                debug!("Starting iteration {iteration} (depth {main_depth})");
            }

            // Distribute work to helper threads (guarded by stop/time checks)
            if self.shared_state.should_stop() {
                break;
            }
            self.distribute_work_to_helpers(
                position,
                iteration,
                main_depth,
                max_depth,
                Some(&main_worker),
            );

            // Main thread searches (time/stop will be checked inside)
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

            // After finishing this iteration (or shallow step), snapshot publish happens below

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
            let improved = best_result.best_move.is_none()
                || result.score > best_result.score
                || (result.score == best_result.score
                    && result.stats.depth > best_result.stats.depth);
            if improved {
                best_result = result.clone();
                // Publish PV snapshot at commit point (single-writer: main thread only)
                use crate::search::snapshot::RootSnapshot;
                let nodes_total = self.shared_state.get_nodes();
                let elapsed_ms = search_start.elapsed().as_millis() as u32;
                let mut commit_pv = SmallVec::from_vec(best_result.stats.pv.clone());
                if commit_pv.is_empty() {
                    if let Some(bm) = best_result.best_move {
                        commit_pv.push(bm);
                    } else if let Some(prev) = self.shared_state.snapshot.try_read() {
                        if prev.search_id == self.shared_state.generation() && !prev.pv.is_empty() {
                            commit_pv = prev.pv;
                        }
                    }
                }
                let snap = RootSnapshot {
                    search_id: self.shared_state.generation(),
                    root_key: position.zobrist_hash(),
                    best: best_result.best_move,
                    pv: commit_pv,
                    depth: best_result.stats.depth,
                    score_cp: best_result.score,
                    nodes: nodes_total,
                    elapsed_ms,
                };
                self.emit_info_string(
                    &limits,
                    format!(
                        "snapshot_publish kind=pv_commit sid={} root_key={:016x} depth={} nodes={} pv_len={}",
                        snap.search_id,
                        snap.root_key,
                        snap.depth,
                        snap.nodes,
                        snap.pv.len()
                    ),
                );
                self.shared_state.snapshot.publish(&snap);
                warned_pv_mismatch = false;
            } else {
                // Preserve previous PV; refresh minimal fields only
                let elapsed_ms = search_start.elapsed().as_millis() as u32;
                if let Some(prev) = self.shared_state.snapshot.try_read() {
                    if let (Some(b), Some(pv0)) =
                        (self.shared_state.get_best_move(), prev.pv.first().copied())
                    {
                        // Use equals_without_piece_type to avoid false positives from piece type differences
                        if !b.equals_without_piece_type(&pv0) && !warned_pv_mismatch {
                            let b_usi = crate::usi::move_to_usi(&b);
                            let pv0_usi = crate::usi::move_to_usi(&pv0);
                            self.emit_info_string(
                                &limits,
                                format!(
                                    "snapshot_warn_pv_head_mismatch=1 sid={} best={} pv0={}",
                                    self.shared_state.generation(),
                                    b_usi,
                                    pv0_usi
                                ),
                            );
                            warned_pv_mismatch = true;
                        }
                    }
                }
                self.emit_info_string(
                    &limits,
                    format!(
                        "snapshot_publish kind=min_preserve sid={} root_key={:016x} elapsed_ms={}",
                        self.shared_state.generation(),
                        position.zobrist_hash(),
                        elapsed_ms
                    ),
                );
                self.shared_state
                    .publish_minimal_snapshot_preserve_pv(position.zobrist_hash(), elapsed_ms);
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

            // ハートビート直後は TM を再取得して評価（ponderhit 直後の作成を確実に拾う）
            if let Some(ref tm) = { self.time_manager.lock().unwrap().clone() } {
                let elapsed_ms = tm.elapsed_ms();
                let current_nodes = self.shared_state.get_nodes();
                if let Some(action) =
                    self.assess_time_limit(tm, elapsed_ms, current_nodes, &best_result)
                {
                    tm.force_stop();
                    return self.finalize_time_limit(
                        position,
                        action,
                        limits.info_string_callback.as_ref(),
                    );
                }
            }

            // Check depth limit
            if main_depth >= max_depth {
                info!("Main thread reached maximum depth {max_depth}, waiting for workers to complete...");

                // 先に停止をブロードキャスト（特に時間制御時のテール短縮）
                self.shared_state.set_stop();

                // Wait for workers to complete their work
                let wait_time = self.wait_for_workers_completion(
                    search_start,
                    &limits,
                    &mut last_heartbeat,
                    &best_result,
                );

                // 追加待ちは TimeManager がない場合（depth-only）に限定
                if self.time_manager.lock().unwrap().is_none() && wait_time < 2000 {
                    thread::sleep(Duration::from_millis(50));
                }

                info!("All workers completed, stopping search");
                break;
            }
        }

        self.prepare_final_result(position, best_result)
    }

    /// Finalize search due to approaching or exceeding time limits.
    fn finalize_time_limit(
        &self,
        position: &mut Position,
        params: TimeLimitFinalization,
        info_cb: Option<&InfoStringCallback>,
    ) -> SearchResult {
        // Publish a minimal snapshot before stopping so that USI fast path can read it
        self.shared_state.publish_minimal_snapshot_preserve_pv(
            position.zobrist_hash(),
            params.elapsed_ms as u32,
        );
        if log::log_enabled!(log::Level::Info) {
            log::info!(
                "diag near_hard_finalize label={} elapsed={}ms soft={}ms hard={}ms planned={}ms nodes={}",
                params.label,
                params.elapsed_ms,
                if params.soft_limit_ms == u64::MAX {
                    0
                } else {
                    params.soft_limit_ms
                },
                if params.hard_limit_ms == u64::MAX {
                    0
                } else {
                    params.hard_limit_ms
                },
                if params.planned_limit_ms == u64::MAX {
                    0
                } else {
                    params.planned_limit_ms
                },
                params.current_nodes
            );
        }

        if let Some(cb) = info_cb {
            cb(&format!(
                "near_hard_finalize=1 label={} elapsed={} soft={} hard={} planned={} nodes={}",
                params.label,
                params.elapsed_ms,
                if params.soft_limit_ms == u64::MAX {
                    0
                } else {
                    params.soft_limit_ms
                },
                if params.hard_limit_ms == u64::MAX {
                    0
                } else {
                    params.hard_limit_ms
                },
                if params.planned_limit_ms == u64::MAX {
                    0
                } else {
                    params.planned_limit_ms
                },
                params.current_nodes
            ));
        }

        self.shared_state.set_stop_with_reason(crate::search::types::StopInfo {
            reason: TerminationReason::TimeLimit,
            elapsed_ms: params.elapsed_ms,
            nodes: params.current_nodes,
            depth_reached: self.shared_state.get_best_depth(),
            hard_timeout: params.hard_timeout,
            soft_limit_ms: params.soft_limit_ms,
            hard_limit_ms: params.hard_limit_ms,
        });
        self.shared_state.mark_finalized_early();

        // Phase 2: Immediate stop propagation (yaneura-style)
        // Do NOT zero pending_work_items here (workers may still fetch_sub -> underflow).
        // Instead, broadcast stop and close queues to signal all workers immediately.
        self.shared_state.set_stop();
        self.shared_state.close_work_queues();

        #[cfg(feature = "diagnostics")]
        {
            log::info!(
                "info string near_hard_finalize=1 label={} elapsed={} soft={} hard={} planned={}",
                params.label,
                params.elapsed_ms,
                if params.soft_limit_ms == u64::MAX {
                    0
                } else {
                    params.soft_limit_ms
                },
                if params.hard_limit_ms == u64::MAX {
                    0
                } else {
                    params.hard_limit_ms
                },
                if params.planned_limit_ms == u64::MAX {
                    0
                } else {
                    params.planned_limit_ms
                }
            );
        }

        self.prepare_final_result(position, params.best_snapshot)
    }

    /// Evaluate whether the current timing requires immediate finalization.
    fn assess_time_limit(
        &self,
        tm: &Arc<TimeManager>,
        elapsed_ms: u64,
        current_nodes: u64,
        best_snapshot: &SearchResult,
    ) -> Option<TimeLimitFinalization> {
        use crate::search::constants::MAIN_NEAR_DEADLINE_WINDOW_MS;

        let soft = tm.soft_limit_ms();
        let hard = tm.hard_limit_ms();
        let planned = tm.scheduled_end_ms();

        let hard_reached = hard > 0 && hard < u64::MAX && elapsed_ms >= hard;
        let planned_reached = planned > 0 && planned < u64::MAX && elapsed_ms >= planned;
        let tm_requested_stop = tm.should_stop(current_nodes);

        let near_hard_finalize = hard > 0
            && hard < u64::MAX
            && elapsed_ms.saturating_add(compute_finalize_window_ms(hard)) >= hard;
        let near_planned_finalize = planned > 0
            && planned < u64::MAX
            && elapsed_ms.saturating_add(compute_finalize_window_ms(planned)) >= planned;

        let near_guard = (planned > 0
            && planned < u64::MAX
            && elapsed_ms.saturating_add(MAIN_NEAR_DEADLINE_WINDOW_MS) >= planned)
            || (hard > 0
                && hard < u64::MAX
                && elapsed_ms.saturating_add(compute_hard_guard_ms(hard)) >= hard);

        // 優先順位: hard/planned 到達 > 近傍 finalize > TM の一般的な should_stop > guard
        // より具体的な理由を優先してラベル付けする（テスト容易性と診断の明確化のため）
        let label = if hard_reached {
            Some("hard_limit")
        } else if planned_reached {
            Some("planned_limit")
        } else if near_hard_finalize {
            Some("near_hard_finalize")
        } else if near_planned_finalize {
            Some("near_planned_finalize")
        } else if tm_requested_stop {
            Some("tm_should_stop")
        } else if near_guard {
            Some("near_guard")
        } else {
            None
        }?;

        Some(TimeLimitFinalization {
            best_snapshot: best_snapshot.clone(),
            elapsed_ms,
            current_nodes,
            soft_limit_ms: soft,
            hard_limit_ms: hard,
            planned_limit_ms: planned,
            hard_timeout: hard_reached,
            label: label.to_string(),
        })
    }

    /// Merge shared-state data into the best search result and ensure a legal move exists.
    fn prepare_final_result(
        &self,
        position: &mut Position,
        mut best_result: SearchResult,
    ) -> SearchResult {
        if let Some(shared_move) = self.shared_state.get_best_move() {
            let shared_score = self.shared_state.get_best_score();
            let shared_depth = self.shared_state.get_best_depth();

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

        if let Some(info) = self.shared_state.stop_info.get() {
            best_result = SearchResult::with_stop_info(
                best_result.best_move,
                best_result.score,
                best_result.stats,
                best_result.node_type,
                info.clone(),
            );
        }

        if best_result.best_move.is_none() {
            warn!(
                "No best move found despite searching {} nodes, using fallback",
                best_result.stats.nodes
            );
            let mg = MoveGenerator::new();
            if let Ok(moves) = mg.generate_all(position) {
                if let Some(&fallback_move) = moves.as_slice().first() {
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

#[cfg(test)]
mod tests_compute_guard_and_wait {
    use super::*;
    use crate::{
        evaluation::evaluate::MaterialEvaluator,
        search::parallel::util::{
            compute_hard_guard_ms, compute_hygiene_wait_budget, HYGIENE_WAIT_MAX_MS,
        },
        search::{SearchLimits, TranspositionTable},
    };

    #[test]
    fn test_compute_hard_guard_ms_piecewise() {
        use crate::search::constants::MAIN_NEAR_DEADLINE_WINDOW_MS;
        assert_eq!(compute_hard_guard_ms(2_000), MAIN_NEAR_DEADLINE_WINDOW_MS);
        assert_eq!(compute_hard_guard_ms(1_000), MAIN_NEAR_DEADLINE_WINDOW_MS);
        assert_eq!(compute_hard_guard_ms(800), 150);
        assert_eq!(compute_hard_guard_ms(500), 150);
        assert_eq!(compute_hard_guard_ms(400), 80);
        assert_eq!(compute_hard_guard_ms(200), 80);
        assert_eq!(compute_hard_guard_ms(180), 0);
        assert_eq!(compute_hard_guard_ms(0), 0);
    }

    #[test]
    fn test_compute_finalize_window_ms_piecewise() {
        use crate::search::constants::NEAR_HARD_FINALIZE_MS;
        use crate::search::parallel::util::{compute_finalize_window_ms, poll_tick_ms};
        assert_eq!(compute_finalize_window_ms(2_000), NEAR_HARD_FINALIZE_MS);
        assert_eq!(compute_finalize_window_ms(1_000), NEAR_HARD_FINALIZE_MS);
        assert_eq!(compute_finalize_window_ms(800), NEAR_HARD_FINALIZE_MS / 2);
        assert_eq!(compute_finalize_window_ms(500), NEAR_HARD_FINALIZE_MS / 2);
        assert_eq!(compute_finalize_window_ms(400), 120);
        assert_eq!(compute_finalize_window_ms(200), 120);
        assert_eq!(compute_finalize_window_ms(180), 10);
        assert_eq!(compute_finalize_window_ms(0), 0);
        assert_eq!(compute_finalize_window_ms(u64::MAX), 0);

        assert_eq!(poll_tick_ms(50), 5);
        assert_eq!(poll_tick_ms(200), 5);
        assert_eq!(poll_tick_ms(300), 10);
        assert_eq!(poll_tick_ms(900), 20);
        assert_eq!(poll_tick_ms(1_500), 20);
        assert_eq!(poll_tick_ms(u64::MAX), 20);
    }

    #[test]
    fn test_compute_hygiene_wait_budget_clips_by_remaining() {
        let default = HYGIENE_WAIT_MAX_MS;
        assert_eq!(compute_hygiene_wait_budget(900, 1_000, u64::MAX, default), 0);
        assert_eq!(compute_hygiene_wait_budget(880, 1_000, u64::MAX, default), 5);
        assert_eq!(compute_hygiene_wait_budget(0, 10_000, u64::MAX, default), default);
        assert_eq!(compute_hygiene_wait_budget(0, u64::MAX, u64::MAX, default), default);
    }

    #[test]
    fn test_compute_hygiene_wait_budget_prefers_planned_limit() {
        let default = HYGIENE_WAIT_MAX_MS;
        // planned limit が hard よりも近い場合は planned を優先
        assert_eq!(compute_hygiene_wait_budget(860, 2_000, 1_000, default), 25);
        // planned が既に尽きている場合は 0
        assert_eq!(compute_hygiene_wait_budget(995, u64::MAX, 1_000, default), 0);
    }

    #[test]
    fn test_wait_finishes_when_stopped_and_inactive() {
        // Arrange a ParallelSearcher and force stop with no active workers but non-zero pending.
        let evaluator = std::sync::Arc::new(MaterialEvaluator);
        let tt = std::sync::Arc::new(TranspositionTable::new(16));
        let mut searcher =
            ParallelSearcher::new(evaluator, tt, 2, std::sync::Arc::new(EngineStopBridge::new()));

        // Install a TimeManager to enable the stop-priority path (as used in time-control mode)
        use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 100 },
            ..Default::default()
        };
        let tm = std::sync::Arc::new(TimeManager::new(
            &limits,
            crate::Color::Black,
            0,
            GamePhase::Opening,
        ));
        searcher.set_time_manager(tm);

        // Force counters/state
        searcher.pending_work_items.store(42, std::sync::atomic::Ordering::Release);
        searcher.active_workers.store(0, std::sync::atomic::Ordering::Release);
        searcher.shared_state.set_stop();

        let start = std::time::Instant::now();
        let limits = SearchLimits::builder().depth(1).build();
        let mut last = start;
        let dummy_stats = SearchStats::default();
        let dummy_res = SearchResult::new(None, 0, dummy_stats);

        // Act
        let waited = searcher.wait_for_workers_completion(start, &limits, &mut last, &dummy_res);

        // Assert: should return quickly (ideally 0ms) regardless of pending.
        assert!(waited <= 20, "waited too long: {}ms", waited);
        // Counters untouched by the wait logic
        assert_eq!(searcher.active_workers.load(std::sync::atomic::Ordering::Acquire), 0);
    }
}

#[cfg(test)]
mod tests_assess_time_and_finalize {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::types::TerminationReason;
    use crate::search::TranspositionTable;
    use crate::shogi::Position;
    use std::sync::{atomic::AtomicBool, Arc};

    fn make_searcher() -> ParallelSearcher<MaterialEvaluator> {
        let evaluator = std::sync::Arc::new(MaterialEvaluator);
        let tt = std::sync::Arc::new(TranspositionTable::new(16));
        ParallelSearcher::new(evaluator, tt, 1, std::sync::Arc::new(EngineStopBridge::new()))
    }

    #[test]
    fn broadcast_stop_preserves_existing_reason() {
        let searcher = make_searcher();
        // 既に TimeLimit 理由が設定されている場合、理由は保持される。
        searcher.shared_state.set_stop_with_reason(crate::search::types::StopInfo {
            reason: TerminationReason::TimeLimit,
            elapsed_ms: 123,
            nodes: 456,
            depth_reached: 7,
            hard_timeout: false,
            soft_limit_ms: 0,
            hard_limit_ms: 0,
        });

        let flag = Arc::new(AtomicBool::new(false));
        searcher.broadcast_stop(Some(&flag));

        let info = searcher.shared_state.stop_info.get().cloned().unwrap();
        assert!(matches!(info.reason, TerminationReason::TimeLimit));
        // 仕様上、既存理由がある場合に external flag を必ず立てる必要はないため
        // フラグ状態は検証しない。
    }

    #[test]
    fn pending_counter_is_replaced_per_session() {
        let mut searcher = make_searcher();
        let old_ptr = Arc::as_ptr(&searcher.pending_work_items);
        let mut pos = Position::startpos();
        let limits = SearchLimits::builder().depth(1).build();
        let _ = searcher.search(&mut pos, limits);
        let new_ptr = Arc::as_ptr(&searcher.pending_work_items);
        assert_ne!(old_ptr, new_ptr, "pending counter should be replaced per session");
    }

    #[test]
    fn test_assess_time_limit_tm_should_stop() {
        use crate::time_management::{
            mock_set_time, GamePhase, TimeControl, TimeLimits, TimeManager,
        };

        mock_set_time(0);
        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 1000 },
            ..Default::default()
        };
        let tm = std::sync::Arc::new(TimeManager::new(
            &limits,
            crate::Color::Black,
            0,
            GamePhase::Opening,
        ));
        // Force immediate stop
        tm.force_stop();

        let searcher = make_searcher();
        let snapshot = SearchResult::new(None, 0, SearchStats::default());
        let action = searcher
            .assess_time_limit(&tm, tm.elapsed_ms(), 0, &snapshot)
            .expect("expected Some for tm_should_stop");
        assert_eq!(action.label, "tm_should_stop");
    }

    #[test]
    fn test_assess_time_limit_hard_limit() {
        use crate::time_management::{
            mock_set_time, GamePhase, TimeControl, TimeLimits, TimeManager,
        };

        mock_set_time(0);
        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 200 },
            ..Default::default()
        };
        let tm = std::sync::Arc::new(TimeManager::new(
            &limits,
            crate::Color::White,
            0,
            GamePhase::Opening,
        ));
        let hard = tm.hard_limit_ms();
        assert!(hard > 0 && hard != u64::MAX);
        mock_set_time(hard.saturating_add(1));

        let searcher = make_searcher();
        let snapshot = SearchResult::new(None, 0, SearchStats::default());
        let action = searcher
            .assess_time_limit(&tm, tm.elapsed_ms(), 0, &snapshot)
            .expect("expected Some for hard_limit");
        assert_eq!(action.label, "hard_limit");
        assert!(action.hard_timeout);
    }

    #[test]
    fn test_assess_time_limit_near_hard_finalize() {
        use crate::time_management::{
            mock_set_time, GamePhase, TimeControl, TimeLimits, TimeManager,
        };

        mock_set_time(0);
        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 1000 },
            ..Default::default()
        };
        let tm = std::sync::Arc::new(TimeManager::new(
            &limits,
            crate::Color::White,
            0,
            GamePhase::Opening,
        ));
        let hard = tm.hard_limit_ms();
        let window = compute_finalize_window_ms(hard);
        // Move time just inside the finalize window (but before hard)
        let elapsed = hard.saturating_sub(window).saturating_add(1);
        mock_set_time(elapsed);

        let searcher = make_searcher();
        let snapshot = SearchResult::new(None, 0, SearchStats::default());
        let action = searcher
            .assess_time_limit(&tm, tm.elapsed_ms(), 0, &snapshot)
            .expect("expected Some for near_hard_finalize");
        assert_eq!(action.label, "near_hard_finalize");
        assert!(!action.hard_timeout);
    }

    #[test]
    fn test_finalize_time_limit_marks_early() {
        let searcher = make_searcher();
        let mut pos = Position::startpos();
        let snapshot = SearchResult::new(None, 0, SearchStats::default());
        let params = TimeLimitFinalization {
            best_snapshot: snapshot,
            elapsed_ms: 123,
            current_nodes: 456,
            soft_limit_ms: u64::MAX,
            hard_limit_ms: u64::MAX,
            planned_limit_ms: u64::MAX,
            hard_timeout: false,
            label: "unit_test".to_string(),
        };

        let _ = searcher.finalize_time_limit(&mut pos, params, None);
        assert!(searcher.shared_state.is_finalized_early());
        let info = searcher.shared_state.stop_info.get().cloned().expect("stop info set");
        assert!(matches!(info.reason, TerminationReason::TimeLimit));
    }
}
