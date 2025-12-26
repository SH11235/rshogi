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

        pub fn update_tt(&self, tt: Arc<TranspositionTable>) {
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

#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
mod imp {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread::JoinHandle;

    use crate::position::Position;
    use crate::tt::TranspositionTable;
    use crate::types::Depth;

    use crate::search::engine::{search_helper, SearchProgress};
    use crate::search::{LimitsType, SearchWorker, TimeManagement, TimeOptions};

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

        pub fn update_tt(&self, tt: Arc<TranspositionTable>) {
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
            // Stack size for wasm32 threads: 2MB, matching JS-side DEFAULT_THREAD_STACK_SIZE.
            // This is smaller than native (64MB) due to wasm memory constraints.
            const SEARCH_STACK_SIZE: usize = 2 * 1024 * 1024;
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

#[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
mod imp {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use crate::position::Position;
    use crate::tt::TranspositionTable;
    use crate::types::Depth;

    use crate::search::{LimitsType, TimeOptions};

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
        }

        pub fn wait_for_search_finished(&self) {}

        pub fn clear_histories(&self) {}

        pub fn update_tt(&self, _tt: Arc<TranspositionTable>) {}

        pub fn helper_threads(&self) -> &[Thread] {
            &[]
        }
    }

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

pub use imp::*;
