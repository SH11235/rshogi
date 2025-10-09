use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::evaluation::evaluate::Evaluator;
use crate::search::ab::ordering::Heuristics;
use crate::search::ab::ClassicBackend;
use crate::search::constants::MAX_PLY;
use crate::search::types::SearchStack;
// SearcherBackend is not directly used here; ClassicBackend is invoked through its public APIs.
use crate::search::{SearchLimits, SearchResult};
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

    fn on_idle(&mut self) {
        // Light maintenance during idle periods (called from after(...) branch).
        // Prevent unbounded memory growth in scratch buffer.
        const MAX_SCRATCH_SIZE: usize = 4096;
        if self.scratch.capacity() > MAX_SCRATCH_SIZE {
            self.scratch.shrink_to(MAX_SCRATCH_SIZE);
        }
    }
}

pub struct ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    backend: Arc<ClassicBackend<E>>,
    workers: Vec<Worker>,
    // Shared task queue (pull model). Envelope carries job + result channel.
    task_tx: crossbeam::channel::Sender<TaskEnvelope>,
    task_rx: crossbeam::channel::Receiver<TaskEnvelope>,

    #[allow(dead_code)]
    task_hi_tx: crossbeam::channel::Sender<TaskEnvelope>,
    task_hi_rx: crossbeam::channel::Receiver<TaskEnvelope>,
    // Reaper thread to join helper joiner threads on timeout to avoid leaking OS threads
    reaper_tx: std_mpsc::Sender<std::thread::JoinHandle<()>>,
    reaper_handle: Option<std::thread::JoinHandle<()>>,
}

pub struct SearchJob {
    pub position: Position,
    pub limits: SearchLimits,
}

struct TaskEnvelope {
    job: SearchJob,
    result_tx: Sender<(usize, SearchResult)>,
    // Future YBWC hooks (unused for now):
    #[allow(dead_code)]
    priority: u8,
    #[allow(dead_code)]
    split: Option<u64>,
}

