use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use crate::search::{SearchLimits, SearchResult};
use crate::shogi::Move;
use crate::Position;

/// Outcome of an aspiration window failure
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspirationOutcome {
    FailHigh,
    FailLow,
}

/// Event-driven search progress notifications for USI formatting
#[derive(Debug, Clone)]
pub enum InfoEvent {
    Depth {
        depth: u32,
        seldepth: u32,
    },
    CurrMove {
        mv: Move,
        number: u32,
    },
    PV {
        line: crate::search::types::RootLine,
    },
    Hashfull(u32),
    Aspiration {
        outcome: AspirationOutcome,
        old_alpha: i32,
        old_beta: i32,
        new_alpha: i32,
        new_beta: i32,
    },
    String(String),
}

pub type InfoEventCallback = Arc<dyn Fn(InfoEvent) + Send + Sync + 'static>;

#[derive(Clone)]
pub struct StopHandle {
    flag: Arc<AtomicBool>,
}

impl StopHandle {
    pub fn new(flag: Arc<AtomicBool>) -> Self {
        Self { flag }
    }

    pub fn request_stop(&self) {
        self.flag.store(true, Ordering::Release);
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

pub struct BackendSearchTask {
    stop_handle: StopHandle,
    result_rx: mpsc::Receiver<SearchResult>,
    join_handle: Option<JoinHandle<()>>,
}

impl BackendSearchTask {
    pub fn new(
        stop_flag: Arc<AtomicBool>,
        result_rx: mpsc::Receiver<SearchResult>,
        join_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            stop_handle: StopHandle::new(stop_flag),
            result_rx,
            join_handle: Some(join_handle),
        }
    }

    pub fn into_parts(self) -> (StopHandle, mpsc::Receiver<SearchResult>, Option<JoinHandle<()>>) {
        (self.stop_handle, self.result_rx, self.join_handle)
    }

    pub fn request_stop(&self) {
        self.stop_handle.request_stop();
    }
}

/// Backend trait for future search implementations
pub trait SearcherBackend: Send + Sync {
    fn start_async(
        self: Arc<Self>,
        root: Position,
        limits: SearchLimits,
        info: Option<InfoEventCallback>,
    ) -> BackendSearchTask;

    fn request_stop(&self, task: &BackendSearchTask) {
        task.request_stop();
    }

    fn think_blocking(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<InfoEventCallback>,
    ) -> SearchResult;
    fn update_threads(&self, n: usize);
    fn update_hash(&self, mb: usize);
}

/// Minimal stub backend that reuses the stub searcher
pub struct StubBackend {
    _threads: AtomicU64,
    _hash_mb: AtomicU64,
}

impl StubBackend {
    pub fn new() -> Self {
        Self {
            _threads: AtomicU64::new(1),
            _hash_mb: AtomicU64::new(32),
        }
    }
}

impl Default for StubBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SearcherBackend for StubBackend {
    fn start_async(
        self: Arc<Self>,
        root: Position,
        mut limits: SearchLimits,
        _info: Option<InfoEventCallback>,
    ) -> BackendSearchTask {
        let stop_flag =
            limits.stop_flag.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();
        let _ = self;
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("stub-backend-search".into())
            .spawn(move || {
                let result = crate::search::stub::run_stub_search(&root, &limits);
                let _ = tx.send(result);
            })
            .expect("spawn stub backend search thread");
        BackendSearchTask::new(stop_flag, rx, handle)
    }

    fn think_blocking(
        &self,
        root: &Position,
        limits: &SearchLimits,
        _info: Option<InfoEventCallback>,
    ) -> SearchResult {
        crate::search::stub::run_stub_search(root, limits)
    }

    fn update_threads(&self, n: usize) {
        self._threads.store(n as u64, Ordering::Release);
    }
    fn update_hash(&self, mb: usize) {
        self._hash_mb.store(mb as u64, Ordering::Release);
    }
}
