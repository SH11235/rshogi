// use super::SharedSearchState; // removed during migration
// use crate::search::snapshot::RootSnapshot; // removed during migration
use crate::search::types::StopInfo;
use log::debug;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, Weak,
};

/// Reason for an out-of-band finalize request issued by time management or other guards.
#[derive(Debug, Clone, Copy)]
pub enum FinalizeReason {
    /// Hard deadline reached (elapsed >= hard)
    Hard,
    /// Approaching hard deadline window (near-hard finalize window)
    NearHard,
    /// Planned rounded stop time reached/near
    Planned,
    /// Generic time-manager stop (e.g., node limit or emergency)
    TimeManagerStop,
    /// User/GUI stop propagation (for consistency)
    UserStop,
}

/// Messages delivered to the USI layer to coordinate exactly-once bestmove emission.
#[derive(Debug, Clone)]
pub enum FinalizerMsg {
    /// New search session started (publish current session id)
    SessionStart { session_id: u64 },
    /// Request immediate finalize for the given session
    Finalize {
        session_id: u64,
        reason: FinalizeReason,
    },
}

/// Non-blocking conduit for issuing immediate stop requests from outer layers (USI, GUI) to
/// the running parallel search without needing to grab Engine mutexes.
#[derive(Clone, Default)]
pub struct EngineStopBridge {
    inner: Arc<EngineStopBridgeInner>,
}

#[derive(Default)]
struct EngineStopBridgeInner {
    // shared_state: Mutex<Option<Weak<SharedSearchState>>>,
    // pending_work_items: Mutex<Option<Weak<AtomicU64>>>,
    external_stop_flag: Mutex<Option<Weak<AtomicBool>>>,
    // Finalizer message channel to USI layer (optional)
    finalizer_tx: Mutex<Option<std::sync::mpsc::Sender<FinalizerMsg>>>,
    // Current session id (engine-core epoch). Used to tag finalize requests.
    session_id: AtomicU64,
    // Tracks whether the current session already emitted bestmove via OOB finalize.
    finalize_claimed: AtomicBool,
}

impl EngineStopBridge {
    /// Create a new bridge instance.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(EngineStopBridgeInner::default()),
        }
    }

    /// Publish the handles for the currently running search session.
    pub fn publish_session(&self, external_stop: Option<&Arc<AtomicBool>>, session_id: u64) {
        self.update_external_stop_flag(external_stop);
        // Record current session id and notify USI side
        self.inner.session_id.store(session_id, Ordering::Release);
        self.inner.finalize_claimed.store(false, Ordering::Release);
        if let Some(tx) = self.inner.finalizer_tx.lock().unwrap().as_ref() {
            let _ = tx.send(FinalizerMsg::SessionStart { session_id });
        }
        debug!("EngineStopBridge: session handles published");
    }

    /// Update only the external stop flag reference (used for non-parallel searches).
    /// Also resets finalize_claimed to allow bestmove emission for the new session.
    pub fn update_external_stop_flag(&self, external_stop: Option<&Arc<AtomicBool>>) {
        let mut guard = self.inner.external_stop_flag.lock().unwrap();
        *guard = external_stop.map(Arc::downgrade);
        // Reset finalize_claimed so single-threaded searches can emit bestmove
        self.inner.finalize_claimed.store(false, Ordering::Release);
    }

    /// Clear all handles (call after a search session finishes).
    pub fn clear(&self) {
        let mut guard = self.inner.external_stop_flag.lock().unwrap();
        *guard = None;
        self.inner.finalize_claimed.store(false, Ordering::Release);
        debug!("EngineStopBridge: session handles cleared");
    }

    /// Forcefully clear references when the previous session failed to shut down cleanly.
    /// This should be used sparingly as it aborts any outstanding workers by dropping handles.
    pub fn force_clear(&self) {
        {
            let mut guard = self.inner.external_stop_flag.lock().unwrap();
            if let Some(flag) = guard.as_ref().and_then(|weak| weak.upgrade()) {
                flag.store(true, Ordering::Release);
            }
            *guard = None;
        }
        self.inner.finalize_claimed.store(false, Ordering::Release);
        // Advance session epoch so that stale finalize messages are ignored.
        self.inner.session_id.fetch_add(1, Ordering::AcqRel);
        debug!("EngineStopBridge: force_clear executed");
    }

    /// Issue a best-effort immediate stop request to the currently running search.
    pub fn request_stop_immediate(&self) {
        let external_flag_upgraded = {
            let guard = self.inner.external_stop_flag.lock().unwrap();
            guard.as_ref().and_then(|weak| weak.upgrade())
        };

        if let Some(ref external_flag) = external_flag_upgraded {
            external_flag.store(true, Ordering::Release);
        }

        // No internal shared state during migration

        // pending work queue not tracked during migration

        debug!("EngineStopBridge: stop broadcasted");
    }

    /// Capture a lightweight snapshot of the active search state for diagnostics/logging.
    pub fn snapshot(&self) -> StopSnapshot {
        StopSnapshot::default()
    }

    /// Try reading a consistent root snapshot for the active session.
    pub fn try_read_snapshot(&self) -> Option<crate::search::snapshot::RootSnapshot> {
        None
    }

    /// Try reading StopInfo snapshot of the current session, if any.
    pub fn try_read_stop_info(&self) -> Option<StopInfo> {
        None
    }

    /// Register USI-side finalizer channel to receive finalize/session messages.
    pub fn register_finalizer(&self, tx: std::sync::mpsc::Sender<FinalizerMsg>) {
        let mut guard = self.inner.finalizer_tx.lock().unwrap();
        *guard = Some(tx);
        debug!("EngineStopBridge: finalizer channel registered");
    }

    /// Issue an out-of-band finalize request to USI layer.
    pub fn request_finalize(&self, reason: FinalizeReason) {
        let session_id = self.inner.session_id.load(Ordering::Acquire);
        if let Some(tx) = self.inner.finalizer_tx.lock().unwrap().as_ref() {
            let _ = tx.send(FinalizerMsg::Finalize { session_id, reason });
        }
    }

    /// Attempt to claim the right to emit bestmove for the active session.
    /// Returns true only once per session (first caller wins).
    pub fn try_claim_finalize(&self) -> bool {
        self.inner
            .finalize_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// Diagnostics snapshot produced by [`EngineStopBridge::snapshot`].
#[derive(Debug, Default, Clone, Copy)]
pub struct StopSnapshot {
    pub pending_work_items: u64,
    pub active_workers: usize,
    pub stop_flag_set: bool,
}
