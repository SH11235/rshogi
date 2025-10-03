//! Search session management for asynchronous search execution.
//!
//! This module provides `SearchSession`, which represents an ongoing search
//! that runs asynchronously without holding the Engine lock.

use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::search::api::StopHandle;
use crate::search::parallel::EngineStopBridge;
use crate::search::SearchResult;
use crate::time_management::TimeManager;

/// Result type for non-blocking poll operations on SearchSession.
///
/// This distinguishes between "still running", "completed", and "disconnected"
/// states, allowing the caller to handle thread failures gracefully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryResult<T> {
    /// Search is still running, no result available yet
    Pending,
    /// Search completed successfully with a result
    Ok(T),
    /// Search thread disconnected without sending a result (panic or early exit)
    Disconnected,
}

/// A handle to an ongoing search session.
///
/// The search runs asynchronously in a background thread, and the caller
/// can poll for results without blocking. This allows the Engine lock to be
/// released immediately after starting the search, enabling concurrent operations.
///
/// The session holds an optional JoinHandle to allow explicit joining during
/// isready/quit commands, while normal game paths remain non-blocking.
#[must_use = "SearchSession should be stored and polled for results"]
pub struct SearchSession {
    /// Unique identifier for this search session
    session_id: u64,

    /// Receiver for the search result
    result_rx: mpsc::Receiver<SearchResult>,

    /// Optional handle to the background thread for explicit joining
    handle: Option<thread::JoinHandle<()>>,

    stop_handle: StopHandle,
    time_manager: Option<Arc<TimeManager>>,
}

impl SearchSession {
    /// Create a new search session.
    pub(crate) fn new(
        session_id: u64,
        result_rx: mpsc::Receiver<SearchResult>,
        handle: Option<thread::JoinHandle<()>>,
        stop_handle: StopHandle,
        time_manager: Option<Arc<TimeManager>>,
    ) -> Self {
        Self {
            session_id,
            result_rx,
            handle,
            stop_handle,
            time_manager,
        }
    }

    /// Get the session ID.
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Try to receive the search result without blocking.
    ///
    /// Returns `Some(result)` if the search has completed, `None` if it's still running.
    ///
    /// Note: This method cannot distinguish between "still running" and "disconnected".
    /// Use `try_poll()` if you need to detect thread failures.
    pub fn try_recv_result(&self) -> Option<SearchResult> {
        self.result_rx.try_recv().ok()
    }

    /// Try to poll the search result without blocking, distinguishing disconnection.
    ///
    /// This is the recommended method for production use as it can detect when the
    /// search thread has panicked or exited without sending a result.
    ///
    /// Returns:
    /// - `TryResult::Ok(result)` if the search has completed successfully
    /// - `TryResult::Pending` if the search is still running
    /// - `TryResult::Disconnected` if the search thread died without sending a result
    pub fn try_poll(&self) -> TryResult<SearchResult> {
        use std::sync::mpsc::TryRecvError;
        match self.result_rx.try_recv() {
            Ok(result) => TryResult::Ok(result),
            Err(TryRecvError::Empty) => TryResult::Pending,
            Err(TryRecvError::Disconnected) => TryResult::Disconnected,
        }
    }

    /// Check if the search is still running without consuming the result.
    ///
    /// Returns `true` if the search is still running, `false` if completed or disconnected.
    pub fn is_pending(&self) -> bool {
        matches!(self.try_poll(), TryResult::Pending)
    }

    /// Receive the search result with a timeout.
    ///
    /// Returns `Some(result)` if the search completed within the timeout,
    /// `None` if the timeout expired.
    pub fn recv_result_timeout(&self, timeout: Duration) -> Option<SearchResult> {
        self.result_rx.recv_timeout(timeout).ok()
    }

    /// Block until the search result is available.
    ///
    /// This is primarily for testing and synchronous use cases.
    pub fn recv_result(&self) -> Option<SearchResult> {
        self.result_rx.recv().ok()
    }

    /// Request stop and wait for result with timeout.
    ///
    /// This is intended for isready/quit commands where we need to ensure
    /// the search completes cleanly. First requests immediate stop via the
    /// stop bridge, then waits for the result with the given timeout.
    ///
    /// Returns `Some(result)` if the search completed within the timeout,
    /// `None` if the timeout expired.
    pub fn request_stop_and_wait(
        &self,
        bridge: &EngineStopBridge,
        timeout: Duration,
    ) -> Option<SearchResult> {
        self.stop_handle.request_stop();
        bridge.request_stop();
        self.recv_result_timeout(timeout)
    }

    /// Request backend stop without waiting for result.
    pub fn request_stop(&self) {
        self.stop_handle.request_stop();
    }

    /// Clone the time manager associated with this search (if any).
    pub fn time_manager(&self) -> Option<Arc<TimeManager>> {
        self.time_manager.as_ref().map(Arc::clone)
    }

    /// Join the background thread, blocking until it completes.
    ///
    /// This consumes the session and should only be used during isready/quit
    /// where we need to ensure all searches have completed before proceeding.
    /// Normal game paths should never call this method.
    ///
    /// If the thread has already been joined or panicked, this returns immediately.
    pub fn join_blocking(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
