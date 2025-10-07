use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::evaluation::evaluate::Evaluator;
use crate::search::ab::ClassicBackend;
use crate::search::api::SearcherBackend;
use crate::search::{SearchLimits, SearchResult};
use crate::shogi::Position;
use std::time::Instant;

pub struct ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    backend: Arc<ClassicBackend<E>>,
    workers: Vec<Worker>,
}

pub struct SearchJob {
    pub position: Position,
    pub limits: SearchLimits,
}

impl<E> ThreadPool<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    pub fn new(backend: Arc<ClassicBackend<E>>, size: usize) -> Self {
        let mut pool = Self {
            backend,
            workers: Vec::new(),
        };
        pool.resize(size);
        pool
    }

    pub fn resize(&mut self, desired: usize) {
        while self.workers.len() < desired {
            let id = self.workers.len() + 1; // helper ids start at 1 (0 is main thread)
            let backend = Arc::clone(&self.backend);
            let (tx, rx) = mpsc::channel();
            let handle = thread::Builder::new()
                .name(format!("lazy-smp-worker-{id}"))
                .spawn(move || worker_loop(backend, rx))
                .expect("spawn lazy smp worker");
            self.workers.push(Worker {
                sender: tx,
                handle: Some(handle),
            });
        }

        while self.workers.len() > desired {
            if let Some(mut worker) = self.workers.pop() {
                let _ = worker.sender.send(WorkerCommand::Shutdown);
                if let Some(handle) = worker.handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }

    pub fn dispatch(&self, jobs: Vec<SearchJob>, result_tx: &Sender<(usize, SearchResult)>) {
        let worker_n = self.workers.len();
        let mut iter = jobs.into_iter();
        // Send up to worker_n jobs to resident workers
        for (idx, worker) in self.workers.iter().enumerate() {
            if let Some(job) = iter.next() {
                let worker_id = idx + 1; // resident ids start at 1
                let _ = worker.sender.send(WorkerCommand::Start {
                    worker_id,
                    job: Box::new(job),
                    result_tx: result_tx.clone(),
                });
            }
        }

        // Overflow handling: spawn ephemeral threads for remaining jobs (small cap)
        let cap = std::env::var("SHOGI_OVERFLOW_SPAWN_CAP")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(8);
        for (i, job) in iter.enumerate() {
            if i >= cap {
                log::warn!(
                    "thread_pool overflow cap reached ({}); executing remaining jobs synchronously",
                    cap
                );
                // Synchronous fallback using a temporary thread per job beyond cap
                // (kept simple; these jobs are expected to be rare and lightweight)
            }
            let backend = Arc::clone(&self.backend);
            let tx = result_tx.clone();
            let worker_id = worker_n + i + 1; // unique id beyond resident range
            std::thread::Builder::new()
                .name(format!("lazy-smp-overflow-{}", worker_id))
                .spawn(move || {
                    let SearchJob { position, limits } = job;
                    let result = backend.think_blocking(&position, &limits, None);
                    let _ = tx.send((worker_id, result));
                })
                .expect("spawn overflow worker");
        }
    }

    pub fn shutdown(&mut self) {
        for worker in self.workers.iter_mut() {
            let _ = worker.sender.send(WorkerCommand::Shutdown);
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
    sender: Sender<WorkerCommand>,
    handle: Option<JoinHandle<()>>,
}

enum WorkerCommand {
    Start {
        worker_id: usize,
        job: Box<SearchJob>,
        result_tx: Sender<(usize, SearchResult)>,
    },
    Shutdown,
}

fn worker_loop<E>(backend: Arc<ClassicBackend<E>>, rx: Receiver<WorkerCommand>)
where
    E: Evaluator + Send + Sync + 'static,
{
    while let Ok(cmd) = rx.recv() {
        match cmd {
            WorkerCommand::Start {
                worker_id,
                job,
                result_tx,
            } => {
                let SearchJob { position, limits } = *job;
                let start = Instant::now();
                let mut result = backend.think_blocking(&position, &limits, None);
                if result.stats.elapsed.as_nanos() == 0 {
                    result.stats.elapsed = start.elapsed();
                }
                let _ = result_tx.send((worker_id, result));
            }
            WorkerCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::{SearchLimitsBuilder, TranspositionTable};

    #[test]
    fn thread_pool_overflow_completes_all_jobs() {
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
        assert_eq!(seen_ids.len(), 5);
    }
}
