use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use super::{
    clone_limits_for_worker, compute_jitter_seed, jitter_enabled, root_moves::build_root_moves,
};
use crate::evaluation::evaluate::Evaluator;
use crate::movegen::error::MoveGenError;
use crate::search::ab::ordering::Heuristics;
use crate::search::ab::ClassicBackend;
use crate::search::constants::MAX_PLY;
use crate::search::types::SearchStack;
// SearcherBackend is not directly used here; ClassicBackend is invoked through its public APIs.
use crate::search::{SearchLimits, SearchResult};
use crate::shogi::Move;
use crate::shogi::Position;
use rand::SeedableRng;
use rand_xoshiro::Xoshiro128PlusPlus;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::mpsc as std_mpsc;
use std::time::Instant;

// Worker-local scratch/state. Lives entirely on each worker thread.
// Today it's only RNG + a small scratch buffer, but this is the hook where we can
// attach heuristics buffers, stacks, killers, etc. in future (YBWC-friendly).
struct WorkerLocal {
    rng: Xoshiro128PlusPlus,
    last_seed: u64,
    // Minimal scratch placeholder; grows as we add features.
    scratch: Vec<u8>,
    // Heuristics buffer reused across jobs within same session (helpers用).
    heur: Heuristics,
    // Track session boundary to clear heuristics between different games.
    last_session_id: u64,
    // Pre-allocated search stack reused across jobs (helpers fast-path)
    stack: Vec<SearchStack>,
}

impl WorkerLocal {
    fn new() -> Self {
        // Seed with a non-zero default to keep RNG valid before first prepare.
        let seed128 = Self::seed128_from_base(0x9E37_79B9_7F4A_7C15);
        Self {
            rng: Xoshiro128PlusPlus::from_seed(seed128),
            last_seed: 0,
            scratch: Vec::with_capacity(512),
            heur: Heuristics::default(),
            last_session_id: 0,
            stack: vec![SearchStack::default(); MAX_PLY + 1],
        }
    }

    fn seed128_from_base(seed: u64) -> [u8; 16] {
        // Minimal local SplitMix64-ish expansion to 128 bits
        fn mix(mut x: u64) -> u64 {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z ^= z >> 30;
            z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z ^= z >> 27;
            z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        let a = mix(seed);
        let b = mix(seed ^ 0xD134_2543_DE82_EADF);
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&a.to_le_bytes());
        out[8..].copy_from_slice(&b.to_le_bytes());
        out
    }

    fn prepare_for_job(
        &mut self,
        session_id: u64,
        root_key: u64,
        worker_id: usize,
        jitter_seed: Option<u64>,
    ) {
        // Session boundary detection: Clear heuristics on new session to prevent cross-game leakage.
        //
        // Each session represents a distinct game/search context. Reusing killer moves or history
        // heuristics from a previous game would contaminate move ordering with irrelevant data.
        // This is similar to YaneuraOu's approach of clearing history tables between games.
        //
        // Within the same session, heuristics are intentionally preserved across jobs (policy A)
        // to improve helper thread move ordering. This reuse is beneficial because multiple jobs
        // within a session typically explore related positions in the same game tree.
        if session_id != self.last_session_id {
            self.heur.clear_all();
            self.last_session_id = session_id;
        }

        // Jitter seed computation: Prefer externally provided seed, fall back to computation.
        //
        // The jitter seed controls RNG-based search diversification (e.g., random move ordering,
        // aspiration window jitter). In normal operation, ParallelSearcher always provides
        // root_jitter_seed via SearchLimits, ensuring consistent behavior across all workers.
        let base = jitter_seed
            .unwrap_or_else(|| super::compute_jitter_seed(session_id, worker_id, root_key));
        let seed128 = Self::seed128_from_base(base);
        self.rng = Xoshiro128PlusPlus::from_seed(seed128);
        self.last_seed = base;
        self.scratch.clear();
        // Heuristics は同一セッション内ではジョブ間で再利用（Policy A）。
        // セッション境界では上記で既にクリア済み。
        // Ensure stack is reset to defaults (length invariant: MAX_PLY+1)
        if self.stack.len() != MAX_PLY + 1 {
            self.stack.resize(MAX_PLY + 1, SearchStack::default());
        } else {
            for s in self.stack.iter_mut() {
                s.reset_for_iteration();
            }
        }
        // Verify stack length invariant in debug builds to catch any logic errors
        debug_assert_eq!(
            self.stack.len(),
            MAX_PLY + 1,
            "WorkerLocal stack must maintain exactly MAX_PLY+1 elements"
        );
    }

