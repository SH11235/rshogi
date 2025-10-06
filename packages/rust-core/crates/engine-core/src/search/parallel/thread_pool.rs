use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::evaluation::evaluate::Evaluator;
use crate::search::ab::ClassicBackend;
use crate::search::api::SearcherBackend;
use crate::search::{SearchLimits, SearchResult};
use crate::shogi::Position;

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
        debug_assert!(jobs.len() <= self.workers.len(), "job count exceeds available workers");
        for (idx, (job, worker)) in jobs.into_iter().zip(self.workers.iter()).enumerate() {
            let worker_id = idx + 1;
            let _ = worker.sender.send(WorkerCommand::Start {
                worker_id,
                job: Box::new(job),
                result_tx: result_tx.clone(),
            });
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
                let result = backend.think_blocking(&position, &limits, None);
                let _ = result_tx.send((worker_id, result));
            }
            WorkerCommand::Shutdown => break,
        }
    }
}
