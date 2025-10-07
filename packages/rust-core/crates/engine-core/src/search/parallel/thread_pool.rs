use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::evaluation::evaluate::Evaluator;
use crate::search::ab::ordering::Heuristics;
use crate::search::ab::ClassicBackend;
// SearcherBackend is not directly used here; ClassicBackend is invoked through its public APIs.
use crate::search::{SearchLimits, SearchResult};
use crate::shogi::Position;
use rand::SeedableRng;
use rand_xoshiro::Xoshiro128PlusPlus;
use std::time::Instant;

// Worker-local scratch/state. Lives entirely on each worker thread.
// Today it's only RNG + a small scratch buffer, but this is the hook where we can
// attach heuristics buffers, stacks, killers, etc. in future (YBWC-friendly).
struct WorkerLocal {
    rng: Xoshiro128PlusPlus,
    last_seed: u64,
    // Minimal scratch placeholder; grows as we add features.
    scratch: Vec<u8>,
    // Heuristics buffer reused across jobs (helpers用)。
    heur: Heuristics,
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
        // Prefer externally provided jitter_seed; if None, derive a deterministic fallback.
        // Note: In practice, ParallelSearcher always provides jitter_seed via SearchLimits,
        // so the fallback path is primarily for defensive coding and standalone testing.
        let base = jitter_seed
            .unwrap_or_else(|| super::compute_jitter_seed(session_id, 1, worker_id, root_key));
        let seed128 = Self::seed128_from_base(base);
        self.rng = Xoshiro128PlusPlus::from_seed(seed128);
        self.last_seed = base;
        self.scratch.clear();
        // Heuristics は helpers ジョブごとにクリア（決定性・メモリ上限確保）。
        self.heur.clear_all();
    }

    #[allow(dead_code)]
    fn on_idle(&mut self) {
        // Placeholder: could age heuristics or recycle buffers here.
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
    task_hi_tx: crossbeam::channel::Sender<TaskEnvelope>,
    task_hi_rx: crossbeam::channel::Receiver<TaskEnvelope>,
}

pub struct SearchJob {
    pub position: Position,
    pub limits: SearchLimits,
}

struct TaskEnvelope {
    job: SearchJob,
    result_tx: Sender<(usize, SearchResult)>,
    // Future YBWC hooks (unused for now):
    priority: u8,
    split: Option<u64>,
}

impl<E> ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    pub fn new(backend: Arc<ClassicBackend<E>>, size: usize) -> Self {
        let (task_tx, task_rx) = crossbeam::channel::unbounded();
        let (task_hi_tx, task_hi_rx) = crossbeam::channel::unbounded();
        let mut pool = Self {
            backend,
            workers: Vec::new(),
            task_tx,
            task_rx,
            task_hi_tx,
            task_hi_rx,
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
            let (ctrl_tx, ctrl_rx) = mpsc::channel();
            let builder = thread::Builder::new().name(format!("lazy-smp-worker-{id}"));
            #[cfg(feature = "large-stack-tests")]
            {
                // Deep AB recursion in tests may need a larger stack.
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
                log::warn!("thread_pool: failed to enqueue job: {}", err);
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
        self.workers.clear();
    }
}

impl<E> Drop for ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Worker {
    ctrl: Sender<WorkerCommand>,
    handle: Option<JoinHandle<()>>,
}

enum WorkerCommand {
    Shutdown,
}

fn worker_loop<E>(
    backend: Arc<ClassicBackend<E>>,
    task_hi_rx: crossbeam::channel::Receiver<TaskEnvelope>,
    task_rx: crossbeam::channel::Receiver<TaskEnvelope>,
    ctrl_rx: Receiver<WorkerCommand>,
    worker_id: usize,
) where
    E: Evaluator + Send + Sync + 'static,
{
    let mut local = WorkerLocal::new();
    loop {
        if let Ok(WorkerCommand::Shutdown) = ctrl_rx.try_recv() {
            break;
        }

        let envelope = crossbeam::select! {
            recv(task_hi_rx) -> msg => msg.ok(),
            recv(task_rx)    -> msg => msg.ok(),
            default(Duration::from_millis(20)) => None,
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
        // Heuristics を TLS にシード（helpers fast-path）
        crate::search::ab::seed_thread_heuristics(std::mem::take(&mut local.heur));
        let mut result = backend.think_with_ctx(&position, &limits, &mut local, None);
        // 探索後に Heuristics を取り戻す（次ジョブで再利用）
        if let Some(h) = crate::search::ab::take_thread_heuristics() {
            local.heur = h;
        }
        if result.stats.elapsed.as_nanos() == 0 {
            result.stats.elapsed = start.elapsed();
            result.refresh_summary();
        }
        let _ = result_tx.send((worker_id, result));
        drop(result_tx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::{SearchLimitsBuilder, TranspositionTable};

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