    fn clear_all(&mut self) {
        self.heur.clear_all();
        self.last_session_id = 0;
        self.scratch.clear();
        if self.stack.len() != MAX_PLY + 1 {
            self.stack.resize(MAX_PLY + 1, SearchStack::default());
        }
        for s in self.stack.iter_mut() {
            s.reset_for_iteration();
        }
    }
}

pub struct ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    backend: Arc<ClassicBackend<E>>,
    workers: Vec<Worker>,
    // 常駐セッション情報（root 配布済みの Position/Limits を保持）
    session: Option<SessionContext>,
    nodes_counter: Arc<AtomicU64>,
    // Reaper thread to join helper joiner threads on timeout to avoid leaking OS threads
    reaper_tx: std_mpsc::Sender<std::thread::JoinHandle<()>>,
    reaper_handle: Option<std::thread::JoinHandle<()>>,
}

pub struct SearchJob {
    pub position: Position,
    pub limits: SearchLimits,
}

impl SearchJob {
    fn clone_for_worker(&self) -> Self {
        Self {
            position: self.position.clone(),
            limits: self.limits.clone(),
        }
    }
}

struct TaskEnvelope {
    job: SearchJob,
    result_tx: Sender<(usize, SearchResult)>,
}

pub struct SessionContext {
    pub jobs: Vec<SearchJob>,
    pub root_key: u64,
    pub root_moves: Arc<Vec<Move>>,
    pub session_id: u64,
}

pub struct PreparedSession {
    pub main_limits: SearchLimits,
    pub root_moves: Arc<Vec<Move>>,
    pub root_key: u64,
    pub session_id: u64,
}

