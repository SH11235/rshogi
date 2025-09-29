use super::SharedSearchState;
use crate::search::snapshot::RootSnapshot;
use crate::search::types::{StopInfo, TerminationReason};
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
    shared_state: Mutex<Option<Weak<SharedSearchState>>>,
    pending_work_items: Mutex<Option<Weak<AtomicU64>>>,
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
    pub fn publish_session(
        &self,
        shared_state: &Arc<SharedSearchState>,
        pending_work: &Arc<AtomicU64>,
        external_stop: Option<&Arc<AtomicBool>>,
        session_id: u64,
    ) {
        {
            let mut guard = self.inner.shared_state.lock().unwrap();
            *guard = Some(Arc::downgrade(shared_state));
        }
        {
            let mut guard = self.inner.pending_work_items.lock().unwrap();
            *guard = Some(Arc::downgrade(pending_work));
        }
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
    pub fn update_external_stop_flag(&self, external_stop: Option<&Arc<AtomicBool>>) {
        let mut guard = self.inner.external_stop_flag.lock().unwrap();
        *guard = external_stop.map(Arc::downgrade);
    }

    /// Clear all handles (call after a search session finishes).
    pub fn clear(&self) {
        {
            let mut guard = self.inner.shared_state.lock().unwrap();
            *guard = None;
        }
        {
            let mut guard = self.inner.pending_work_items.lock().unwrap();
            *guard = None;
        }
        {
            let mut guard = self.inner.external_stop_flag.lock().unwrap();
            *guard = None;
        }
        self.inner.finalize_claimed.store(false, Ordering::Release);
        debug!("EngineStopBridge: session handles cleared");
    }

    /// Forcefully clear references when the previous session failed to shut down cleanly.
    /// This should be used sparingly as it aborts any outstanding workers by dropping handles.
    pub fn force_clear(&self) {
        {
            let mut guard = self.inner.shared_state.lock().unwrap();
            if let Some(shared) = guard.as_ref().and_then(|weak| weak.upgrade()) {
                shared.set_stop();
                shared.close_work_queues();
            }
            *guard = None;
        }
        {
            let mut guard = self.inner.pending_work_items.lock().unwrap();
            *guard = None;
        }
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
        let shared_upgraded = {
            let guard = self.inner.shared_state.lock().unwrap();
            guard.as_ref().and_then(|weak| weak.upgrade())
        };
        let external_flag_upgraded = {
            let guard = self.inner.external_stop_flag.lock().unwrap();
            guard.as_ref().and_then(|weak| weak.upgrade())
        };

        if let Some(ref external_flag) = external_flag_upgraded {
            external_flag.store(true, Ordering::Release);
        }

        if let Some(shared) = shared_upgraded {
            // Do not overwrite existing stop_info (e.g., TimeLimit). If already set, only set the flag.
            if shared.stop_info.get().is_some() {
                shared.set_stop();
            } else {
                let nodes = shared.get_nodes();
                let depth = shared.get_best_depth();
                let stop_info = StopInfo {
                    reason: TerminationReason::UserStop,
                    elapsed_ms: 0,
                    nodes,
                    depth_reached: depth,
                    hard_timeout: false,
                    soft_limit_ms: 0,
                    hard_limit_ms: 0,
                };
                shared.set_stop_with_reason(stop_info);
            }
            shared.close_work_queues();
        }

        if let Some(pending) = {
            let guard = self.inner.pending_work_items.lock().unwrap();
            guard.as_ref().and_then(|weak| weak.upgrade())
        } {
            pending.store(0, Ordering::Release);
        }

        debug!("EngineStopBridge: stop broadcasted");
    }

    /// Capture a lightweight snapshot of the active search state for diagnostics/logging.
    pub fn snapshot(&self) -> StopSnapshot {
        let shared_state = {
            let guard = self.inner.shared_state.lock().unwrap();
            guard.as_ref().and_then(|weak| weak.upgrade())
        };

        let pending = {
            let guard = self.inner.pending_work_items.lock().unwrap();
            guard
                .as_ref()
                .and_then(|weak| weak.upgrade())
                .map(|p| p.load(Ordering::Acquire))
                .unwrap_or(0)
        };

        if let Some(shared) = shared_state {
            StopSnapshot {
                pending_work_items: pending,
                active_workers: shared.active_thread_count(),
                stop_flag_set: shared.should_stop(),
            }
        } else {
            StopSnapshot {
                pending_work_items: pending,
                ..StopSnapshot::default()
            }
        }
    }

    /// Try reading a consistent root snapshot for the active session.
    pub fn try_read_snapshot(&self) -> Option<RootSnapshot> {
        let shared_state = {
            let guard = self.inner.shared_state.lock().unwrap();
            guard.as_ref().and_then(|weak| weak.upgrade())
        }?;
        shared_state.snapshot.try_read()
    }

    /// Try reading StopInfo snapshot of the current session, if any.
    pub fn try_read_stop_info(&self) -> Option<StopInfo> {
        let shared_state = {
            let guard = self.inner.shared_state.lock().unwrap();
            guard.as_ref().and_then(|weak| weak.upgrade())
        }?;
        shared_state.stop_info.get().cloned()
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
