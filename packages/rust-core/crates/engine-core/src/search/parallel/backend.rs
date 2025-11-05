use super::ParallelSearcher;
use crate::evaluation::evaluate::Evaluator;
use crate::search::api::{BackendSearchTask, InfoEventCallback, SearcherBackend};
use crate::search::{SearchLimits, SearchResult};
use crate::Position;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

/// SearcherBackend wrapper for ParallelSearcher
///
/// This adapter allows ParallelSearcher to be used as a SearcherBackend,
/// enabling seamless integration with the Engine API while maintaining
/// a single source of truth for search logic (ClassicBackend).
///
/// # Design
///
/// - Threads=1: Automatically uses single-threaded path (no overhead)
/// - Threads>1: Spawns helper threads via ThreadPool
/// - All search improvements are centralized in ClassicBackend
pub struct ParallelSearcherBackend<E: Evaluator + Send + Sync + 'static> {
    inner: Arc<RwLock<ParallelSearcher<E>>>,
}

impl<E: Evaluator + Send + Sync + 'static> ParallelSearcherBackend<E> {
    pub fn new(searcher: ParallelSearcher<E>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(searcher)),
        }
    }
}

impl<E: Evaluator + Send + Sync + 'static> SearcherBackend for ParallelSearcherBackend<E> {
    fn start_async(
        self: Arc<Self>,
        mut root: Position,
        limits: SearchLimits,
        info: Option<InfoEventCallback>,
        active_counter: Arc<AtomicUsize>,
    ) -> BackendSearchTask {
        let stop_flag =
            limits.stop_flag.clone().unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

        active_counter.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = mpsc::channel();
        let inner = Arc::clone(&self.inner);

        let handle = thread::Builder::new()
            .name(format!("parallel-search-{}", limits.session_id))
            .spawn(move || {
                struct Guard(Arc<AtomicUsize>);
                impl Drop for Guard {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                let _guard = Guard(active_counter);

                // Attach info callback to limits
                let mut limits_with_info = limits;
                limits_with_info.info_callback = info;

                let mut searcher = inner.write();
                let result = searcher.search(&mut root, limits_with_info);
                let _ = tx.send(result);
            })
            .expect("Failed to spawn parallel search thread");

        BackendSearchTask::new(stop_flag, rx, handle)
    }

    fn think_blocking(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<InfoEventCallback>,
    ) -> SearchResult {
        // Clone SearchLimits and attach info callback
        let limits_with_info = SearchLimits {
            time_control: limits.time_control.clone(),
            moves_to_go: limits.moves_to_go,
            depth: limits.depth,
            nodes: limits.nodes,
            qnodes_limit: limits.qnodes_limit,
            time_parameters: limits.time_parameters,
            random_time_ms: limits.random_time_ms,
            session_id: limits.session_id,
            start_time: limits.start_time,
            panic_time_scale: limits.panic_time_scale,
            contempt: limits.contempt,
            is_ponder: limits.is_ponder,
            stop_flag: limits.stop_flag.clone(),
            info_callback: info,
            info_string_callback: limits.info_string_callback.clone(),
            iteration_callback: limits.iteration_callback.clone(),
            ponder_hit_flag: limits.ponder_hit_flag.clone(),
            root_jitter_seed: limits.root_jitter_seed,
            jitter_override: limits.jitter_override,
            helper_role: limits.helper_role,
            store_heuristics: limits.store_heuristics,
            threads_hint: limits.threads_hint,
            stop_controller: limits.stop_controller.clone(),
            time_manager: limits.time_manager.clone(),
            fallback_deadlines: limits.fallback_deadlines,
            multipv: limits.multipv,
            enable_fail_safe: limits.enable_fail_safe,
            immediate_eval_at_depth_zero: limits.immediate_eval_at_depth_zero,
        };

        let mut searcher = self.inner.write();
        let mut root_clone = root.clone();
        searcher.search(&mut root_clone, limits_with_info)
    }

    fn update_threads(&self, n: usize) {
        let mut searcher = self.inner.write();
        searcher.adjust_thread_count(n);
    }

    fn update_hash(&self, _mb: usize) {
        // TT is managed by Engine, not by the backend
    }
}