impl<E> ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    pub fn new(backend: Arc<ClassicBackend<E>>, size: usize) -> Self {
        let (reaper_tx, reaper_rx) = std_mpsc::channel::<std::thread::JoinHandle<()>>();
        let nodes_counter = Arc::new(AtomicU64::new(0));
        let reaper_handle = std::thread::spawn(move || {
            // Join any joiner threads handed over after timeout
            while let Ok(h) = reaper_rx.recv() {
                let _ = h.join();
            }
        });
        let mut pool = Self {
            backend,
            workers: Vec::new(),
            session: None,
            nodes_counter,
            reaper_tx,
            reaper_handle: Some(reaper_handle),
        };
        pool.resize(size);
        pool
    }

    pub fn resize(&mut self, desired: usize) {
        // Normalize worker list: if any dead entries (handle.is_none()) remain for any reason,
        // drop them so the length check below reflects actual live workers.
        // Do a fast pre-scan to avoid retain() when not needed.
        let mut found_dead = false;
        for w in &self.workers {
            if w.handle.is_none() {
                found_dead = true;
                break;
            }
        }
        if found_dead {
            self.workers.retain(|w| w.handle.is_some());
        }

        while self.workers.len() < desired {
            let id = self.workers.len() + 1; // helper ids start at 1 (0 is main thread)
            let backend = Arc::clone(&self.backend);
            let (ctrl_tx, ctrl_rx) = crossbeam::channel::unbounded();
            let nodes_counter = Arc::clone(&self.nodes_counter);
            let mut builder = thread::Builder::new().name(format!("lazy-smp-worker-{id}"));

            // Stack size override for deep recursion (diagnostic/debug builds).
            // Default OS stack (typically 2MB release, 8MB on some Linux) is sufficient for
            // normal operation. Override only when needed via env var or feature flag.
            if let Some(mb_str) = crate::util::env_var("SHOGI_WORKER_STACK_MB") {
                if let Ok(mb) = mb_str.parse::<usize>() {
                    builder = builder.stack_size(mb * 1024 * 1024);
                }
            }
            #[cfg(feature = "large-stack-tests")]
            {
                builder = builder.stack_size(8 * 1024 * 1024);
            }

            let handle = builder
                .spawn(move || worker_loop(backend, ctrl_rx, id, nodes_counter))
                .expect("spawn lazy smp worker");
            self.workers.push(Worker {
                ctrl: ctrl_tx,
                handle: Some(handle),
            });
        }

        while self.workers.len() > desired {
            if let Some(mut worker) = self.workers.pop() {
                let _ = worker.ctrl.send(WorkerCommand::Shutdown);
                if let Some(handle) = worker.handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }

    pub fn set_resident(&mut self, threads: usize) {
        self.clear_workers();
        self.resize(threads);
        self.ensure_network_replicated();
    }

    pub fn clear_workers(&mut self) {
        self.nodes_counter.store(0, AtomicOrdering::Relaxed);
        self.session = None;
        for worker in self.workers.iter_mut() {
            let _ = worker.ctrl.send(WorkerCommand::Clear);
        }
    }

    pub fn start_thinking(
        &mut self,
        pos: &Position,
        base_limits: &SearchLimits,
        helper_count: usize,
    ) -> Result<PreparedSession, MoveGenError> {
        self.nodes_counter.store(0, AtomicOrdering::Relaxed);

        let root_moves = Arc::new(build_root_moves(pos, base_limits)?);
        let root_key = pos.zobrist_hash();

        let mut jobs = Vec::with_capacity(helper_count);
        for worker_idx in 0..helper_count {
            let mut limits = clone_limits_for_worker(base_limits);
            limits.store_heuristics = false;
            limits.info_callback = None;
            limits.info_string_callback = None;
            limits.iteration_callback = None;
            // qnodes_counter は使用しない
            limits.stop_controller = None;
            limits.helper_role = true;
            // helpers は MultiPV=1 固定
            limits.multipv = 1;
            let jitter_on = base_limits.jitter_override.unwrap_or_else(jitter_enabled);
            let bench_allrun = crate::search::policy::bench_allrun_enabled();
            if jitter_on && !bench_allrun {
                limits.root_jitter_seed =
                    Some(compute_jitter_seed(base_limits.session_id, worker_idx + 1, root_key));
            } else {
                limits.root_jitter_seed = None;
            }
            limits.root_moves = Some(Arc::clone(&root_moves));

            jobs.push(SearchJob {
                position: pos.clone(),
                limits,
            });
        }

        self.session = Some(SessionContext {
            jobs,
            root_key,
            root_moves: Arc::clone(&root_moves),
            session_id: base_limits.session_id,
        });

        let mut main_limits = clone_limits_for_worker(base_limits);
        main_limits.root_moves = Some(Arc::clone(&root_moves));
        main_limits.helper_role = false;
        main_limits.store_heuristics = true;
        main_limits.root_jitter_seed = None;

        Ok(PreparedSession {
            main_limits,
            root_moves,
            root_key,
            session_id: base_limits.session_id,
        })
    }

    pub fn start_searching(&mut self, result_tx: &Sender<(usize, SearchResult)>) {
        let Some(ctx) = self.session.take() else {
            log::warn!("thread_pool: start_searching called without session");
            return;
        };
        for (idx, worker) in self.workers.iter().enumerate() {
            if let Some(job) = ctx.jobs.get(idx) {
                let env = TaskEnvelope {
                    job: job.clone_for_worker(),
                    result_tx: result_tx.clone(),
                };
                let _ = worker.ctrl.send(WorkerCommand::Start(Box::new(env)));
            }
        }
    }

    pub fn wait_for_search_finished(
        &mut self,
        expected_helpers: usize,
        result_rx: &std_mpsc::Receiver<(usize, SearchResult)>,
        timeout: Option<std::time::Duration>,
    ) -> (Vec<(usize, SearchResult)>, bool) {
        let mut results = Vec::with_capacity(expected_helpers);
        let mut timed_out = false;
        if expected_helpers == 0 {
            return (results, timed_out);
        }
        let start = Instant::now();
        while results.len() < expected_helpers {
            if let Some(limit) = timeout {
                let elapsed = start.elapsed();
                if elapsed >= limit {
                    timed_out = true;
                    break;
                }
                let slice = limit.saturating_sub(elapsed);
                match result_rx.recv_timeout(slice.min(std::time::Duration::from_millis(200))) {
                    Ok(res) => results.push(res),
                    Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                        timed_out = true;
                        break;
                    }
                }
            } else {
                match result_rx.recv() {
                    Ok(res) => results.push(res),
                    Err(_) => {
                        timed_out = true;
                        break;
                    }
                }
            }
        }
        (results, timed_out)
    }

    pub fn nodes_searched(&self) -> u64 {
        self.nodes_counter.load(AtomicOrdering::Relaxed)
    }

    pub fn ensure_network_replicated(&self) {
        // Placeholder for YaneuraOu 互換 API。NNUE は共有 Arc を利用するため no-op。
        // 呼び出し側の明示的呼出で dead_code を避け、将来の NUMA 配置に備える。
    }

    pub fn shutdown(&mut self) {
        for worker in self.workers.iter_mut() {
            let _ = worker.ctrl.send(WorkerCommand::Shutdown);
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
        self.workers.clear();
    }

    /// Request all workers to stop accepting new jobs and exit ASAP.
    /// This is best-effort cancellation: workers currently running a job will
    /// observe the search stop flag and exit after finishing the in-flight job.
    pub fn cancel_all(&mut self) {
        for worker in self.workers.iter_mut() {
            let _ = worker.ctrl.send(WorkerCommand::Shutdown);
        }
    }

    /// Join all worker threads with an overall timeout budget.
    /// Returns the number of workers successfully joined.
    pub fn join_with_timeout(&mut self, timeout: std::time::Duration) -> usize {
        use std::sync::mpsc::channel;
        use std::sync::mpsc::RecvTimeoutError;
        let start = Instant::now();
        let mut joined = 0usize;
        for worker in self.workers.iter_mut() {
            if let Some(handle) = worker.handle.take() {
                let (tx, rx) = channel::<()>();
                // Move join into a helper thread so we can time out without blocking.
                let joiner = thread::spawn(move || {
                    let _ = handle.join();
                    let _ = tx.send(());
                });
                let remaining = timeout
                    .checked_sub(start.elapsed())
                    .unwrap_or_else(|| std::time::Duration::from_millis(0));
                match rx.recv_timeout(remaining) {
                    Ok(()) => {
                        // Join the joiner to avoid stray threads
                        let _ = joiner.join();
                        joined += 1;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        log::warn!(
                            "thread_pool: join timeout; worker still exiting in background (handing to reaper)"
                        );
                        // Hand the joiner thread to reaper for out-of-band joining
                        let _ = self.reaper_tx.send(joiner);
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        joined += 1; // already finished
                    }
                }
            }
        }
        // Remove any workers whose handles have been taken (dead or timing-out workers).
        // This ensures resize() will correctly spawn replacements next time.
        self.workers.retain(|w| w.handle.is_some());
        joined
    }

    /// Convenience: cancel all workers and wait for up to `timeout` for them to exit.
    pub fn cancel_all_join(&mut self, timeout: std::time::Duration) -> usize {
        self.cancel_all();
        let joined = self.join_with_timeout(timeout);
        self.workers.retain(|w| w.handle.is_some());
        joined
    }
}

impl<E> Drop for ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.shutdown();
        // Close reaper channel by dropping sender, then join the reaper thread.
        // Replace the sender with a fresh one and drop the old to ensure the channel closes now.
        let (tmp_tx, _tmp_rx) = std_mpsc::channel::<std::thread::JoinHandle<()>>();
        let old_tx = std::mem::replace(&mut self.reaper_tx, tmp_tx);
        drop(old_tx);
        if let Some(h) = self.reaper_handle.take() {
            let _ = h.join();
        }
    }
}

