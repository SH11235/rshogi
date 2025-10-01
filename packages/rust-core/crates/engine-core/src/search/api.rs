use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

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

pub type InfoEventCallback = Arc<dyn Fn(InfoEvent) + Send + Sync>;

/// Handle for addressing an in-flight search session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopHandle {
    pub session_id: u64,
}

/// Backend trait for future search implementations
pub trait SearcherBackend: Send + Sync {
    fn think_blocking(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<InfoEventCallback>,
    ) -> SearchResult;
    fn start_async(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<InfoEventCallback>,
    ) -> StopHandle;
    fn stop(&self, handle: &StopHandle);
    fn update_threads(&self, n: usize);
    fn update_hash(&self, mb: usize);
}

/// Minimal stub backend that reuses the stub searcher
pub struct StubBackend {
    session_ctr: AtomicU64,
    _threads: AtomicU64,
    _hash_mb: AtomicU64,
}

impl StubBackend {
    pub fn new() -> Self {
        Self {
            session_ctr: AtomicU64::new(1),
            _threads: AtomicU64::new(1),
            _hash_mb: AtomicU64::new(32),
        }
    }
    fn next_session_id(&self) -> u64 {
        self.session_ctr.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }
}

impl Default for StubBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SearcherBackend for StubBackend {
    fn think_blocking(
        &self,
        root: &Position,
        limits: &SearchLimits,
        _info: Option<InfoEventCallback>,
    ) -> SearchResult {
        crate::search::stub::run_stub_search(root, limits)
    }

    fn start_async(
        &self,
        root: &Position,
        limits: &SearchLimits,
        _info: Option<InfoEventCallback>,
    ) -> StopHandle {
        let sid = self.next_session_id();
        let pos = root.clone();
        let lim = limits.clone();
        std::thread::spawn(move || {
            let _ = crate::search::stub::run_stub_search(&pos, &lim);
        });
        StopHandle { session_id: sid }
    }

    fn stop(&self, _handle: &StopHandle) { /* nothing to stop for stub */
    }
    fn update_threads(&self, n: usize) {
        self._threads.store(n as u64, Ordering::Release);
    }
    fn update_hash(&self, mb: usize) {
        self._hash_mb.store(mb as u64, Ordering::Release);
    }
}
