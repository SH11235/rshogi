use super::SharedSearchState;
use crate::search::types::{StopInfo, TerminationReason};
use log::debug;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, Weak,
};

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
        debug!("EngineStopBridge: session handles cleared");
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
}

/// Diagnostics snapshot produced by [`EngineStopBridge::snapshot`].
#[derive(Debug, Default, Clone, Copy)]
pub struct StopSnapshot {
    pub pending_work_items: u64,
    pub active_workers: usize,
    pub stop_flag_set: bool,
}