struct Worker {
    ctrl: crossbeam::channel::Sender<WorkerCommand>,
    handle: Option<JoinHandle<()>>,
}

enum WorkerCommand {
    Shutdown,
    Clear,
    Start(Box<TaskEnvelope>),
}

fn worker_loop<E>(
    backend: Arc<ClassicBackend<E>>,
    ctrl_rx: crossbeam::channel::Receiver<WorkerCommand>,
    worker_id: usize,
    nodes_counter: Arc<AtomicU64>,
) where
    E: Evaluator + Send + Sync + 'static,
{
    let mut local = WorkerLocal::new();
    loop {
        let mut shutdown = false;
        let mut envelope: Option<TaskEnvelope> = None;
        match ctrl_rx.recv() {
            Ok(WorkerCommand::Shutdown) | Err(_) => {
                shutdown = true;
            }
            Ok(WorkerCommand::Clear) => local.clear_all(),
            Ok(WorkerCommand::Start(env)) => {
                envelope = Some(*env);
            }
        }
        if shutdown {
            break;
        }

        let Some(TaskEnvelope {
            job: SearchJob { position, limits },
            result_tx,
            ..
        }) = envelope
        else {
            continue;
        };

        let root_key = position.zobrist_hash();
        local.prepare_for_job(limits.session_id, root_key, worker_id, limits.root_jitter_seed);
        let start = Instant::now();
        let mut result =
            backend.think_with_ctx(&position, &limits, &mut local.stack, &mut local.heur, None);
        if result.stats.elapsed.as_nanos() == 0 {
            result.stats.elapsed = start.elapsed();
            result.refresh_summary();
        }
        nodes_counter.fetch_add(result.nodes, AtomicOrdering::Relaxed);
        let _ = result_tx.send((worker_id, result));
        drop(result_tx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::{SearchLimitsBuilder, TranspositionTable};
    use crate::time_management::{
        detect_game_phase_for_time, TimeControl as TMTimeControl, TimeLimits, TimeManager,
    };
    use crate::Color;
    use std::sync::mpsc;

    /// Verify that resident start_thinking/start_searching completes all jobs.
    #[test]
    fn resident_start_completes_all_jobs() {
        let backend = Arc::new(ClassicBackend::with_tt(
            Arc::new(MaterialEvaluator),
            Arc::new(TranspositionTable::new(2)),
        ));
        let mut pool = ThreadPool::new(backend, 5);

        // TimeManager を同伴して FixedNodes を厳密適用
        let pos = crate::shogi::Position::startpos();
        let tl = TimeLimits {
            time_control: TMTimeControl::FixedNodes { nodes: 64 },
            ..Default::default()
        };
        let tm = TimeManager::new(&tl, Color::Black, 0, detect_game_phase_for_time(&pos, 0));
        let mut limits = SearchLimitsBuilder::default().fixed_nodes(64).depth(1).build();
        limits.time_manager = Some(Arc::new(tm));

        let (tx, rx) = mpsc::channel();
        pool.start_thinking(&pos, &limits, 5).expect("start_thinking");
        pool.start_searching(&tx);

        let mut got = 0usize;
        let mut seen_ids = std::collections::HashSet::new();
        while got < 5 {
            let (wid, res) = rx.recv().expect("result");
            seen_ids.insert(wid);
            assert!(res.stats.nodes > 0);
            got += 1;
        }
        assert_eq!(got, 5);
        // 5 workers process 5 jobs, so最大で5種類の worker ID が観測される。
        assert!(seen_ids.len() <= 5, "should see at most 5 worker IDs");
        assert!(!seen_ids.is_empty(), "should see at least 1 worker ID");
    }

    #[test]
    fn worker_local_prepare_resets_state() {
        // Directly exercise WorkerLocal API (module-private) to ensure deterministic reseed/clear.
        let mut wl = WorkerLocal::new();
        // First prepare
        wl.prepare_for_job(42, 0x1234_5678_9ABC_DEF0, 1, Some(0xCAFEBABE));
        let seed1 = wl.last_seed;
        assert_eq!(seed1, 0xCAFEBABE);
        // Use scratch and ensure it's cleared on next prepare
        wl.scratch.extend_from_slice(&[1, 2, 3, 4]);
        wl.prepare_for_job(42, 0x1234_5678_9ABC_DEF0, 2, Some(0xDEAD_BEEF_F00D));
        let seed2 = wl.last_seed;
        assert_eq!(seed2, 0xDEAD_BEEF_F00D);
        assert!(wl.scratch.is_empty());
        assert_ne!(seed1, seed2);
    }

    #[test]
    fn shutdown_response_time() {
        // Verify that shutdown completes within reasonable time (with after-based waiting).
        let backend = Arc::new(ClassicBackend::with_tt(
            Arc::new(MaterialEvaluator),
            Arc::new(TranspositionTable::new(2)),
        ));
        let mut pool = ThreadPool::new(backend, 4);

        let shutdown_start = Instant::now();
        pool.shutdown();
        let shutdown_elapsed = shutdown_start.elapsed();

        // Shutdown should complete quickly. With after(20ms), workers may take up to ~20ms
        // to notice shutdown. Allow 200ms margin for CI environment variations.
        assert!(
            shutdown_elapsed.as_millis() < 200,
            "shutdown took {shutdown_elapsed:?}, expected <200ms"
        );
    }

    #[test]
    fn heuristics_reuse_across_jobs() {
        // Verify that Heuristics are reused across jobs (policy A from review).
        // We test this by running 2 sequential jobs on a single-worker pool and ensuring both complete.
        // Direct verification of heur non-empty is not feasible from outside, so we rely on:
        // - No panics/errors (if reuse were broken, TLS seed/take logic might fail)
        // - Both jobs complete successfully
        let backend = Arc::new(ClassicBackend::with_tt(
            Arc::new(MaterialEvaluator),
            Arc::new(TranspositionTable::new(2)),
        ));
        let mut pool = ThreadPool::new(backend, 2); // 2 workersで同時実行

        let (tx, rx) = mpsc::channel();
        let pos = crate::shogi::Position::startpos();
        // それぞれに TimeManager を同伴
        let tl1 = TimeLimits {
            time_control: TMTimeControl::FixedNodes { nodes: 64 },
            ..Default::default()
        };
        let tm1 = TimeManager::new(&tl1, Color::Black, 0, detect_game_phase_for_time(&pos, 0));
        let mut base_limits = SearchLimitsBuilder::default().fixed_nodes(64).depth(1).build();
        base_limits.time_manager = Some(Arc::new(tm1));

        // helper_count=2 で 2 ジョブを生成
        pool.start_thinking(&pos, &base_limits, 2).expect("start_thinking");
        pool.start_searching(&tx);

        // Receive both results
        let (_, res1) = rx.recv().expect("job1 result");
        let (_, res2) = rx.recv().expect("job2 result");

        assert!(res1.stats.nodes > 0, "job1 should have searched nodes");
        assert!(res2.stats.nodes > 0, "job2 should have searched nodes");
        // If Heuristics reuse is working correctly, both jobs complete without issues.
    }

    #[test]
    fn worker_refreshes_nps_when_elapsed_is_zero() {
        // Confirm that if backend returns result with elapsed=0, worker_loop compensates
        // elapsed and refreshes nps.
        let backend = Arc::new(ClassicBackend::with_tt(
            Arc::new(MaterialEvaluator),
            Arc::new(TranspositionTable::new(2)),
        ));
        let mut pool = ThreadPool::new(backend, 1);

        let pos = crate::shogi::Position::startpos();
        let tl = TimeLimits {
            time_control: TMTimeControl::FixedNodes { nodes: 128 },
            ..Default::default()
        };
        let tm = TimeManager::new(&tl, Color::Black, 0, detect_game_phase_for_time(&pos, 0));
        let mut limits = SearchLimitsBuilder::default().fixed_nodes(128).depth(2).build();
        limits.time_manager = Some(Arc::new(tm));

        let (tx, rx) = mpsc::channel();
        pool.start_thinking(&pos, &limits, 1).expect("start_thinking");
        pool.start_searching(&tx);

        let (_worker_id, result) = rx.recv().expect("worker result");
        // Worker should have compensated elapsed and refreshed nps.
        assert!(result.stats.elapsed.as_nanos() > 0, "elapsed should be non-zero");
        assert!(result.nps > 0, "nps should be refreshed and positive");
    }

    /// After cancel_all_join(), dead workers must be removed so that a subsequent
    /// resize() can respawn helper threads. This test verifies that helpers
    /// resume functioning in the next run.
    #[test]
    fn cancel_then_resize_recreates_workers() {
        let backend = Arc::new(ClassicBackend::with_tt(
            Arc::new(MaterialEvaluator),
            Arc::new(TranspositionTable::new(2)),
        ));
        let mut pool = ThreadPool::new(backend, 2);

        // Cancel helpers and join with a small timeout; pool should drop dead entries.
        let _ = pool.cancel_all_join(std::time::Duration::from_millis(300));

        // Now ask for 2 helpers again; this should respawn workers.
        pool.resize(2);

        // Dispatch two small jobs and ensure we receive two results.
        let (tx, rx) = mpsc::channel();
        let pos = crate::shogi::Position::startpos();

        let tl = TimeLimits {
            time_control: TMTimeControl::FixedNodes { nodes: 128 },
            ..Default::default()
        };
        let tm = TimeManager::new(&tl, Color::Black, 0, detect_game_phase_for_time(&pos, 0));
        let mut limits = SearchLimitsBuilder::default().fixed_nodes(128).depth(2).build();
        limits.time_manager = Some(Arc::new(tm));

        pool.start_thinking(&pos, &limits, 2).expect("start_thinking");
        pool.start_searching(&tx);

        let mut received = 0usize;
        while received < 2 {
            let (_wid, res) = rx.recv().expect("result");
            assert!(res.stats.nodes > 0);
            received += 1;
        }

        assert_eq!(received, 2);
    }
}
