// Native build (non-Wasm) implementation.
// Uses std::thread for parallel LazySMP search with Condvar-based synchronization.
// Each helper thread runs in its own OS thread with a dedicated SearchWorker.
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread::JoinHandle;

    use crate::position::Position;
    use crate::tt::TranspositionTable;
    use crate::types::Depth;

    use crate::search::engine::{search_helper, SearchProgress};
    use crate::search::{LimitsType, SearchWorker, TimeManagement, TimeOptions};

    const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

    pub struct ThreadPool {
        threads: Vec<Thread>,
        stop: Arc<AtomicBool>,
        ponderhit: Arc<AtomicBool>,
    }

    impl ThreadPool {
        pub fn new(
            num_threads: usize,
            tt: Arc<TranspositionTable>,
            stop: Arc<AtomicBool>,
            ponderhit: Arc<AtomicBool>,
            max_moves_to_draw: i32,
        ) -> Self {
            let mut pool = Self {
                threads: Vec::new(),
                stop,
                ponderhit,
            };
            pool.set_num_threads(num_threads, tt, max_moves_to_draw);
            pool
        }

        pub fn set_num_threads(
            &mut self,
            num_threads: usize,
            tt: Arc<TranspositionTable>,
            max_moves_to_draw: i32,
        ) {
            let helper_count = num_threads.saturating_sub(1);
            if helper_count == self.threads.len() {
                return;
            }

            self.wait_for_search_finished();
            self.threads.clear();

            for id in 1..=helper_count {
                self.threads.push(Thread::new(
                    id,
                    Arc::clone(&tt),
                    Arc::clone(&self.stop),
                    Arc::clone(&self.ponderhit),
                    max_moves_to_draw,
                ));
            }
        }

        pub fn start_thinking(
            &self,
            pos: &Position,
            limits: LimitsType,
            max_depth: Depth,
            time_options: TimeOptions,
            max_moves_to_draw: i32,
            skill_enabled: bool,
        ) {
            if self.threads.is_empty() {
                return;
            }

            for thread in &self.threads {
                thread.start_searching(SearchTask {
                    pos: pos.clone(),
                    limits: limits.clone(),
                    max_depth,
                    time_options,
                    max_moves_to_draw,
                    skill_enabled,
                });
            }
        }

        pub fn wait_for_search_finished(&self) {
            for thread in &self.threads {
                thread.wait_for_search_finished();
            }
        }

        pub fn clear_histories(&self) {
            for thread in &self.threads {
                thread.clear_worker();
            }
            for thread in &self.threads {
                thread.wait_for_search_finished();
            }
        }

        pub fn update_tt(&mut self, tt: Arc<TranspositionTable>) {
            for thread in &self.threads {
                let tt = Arc::clone(&tt);
                thread.with_worker(|worker| {
                    worker.tt = tt;
                });
            }
        }

        pub fn helper_threads(&self) -> &[Thread] {
            &self.threads
        }
    }

    struct ThreadInner {
        worker: Mutex<Box<SearchWorker>>,
        state: Mutex<ThreadState>,
        condvar: Condvar,
        stop: Arc<AtomicBool>,
        ponderhit: Arc<AtomicBool>,
        progress: Arc<SearchProgress>,
    }

    struct ThreadState {
        searching: bool,
        exit: bool,
        task: Option<ThreadTask>,
    }

    enum ThreadTask {
        Search(Box<SearchTask>),
        ClearHistories,
    }

    struct SearchTask {
        pos: Position,
        limits: LimitsType,
        max_depth: Depth,
        time_options: TimeOptions,
        max_moves_to_draw: i32,
        skill_enabled: bool,
    }

    pub struct Thread {
        id: usize,
        inner: Arc<ThreadInner>,
        handle: Option<JoinHandle<()>>,
    }

    impl Thread {
        fn new(
            id: usize,
            tt: Arc<TranspositionTable>,
            stop: Arc<AtomicBool>,
            ponderhit: Arc<AtomicBool>,
            max_moves_to_draw: i32,
        ) -> Self {
            let worker = SearchWorker::new(tt, max_moves_to_draw, id);
            let progress = Arc::new(SearchProgress::new());
            let inner = Arc::new(ThreadInner {
                worker: Mutex::new(worker),
                state: Mutex::new(ThreadState {
                    searching: true,
                    exit: false,
                    task: None,
                }),
                condvar: Condvar::new(),
                stop,
                ponderhit,
                progress,
            });
            let inner_clone = Arc::clone(&inner);
            let handle = std::thread::Builder::new()
                .stack_size(SEARCH_STACK_SIZE)
                .spawn(move || idle_loop(inner_clone))
                .expect("failed to spawn search helper thread");

            let thread = Self {
                id,
                inner,
                handle: Some(handle),
            };
            thread.wait_for_search_finished();
            thread
        }

        pub fn id(&self) -> usize {
            self.id
        }

        fn start_searching(&self, task: SearchTask) {
            self.schedule_task(ThreadTask::Search(Box::new(task)));
        }

        fn clear_worker(&self) {
            self.schedule_task(ThreadTask::ClearHistories);
        }

        fn schedule_task(&self, task: ThreadTask) {
            let mut state = self.inner.state.lock().unwrap();
            while state.searching {
                state = self.inner.condvar.wait(state).unwrap();
            }
            state.task = Some(task);
            state.searching = true;
            self.inner.condvar.notify_one();
        }

        pub fn wait_for_search_finished(&self) {
            let mut state = self.inner.state.lock().unwrap();
            while state.searching {
                state = self.inner.condvar.wait(state).unwrap();
            }
        }

        pub fn with_worker<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&mut SearchWorker) -> R,
        {
            let mut worker = self.inner.worker.lock().unwrap();
            f(&mut worker)
        }

        pub fn nodes(&self) -> u64 {
            self.inner.progress.nodes()
        }

        pub fn best_move_changes(&self) -> f64 {
            self.inner.progress.best_move_changes()
        }
    }

    impl Drop for Thread {
        fn drop(&mut self) {
            {
                let mut state = self.inner.state.lock().unwrap();
                state.exit = true;
                state.searching = true;
                self.inner.condvar.notify_one();
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn idle_loop(inner: Arc<ThreadInner>) {
        loop {
            let task = {
                let mut state = inner.state.lock().unwrap();
                state.searching = false;
                inner.condvar.notify_all();

                while !state.searching && !state.exit {
                    state = inner.condvar.wait(state).unwrap();
                }

                if state.exit {
                    return;
                }

                state.task.take()
            };

            match task {
                Some(ThreadTask::Search(task)) => {
                    let task = *task;
                    inner.progress.reset();
                    let mut worker = inner.worker.lock().unwrap();
                    worker.max_moves_to_draw = task.max_moves_to_draw;
                    worker.prepare_search();

                    let mut pos = task.pos;
                    let mut time_manager =
                        TimeManagement::new(Arc::clone(&inner.stop), Arc::clone(&inner.ponderhit));
                    time_manager.set_options(&task.time_options);
                    time_manager.init(
                        &task.limits,
                        pos.side_to_move(),
                        pos.game_ply(),
                        task.max_moves_to_draw,
                    );

                    search_helper(
                        &mut worker,
                        &mut pos,
                        &task.limits,
                        &mut time_manager,
                        task.max_depth,
                        task.skill_enabled,
                        Some(&inner.progress),
                    );
                }
                Some(ThreadTask::ClearHistories) => {
                    inner.progress.reset();
                    let mut worker = inner.worker.lock().unwrap();
                    worker.clear();
                }
                None => {}
            }
        }
    }
}

// WASM builds without wasm-threads feature use single-threaded search only.
// See docs/wasm-multithreading-investigation.md for details.
//
// This module provides stub implementations of ThreadPool and Thread for API compatibility.
// Since there are no helper threads in single-threaded mode:
// - All ThreadPool methods are no-ops (empty implementations)
// - helper_threads() always returns an empty slice
// - The Thread struct exists only for type compatibility and is never instantiated
//
// The main thread search runs directly in Search::go() without any parallel helpers.
#[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
mod imp {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use crate::position::Position;
    use crate::tt::TranspositionTable;
    use crate::types::Depth;

    use crate::search::{LimitsType, TimeOptions};

    /// Stub ThreadPool for single-threaded Wasm builds.
    /// All methods are no-ops since there are no helper threads.
    pub struct ThreadPool {
        _stop: Arc<AtomicBool>,
        _ponderhit: Arc<AtomicBool>,
    }

    impl ThreadPool {
        pub fn new(
            _num_threads: usize,
            _tt: Arc<TranspositionTable>,
            stop: Arc<AtomicBool>,
            ponderhit: Arc<AtomicBool>,
            _max_moves_to_draw: i32,
        ) -> Self {
            // num_threads is ignored; single-threaded mode has no helpers
            Self {
                _stop: stop,
                _ponderhit: ponderhit,
            }
        }

        pub fn set_num_threads(
            &mut self,
            _num_threads: usize,
            _tt: Arc<TranspositionTable>,
            _max_moves_to_draw: i32,
        ) {
            // No-op: single-threaded mode ignores thread count
        }

        pub fn start_thinking(
            &self,
            _pos: &Position,
            _limits: LimitsType,
            _max_depth: Depth,
            _time_options: TimeOptions,
            _max_moves_to_draw: i32,
            _skill_enabled: bool,
        ) {
            // No-op: no helper threads to start
        }

        pub fn wait_for_search_finished(&self) {
            // No-op: no helper threads to wait for
        }

        pub fn clear_histories(&self) {
            // No-op: no helper thread workers to clear
        }

        pub fn update_tt(&mut self, _tt: Arc<TranspositionTable>) {
            // No-op: no helper thread workers to update
        }

        pub fn helper_threads(&self) -> &[Thread] {
            // Always empty: no helper threads exist
            &[]
        }
    }

    /// Stub Thread for single-threaded Wasm builds.
    /// This struct exists only for type compatibility and is never instantiated.
    pub struct Thread;

    impl Thread {
        pub fn id(&self) -> usize {
            0
        }

        pub fn with_worker<F, R>(&self, _f: F) -> R
        where
            F: FnOnce(&mut crate::search::SearchWorker) -> R,
        {
            unreachable!("thread pool is disabled on wasm32")
        }

        pub fn nodes(&self) -> u64 {
            0
        }

        pub fn best_move_changes(&self) -> f64 {
            0.0
        }
    }
}

// WASM builds with wasm-threads feature use Rayon for parallel search.
// This uses wasm-bindgen-rayon to handle Web Worker creation asynchronously,
// avoiding the Condvar/async mismatch that caused deadlocks with wasm_thread.
//
// Key design: start_thinking() uses rayon::spawn_fifo() to launch helper threads
// asynchronously, allowing main thread search to run in parallel with helpers.
// wait_for_search_finished() polls an atomic counter until all helpers complete.
//
// ## LazySMP Effectiveness
//
// The core LazySMP mechanism IS working:
// - All threads share the same TranspositionTable via Arc
// - Helper threads write their search results to the shared TT
// - Main thread benefits from TT entries discovered by helpers
// - This provides the essential speedup of parallel search
//
// ## Result Collection
//
// Helper threads push their results to a shared `helper_results` vector when
// they complete each depth. This allows get_best_thread_id() to consider
// helper results when selecting the best move.
//
// The TT sharing effect (the primary benefit of LazySMP) is NOT affected.
#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
mod imp {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use rayon::prelude::*;

    use crate::position::Position;
    use crate::tt::TranspositionTable;
    use crate::types::{Depth, Move, Value};

    use crate::search::engine::search_helper;
    use crate::search::{LimitsType, SearchWorker, TimeManagement, TimeOptions};

    // Thread-local storage for SearchWorker instances.
    // Each Rayon worker thread gets its own SearchWorker on first use.
    thread_local! {
        static THREAD_WORKER: RefCell<Option<Box<SearchWorker>>> = const { RefCell::new(None) };
    }

    /// Helper thread の探索結果を格納する構造体。
    /// 各 helper が探索完了時にこの情報を shared vector に push する。
    #[derive(Debug, Clone)]
    pub struct HelperResult {
        /// スレッドID (1-indexed for helpers)
        pub thread_id: usize,
        /// 探索ノード数
        pub nodes: u64,
        /// 最善手の変化量（時間管理用）
        pub best_move_changes: f64,
        /// 完了した探索深さ
        pub completed_depth: Depth,
        /// 最善手
        pub best_move: Move,
        /// 最善手のスコア
        pub best_score: Value,
    }

    /// Helper thread の進捗をリアルタイムで追跡する構造体。
    /// 各イテレーション完了時に更新され、info出力や時間管理で参照される。
    pub struct HelperProgress {
        nodes: AtomicU64,
        best_move_changes_bits: AtomicU64,
    }

    impl HelperProgress {
        pub fn new() -> Self {
            Self {
                nodes: AtomicU64::new(0),
                best_move_changes_bits: AtomicU64::new(0.0f64.to_bits()),
            }
        }

        pub fn reset(&self) {
            self.nodes.store(0, Ordering::Relaxed);
            self.best_move_changes_bits.store(0.0f64.to_bits(), Ordering::Relaxed);
        }

        pub fn update(&self, nodes: u64, best_move_changes: f64) {
            self.nodes.store(nodes, Ordering::Relaxed);
            self.best_move_changes_bits
                .store(best_move_changes.to_bits(), Ordering::Relaxed);
        }

        pub fn nodes(&self) -> u64 {
            self.nodes.load(Ordering::Relaxed)
        }

        pub fn best_move_changes(&self) -> f64 {
            f64::from_bits(self.best_move_changes_bits.load(Ordering::Relaxed))
        }
    }

    pub struct ThreadPool {
        num_threads: usize,
        tt: Arc<TranspositionTable>,
        stop: Arc<AtomicBool>,
        ponderhit: Arc<AtomicBool>,
        max_moves_to_draw: i32,
        /// Counter for pending helper thread tasks.
        /// Decremented when each helper thread completes its search.
        pending_tasks: Arc<AtomicUsize>,
        /// Helper threads の探索結果を収集するベクタ。
        /// 各 helper が探索完了時に結果を push し、get_best_thread_id() で参照される。
        helper_results: Arc<Mutex<Vec<HelperResult>>>,
        /// Helper threads の進捗をリアルタイムで追跡。
        /// 各イテレーション完了時に更新され、info出力や時間管理で参照される。
        helper_progress: Vec<Arc<HelperProgress>>,
    }

    impl ThreadPool {
        pub fn new(
            num_threads: usize,
            tt: Arc<TranspositionTable>,
            stop: Arc<AtomicBool>,
            ponderhit: Arc<AtomicBool>,
            max_moves_to_draw: i32,
        ) -> Self {
            let num_threads = num_threads.max(1);
            let helper_count = num_threads.saturating_sub(1);
            let helper_progress =
                (0..helper_count).map(|_| Arc::new(HelperProgress::new())).collect();
            Self {
                num_threads,
                tt,
                stop,
                ponderhit,
                max_moves_to_draw,
                pending_tasks: Arc::new(AtomicUsize::new(0)),
                helper_results: Arc::new(Mutex::new(Vec::new())),
                helper_progress,
            }
        }

        pub fn set_num_threads(
            &mut self,
            num_threads: usize,
            tt: Arc<TranspositionTable>,
            max_moves_to_draw: i32,
        ) {
            let num_threads = num_threads.max(1);
            let helper_count = num_threads.saturating_sub(1);
            // Resize helper_progress to match new thread count
            self.helper_progress
                .resize_with(helper_count, || Arc::new(HelperProgress::new()));
            self.helper_progress.truncate(helper_count);
            self.num_threads = num_threads;
            self.tt = tt;
            self.max_moves_to_draw = max_moves_to_draw;
        }

        /// Start helper threads for LazySMP parallel search.
        ///
        /// This method returns immediately after spawning helper threads.
        /// Helper threads run asynchronously and search the same position
        /// as the main thread, sharing the transposition table.
        ///
        /// Call `wait_for_search_finished()` after main thread search completes
        /// to ensure all helpers have finished.
        pub fn start_thinking(
            &self,
            pos: &Position,
            limits: LimitsType,
            max_depth: Depth,
            time_options: TimeOptions,
            max_moves_to_draw: i32,
            skill_enabled: bool,
        ) {
            // Clear previous results before starting new search
            // This must be done even when helper_count is 0, to prevent stale results
            // from being used after switching from multi-threaded to single-threaded mode.
            if let Ok(mut results) = self.helper_results.lock() {
                results.clear();
            }
            self.pending_tasks.store(0, Ordering::SeqCst);

            let helper_count = self.num_threads.saturating_sub(1);
            if helper_count == 0 {
                return;
            }

            // Set pending task count before spawning
            self.pending_tasks.store(helper_count, Ordering::SeqCst);

            // Reset all helper progress before starting new search
            for progress in &self.helper_progress {
                progress.reset();
            }

            // Spawn each helper thread asynchronously using rayon::spawn_fifo
            // Using spawn_fifo instead of spawn to avoid work-stealing where the
            // current thread (main search thread) might execute helper tasks,
            // which would delay the main thread search and cause time management issues.
            for thread_id in 1..=helper_count {
                let stop = Arc::clone(&self.stop);
                let ponderhit = Arc::clone(&self.ponderhit);
                let tt = Arc::clone(&self.tt);
                let pending = Arc::clone(&self.pending_tasks);
                let helper_results = Arc::clone(&self.helper_results);
                // thread_id is 1-indexed, so subtract 1 to get the progress index
                let progress = Arc::clone(&self.helper_progress[thread_id - 1]);
                let pos_clone = pos.clone();
                let limits_clone = limits.clone();

                rayon::spawn_fifo(move || {
                    THREAD_WORKER.with(|cell| {
                        let mut worker_opt = cell.borrow_mut();

                        // Initialize SearchWorker on first use
                        if worker_opt.is_none() {
                            *worker_opt = Some(SearchWorker::new(
                                Arc::clone(&tt),
                                max_moves_to_draw,
                                thread_id,
                            ));
                        }

                        let worker = worker_opt.as_mut().unwrap();

                        // Update worker state for this search
                        worker.tt = Arc::clone(&tt);
                        worker.max_moves_to_draw = max_moves_to_draw;
                        worker.prepare_search();

                        let mut search_pos = pos_clone;
                        let mut time_manager =
                            TimeManagement::new(Arc::clone(&stop), Arc::clone(&ponderhit));
                        time_manager.set_options(&time_options);
                        time_manager.init(
                            &limits_clone,
                            search_pos.side_to_move(),
                            search_pos.game_ply(),
                            max_moves_to_draw,
                        );

                        search_helper(
                            worker,
                            &mut search_pos,
                            &limits_clone,
                            &mut time_manager,
                            max_depth,
                            skill_enabled,
                            Some(&*progress),
                        );

                        // Collect result after search completes
                        let result = HelperResult {
                            thread_id,
                            nodes: worker.nodes,
                            best_move_changes: worker.best_move_changes,
                            completed_depth: worker.completed_depth,
                            best_move: worker.best_move,
                            best_score: worker
                                .root_moves
                                .get(0)
                                .map(|rm| rm.score)
                                .unwrap_or(Value::ZERO),
                        };
                        if let Ok(mut results) = helper_results.lock() {
                            results.push(result);
                        }
                    });

                    // Decrement pending count when this helper completes
                    pending.fetch_sub(1, Ordering::SeqCst);
                });
            }
            // Returns immediately - helpers run asynchronously
        }

        /// Wait for all helper threads to complete their search.
        ///
        /// This polls the pending task counter until all helpers have finished.
        /// Should be called after the main thread search completes and stop flag is set.
        pub fn wait_for_search_finished(&self) {
            // Spin-wait until all helpers complete
            // This is acceptable because:
            // 1. It only happens at search end (not during search)
            // 2. Helpers should finish quickly once stop flag is set
            while self.pending_tasks.load(Ordering::SeqCst) > 0 {
                std::hint::spin_loop();
            }
        }

        pub fn clear_histories(&self) {
            let helper_count = self.num_threads.saturating_sub(1);
            if helper_count == 0 {
                return;
            }

            // clear_histories can use synchronous parallel iteration
            // since it's called outside of search
            (1..=helper_count).into_par_iter().for_each(|_| {
                THREAD_WORKER.with(|cell| {
                    if let Some(worker) = cell.borrow_mut().as_mut() {
                        worker.clear();
                    }
                });
            });
        }

        pub fn update_tt(&mut self, tt: Arc<TranspositionTable>) {
            // Update self.tt so that subsequent start_thinking calls use the new TT.
            // Thread-local workers will get the updated TT reference when start_thinking
            // calls worker.tt = Arc::clone(&tt) for each helper.
            self.tt = tt;
        }

        pub fn helper_threads(&self) -> &[Thread] {
            // Rayon's thread-local model prevents exposing Thread objects.
            // Use helper_results() instead to get search results.
            &[]
        }

        /// Get the collected helper thread results.
        /// Results are collected when each helper thread completes its search.
        /// Call this after wait_for_search_finished() to get final results.
        pub fn helper_results(&self) -> Vec<HelperResult> {
            self.helper_results.lock().map(|guard| guard.clone()).unwrap_or_default()
        }

        /// Get the total nodes searched by all helper threads (realtime).
        /// This is updated each time a helper completes an iteration.
        pub fn helper_nodes(&self) -> u64 {
            self.helper_progress.iter().fold(0u64, |acc, p| acc.saturating_add(p.nodes()))
        }

        /// Get best_move_changes values from all helper threads (realtime).
        /// Returns a vector of (nodes, best_move_changes) for each helper.
        pub fn helper_best_move_changes(&self) -> Vec<f64> {
            self.helper_progress.iter().map(|p| p.best_move_changes()).collect()
        }
    }

    /// Stub Thread for wasm-threads builds.
    /// These methods exist for API compatibility but cannot return real values
    /// because Rayon workers use thread-local storage inaccessible from outside.
    pub struct Thread;

    impl Thread {
        pub fn id(&self) -> usize {
            0
        }

        pub fn with_worker<F, R>(&self, _f: F) -> R
        where
            F: FnOnce(&mut SearchWorker) -> R,
        {
            // LIMITATION: Cannot access thread-local SearchWorker from outside
            unreachable!("rayon thread pool does not expose individual threads")
        }

        pub fn nodes(&self) -> u64 {
            // LIMITATION: Returns 0; actual nodes are in thread-local workers
            // NPS display will be lower than actual
            0
        }

        pub fn best_move_changes(&self) -> f64 {
            // LIMITATION: Returns 0; actual value is in thread-local workers
            // Time management may be slightly less optimal
            0.0
        }
    }
}

pub use imp::*;