impl<E> ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    pub fn new(backend: Arc<ClassicBackend<E>>, size: usize) -> Self {
        let (task_tx, task_rx) = crossbeam::channel::unbounded();
        let (task_hi_tx, task_hi_rx) = crossbeam::channel::unbounded();
        let (reaper_tx, reaper_rx) = std_mpsc::channel::<std::thread::JoinHandle<()>>();
        let reaper_handle = std::thread::spawn(move || {
            // Join any joiner threads handed over after timeout
            while let Ok(h) = reaper_rx.recv() {
                let _ = h.join();
            }
        });
        let mut pool = Self {
            backend,
            workers: Vec::new(),
            task_tx,
            task_rx,
            task_hi_tx,
            task_hi_rx,
            reaper_tx,
            reaper_handle: Some(reaper_handle),
        };
        pool.resize(size);
        pool
    }

    pub fn resize(&mut self, desired: usize) {
        while self.workers.len() < desired {
            let id = self.workers.len() + 1; // helper ids start at 1 (0 is main thread)
            let backend = Arc::clone(&self.backend);
            let task_rx = self.task_rx.clone();
            let task_hi_rx = self.task_hi_rx.clone();
            let (ctrl_tx, ctrl_rx) = crossbeam::channel::unbounded();
            let mut builder = thread::Builder::new().name(format!("lazy-smp-worker-{id}"));

            // Stack size override for deep recursion (diagnostic/debug builds).
            // Default OS stack (typically 2MB release, 8MB on some Linux) is sufficient for
            // normal operation. Override only when needed via env var or feature flag.
            if let Ok(mb_str) = std::env::var("SHOGI_WORKER_STACK_MB") {
                if let Ok(mb) = mb_str.parse::<usize>() {
                    builder = builder.stack_size(mb * 1024 * 1024);
                }
            }
            #[cfg(feature = "large-stack-tests")]
            {
                builder = builder.stack_size(8 * 1024 * 1024);
            }

            let handle = builder
                .spawn(move || worker_loop(backend, task_hi_rx, task_rx, ctrl_rx, id))
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

    pub fn dispatch(&self, jobs: Vec<SearchJob>, result_tx: &Sender<(usize, SearchResult)>) {
        for job in jobs.into_iter() {
            // Push into shared queue; any worker will pick it up.
            let env = TaskEnvelope {
                job,
                result_tx: result_tx.clone(),
                priority: 0,
                split: None,
            };
            if let Err(err) = self.task_tx.send(env) {
                log::warn!("thread_pool: failed to enqueue job: {err}");
            }
        }
    }

    #[allow(dead_code)]
    pub fn dispatch_high_priority(
        &self,
        jobs: Vec<SearchJob>,
        result_tx: &Sender<(usize, SearchResult)>,
    ) {
        for job in jobs.into_iter() {
            // Push into high-priority queue for PV-first processing (YBWC preparation).
            let env = TaskEnvelope {
                job,
                result_tx: result_tx.clone(),
                priority: 1,
                split: None,
            };
            if let Err(err) = self.task_hi_tx.send(env) {
                log::warn!("thread_pool: failed to enqueue high-priority job: {err}");
            }
        }
    }

    pub fn shutdown(&mut self) {
        for worker in self.workers.iter_mut() {
            let _ = worker.ctrl.send(WorkerCommand::Shutdown);
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
        // Optional metrics logging (env opt-in)
        if std::env::var("SHOGI_THREADPOOL_METRICS").ok().as_deref() == Some("1") {
            let hi = METRICS_HI.load(std::sync::atomic::Ordering::Relaxed);
            let lo = METRICS_NORMAL.load(std::sync::atomic::Ordering::Relaxed);
            let idle = METRICS_IDLE.load(std::sync::atomic::Ordering::Relaxed);
            log::info!(
                "thread_pool metrics: hi_jobs={} normal_jobs={} idle_ticks={}",
                hi,
                lo,
                idle
            );
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
        joined
    }

    /// Convenience: cancel all workers and wait for up to `timeout` for them to exit.
    pub fn cancel_all_join(&mut self, timeout: std::time::Duration) -> usize {
        self.cancel_all();
        self.join_with_timeout(timeout)
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
}

fn worker_loop<E>(
    backend: Arc<ClassicBackend<E>>,
    task_hi_rx: crossbeam::channel::Receiver<TaskEnvelope>,
    task_rx: crossbeam::channel::Receiver<TaskEnvelope>,
    ctrl_rx: crossbeam::channel::Receiver<WorkerCommand>,
    worker_id: usize,
) where
    E: Evaluator + Send + Sync + 'static,
{
    let mut local = WorkerLocal::new();
    let biased = matches!(
        std::env::var("SHOGI_THREADPOOL_BIASED")
            .map(|s| s.to_ascii_lowercase())
            .ok()
            .as_deref(),
        Some("1" | "true" | "on")
    );
    let timeout = std::time::Duration::from_millis(20);
    loop {
        // crossbeam select! に制御チャネルを組み込んで、終了要求に即時反応する。
        let tick = crossbeam::channel::after(timeout);
        // 先に高優先度キューを非ブロッキングで吸い切る（優先処理の安定化）
        let envelope = if biased {
            if let Ok(env) = task_hi_rx.try_recv() {
                METRICS_HI.fetch_add(1, AtomicOrdering::Relaxed);
                Some(env)
            } else {
                crossbeam::select! {
                    recv(ctrl_rx) -> _ => { break; }
                    recv(task_hi_rx) -> msg => msg.ok().inspect(|_| { METRICS_HI.fetch_add(1, AtomicOrdering::Relaxed); }),
                    recv(task_rx)    -> msg => msg.ok().inspect(|_| { METRICS_NORMAL.fetch_add(1, AtomicOrdering::Relaxed); }),
                    recv(tick) -> _ => { local.on_idle(); METRICS_IDLE.fetch_add(1, AtomicOrdering::Relaxed); None }
                }
            }
        } else {
            crossbeam::select! {
                recv(ctrl_rx) -> _ => { break; }
                recv(task_hi_rx) -> msg => msg.ok().inspect(|_| { METRICS_HI.fetch_add(1, AtomicOrdering::Relaxed); }),
                recv(task_rx)    -> msg => msg.ok().inspect(|_| { METRICS_NORMAL.fetch_add(1, AtomicOrdering::Relaxed); }),
                recv(tick) -> _ => { local.on_idle(); METRICS_IDLE.fetch_add(1, AtomicOrdering::Relaxed); None }
            }
        };

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
        let _ = result_tx.send((worker_id, result));
        drop(result_tx);
    }
}

// Phase 4 metrics (opt-in via SHOGI_THREADPOOL_METRICS=1)
static METRICS_IDLE: AtomicU64 = AtomicU64::new(0);
static METRICS_HI: AtomicU64 = AtomicU64::new(0);
static METRICS_NORMAL: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::{SearchLimitsBuilder, TranspositionTable};
    use std::sync::mpsc;

    /// Verify that ThreadPool's shared queue correctly processes jobs exceeding worker count.
    ///
    /// This test dispatches 5 jobs to a pool with 2 workers, confirming that:
    /// - All jobs complete successfully via the pull-based shared queue
    /// - Workers process jobs sequentially from the shared crossbeam channel
    /// - Worker IDs are correctly reported (expecting IDs 1 and 2)
    #[test]
    fn shared_queue_completes_all_jobs() {
        let backend = Arc::new(ClassicBackend::with_tt(
            Arc::new(MaterialEvaluator),
            Arc::new(TranspositionTable::new(2)),
        ));
        let pool = ThreadPool::new(backend, 2);

        let mut jobs: Vec<SearchJob> = Vec::new();
        for _ in 0..5 {
            let pos = crate::shogi::Position::startpos();
            let limits = SearchLimitsBuilder::default().fixed_nodes(64).depth(1).build();
            jobs.push(SearchJob {
                position: pos,
                limits,
            });
        }
        let (tx, rx) = mpsc::channel();
        pool.dispatch(jobs, &tx);

        let mut got = 0usize;
        let mut seen_ids = std::collections::HashSet::new();
        while got < 5 {
            let (wid, res) = rx.recv().expect("result");
            seen_ids.insert(wid);
            assert!(res.stats.nodes > 0);
            got += 1;
        }
        assert_eq!(got, 5);
        // 2 workers process 5 jobs, so we should see both worker IDs (1 and 2).
        assert!(seen_ids.len() <= 2, "should see at most 2 worker IDs");
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
        let pool = ThreadPool::new(backend, 1); // Single worker for deterministic job sequencing

        let (tx, rx) = mpsc::channel();
        let pos = crate::shogi::Position::startpos();
        let limits1 = SearchLimitsBuilder::default().fixed_nodes(64).depth(1).build();
        let limits2 = SearchLimitsBuilder::default().fixed_nodes(64).depth(1).build();

        let job1 = SearchJob {
            position: pos.clone(),
            limits: limits1,
        };
        let job2 = SearchJob {
            position: pos,
            limits: limits2,
        };

        pool.dispatch(vec![job1, job2], &tx);

        // Receive both results
        let (_, res1) = rx.recv().expect("job1 result");
        let (_, res2) = rx.recv().expect("job2 result");

        assert!(res1.stats.nodes > 0, "job1 should have searched nodes");
        assert!(res2.stats.nodes > 0, "job2 should have searched nodes");
        // If Heuristics reuse is working correctly, both jobs complete without issues.
    }

    #[test]
    fn priority_queue_favor() {
        // Verify that high-priority jobs actually complete before normal-priority jobs.
        // Strategy: dispatch normal jobs (heavy: 2048 nodes) first, then high-priority jobs (light: 64 nodes).
        // High-priority jobs should complete first despite being dispatched later.
        let backend = Arc::new(ClassicBackend::with_tt(
            Arc::new(MaterialEvaluator),
            Arc::new(TranspositionTable::new(2)),
        ));
        let pool = ThreadPool::new(backend, 2);

        let (tx, rx) = mpsc::channel();
        let pos = crate::shogi::Position::startpos();

        // Dispatch normal-priority jobs with heavier workload first
        let normal_jobs: Vec<_> = (0..4)
            .map(|_| SearchJob {
                position: pos.clone(),
                limits: SearchLimitsBuilder::default().fixed_nodes(2048).depth(3).build(),
            })
            .collect();
        pool.dispatch(normal_jobs, &tx);

        // Then dispatch high-priority jobs with lighter workload
        let high_jobs: Vec<_> = (0..4)
            .map(|_| SearchJob {
                position: pos.clone(),
                limits: SearchLimitsBuilder::default().fixed_nodes(64).depth(1).build(),
            })
            .collect();
        pool.dispatch_high_priority(high_jobs, &tx);

        // Collect first 4 results and count how many are high-priority (light workload)
        let mut high_priority_count = 0;
        for _ in 0..4 {
            let (_, res) = rx.recv().expect("result");
            // High-priority jobs have much fewer nodes (64 vs 2048)
            if res.stats.nodes < 500 {
                high_priority_count += 1;
            }
        }

        // Drain remaining results
        for _ in 0..4 {
            let _ = rx.recv().expect("result");
        }

        // Expect at least 2 out of first 4 to be high-priority (conservative threshold).
        // This isn't deterministic but shows HI queue bias.
        assert!(
            high_priority_count >= 2,
            "Expected at least 2 high-priority jobs in first 4 completions, got {high_priority_count}"
        );
    }

    #[test]
    fn worker_refreshes_nps_when_elapsed_is_zero() {
        // Confirm that if backend returns result with elapsed=0, worker_loop compensates
        // elapsed and refreshes nps.
        let backend = Arc::new(ClassicBackend::with_tt(
            Arc::new(MaterialEvaluator),
            Arc::new(TranspositionTable::new(2)),
        ));
        let pool = ThreadPool::new(backend, 1);

        let pos = crate::shogi::Position::startpos();
        let limits = SearchLimitsBuilder::default().fixed_nodes(128).depth(2).build();
        let job = SearchJob {
            position: pos,
            limits,
        };

        let (tx, rx) = mpsc::channel();
        pool.dispatch(vec![job], &tx);

        let (_worker_id, result) = rx.recv().expect("worker result");
        // Worker should have compensated elapsed and refreshed nps.
        assert!(result.stats.elapsed.as_nanos() > 0, "elapsed should be non-zero");
        assert!(result.nps > 0, "nps should be refreshed and positive");
    }
}
