use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        line: Arc<crate::search::types::RootLine>,
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
        active_counter: Arc<AtomicUsize>,
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
