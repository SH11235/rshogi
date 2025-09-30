//! Search session management for asynchronous search execution.
//!
//! This module provides `SearchSession`, which represents an ongoing search
//! that runs asynchronously without holding the Engine lock.

use std::sync::mpsc;
use std::time::Duration;

use crate::search::SearchResult;

/// A handle to an ongoing search session.
///
/// The search runs asynchronously in a background thread, and the caller
/// can poll for results without blocking. This allows the Engine lock to be
/// released immediately after starting the search, enabling concurrent operations.
pub struct SearchSession {
    /// Unique identifier for this search session
    session_id: u64,

    /// Receiver for the search result
    result_rx: mpsc::Receiver<SearchResult>,
}

impl SearchSession {
    /// Create a new search session.
    pub(crate) fn new(session_id: u64, result_rx: mpsc::Receiver<SearchResult>) -> Self {
        Self {
            session_id,
            result_rx,
        }
    }

    /// Get the session ID.
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Try to receive the search result without blocking.
    ///
    /// Returns `Some(result)` if the search has completed, `None` if it's still running.
    pub fn try_recv_result(&self) -> Option<SearchResult> {
        self.result_rx.try_recv().ok()
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
}
