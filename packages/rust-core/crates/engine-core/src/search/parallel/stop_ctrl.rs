//! Stop controller (new API) for coordinating search finalization and stop requests.
//!
//! Transitional wrapper around the existing `EngineStopBridge` that provides a
//! cleaner, Engine-facing surface. This allows us to migrate call sites without
//! large-scale refactors and then replace the internals later.

use crate::search::snapshot::{RootSnapshot, RootSnapshotPublisher};
use crate::search::types::StopInfo;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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

/// Diagnostics snapshot produced by StopController::snapshot.
#[derive(Debug, Default, Clone, Copy)]
pub struct StopSnapshot {
    pub pending_work_items: u64,
    pub active_workers: usize,
    pub stop_flag_set: bool,
}

/// New stop controller facade. Delegates to the legacy bridge for now.
#[derive(Clone, Default)]
pub struct StopController {
    inner: Inner,
    publisher: Arc<RootSnapshotPublisher>,
}

#[derive(Default, Clone)]
struct Inner {
    // During migration we keep only an external stop flag and minimal state.
    external_stop_flag: Arc<std::sync::Mutex<Option<std::sync::Weak<AtomicBool>>>>,
    session_id: Arc<std::sync::atomic::AtomicU64>,
    finalize_claimed: Arc<std::sync::atomic::AtomicBool>,
    finalizer_tx: Arc<std::sync::Mutex<Option<std::sync::mpsc::Sender<FinalizerMsg>>>>,
    stop_info: Arc<std::sync::Mutex<Option<StopInfo>>>,
}

impl StopController {
    /// Create a new controller instance.
    pub fn new() -> Self {
        Self {
            inner: Inner::default(),
            publisher: Arc::new(RootSnapshotPublisher::new()),
        }
    }

    /// Get the underlying bridge (temporary accessor for mixed code paths).
    pub fn bridge(&self) -> StopController {
        self.clone()
    }

    /// Publish the current session (external stop flag + session id).
    pub fn publish_session(&self, external_stop: Option<&Arc<AtomicBool>>, session_id: u64) {
        self.update_external_stop_flag(external_stop);
        self.inner.session_id.store(session_id, std::sync::atomic::Ordering::Release);
        self.inner.finalize_claimed.store(false, std::sync::atomic::Ordering::Release);
        if let Some(tx) = self.inner.finalizer_tx.lock().unwrap().as_ref() {
            let _ = tx.send(FinalizerMsg::SessionStart { session_id });
        }
    }

    /// Initialize StopInfo snapshot for the upcoming session.
    pub fn prime_stop_info(&self, info: StopInfo) {
        let mut guard = self.inner.stop_info.lock().unwrap();
        *guard = Some(info);
    }

    /// Update only the external stop flag reference.
    pub fn update_external_stop_flag(&self, external_stop: Option<&Arc<AtomicBool>>) {
        let mut guard = self.inner.external_stop_flag.lock().unwrap();
        *guard = external_stop.map(Arc::downgrade);
        self.inner.finalize_claimed.store(false, std::sync::atomic::Ordering::Release);
    }

    fn set_external_stop_flag(&self) {
        let upgraded = {
            let guard = self.inner.external_stop_flag.lock().unwrap();
            guard.as_ref().and_then(|w| w.upgrade())
        };
        if let Some(flag) = upgraded {
            flag.store(true, Ordering::Release);
        }
    }

    /// Clear all handles after the session finishes.
    pub fn clear(&self) {
        let mut guard = self.inner.external_stop_flag.lock().unwrap();
        *guard = None;
        self.inner.finalize_claimed.store(false, std::sync::atomic::Ordering::Release);
    }

    /// Force clear references (advances session epoch).
    pub fn force_clear(&self) {
        {
            let mut guard = self.inner.external_stop_flag.lock().unwrap();
            if let Some(flag) = guard.as_ref().and_then(|w| w.upgrade()) {
                flag.store(true, std::sync::atomic::Ordering::Release);
            }
            *guard = None;
        }
        self.inner.finalize_claimed.store(false, std::sync::atomic::Ordering::Release);
        self.inner.session_id.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Request immediate stop (best-effort broadcast).
    pub fn request_stop(&self) {
        let upgraded = {
            let guard = self.inner.external_stop_flag.lock().unwrap();
            guard.as_ref().and_then(|w| w.upgrade())
        };
        if let Some(flag) = upgraded {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }
        let mut guard = self.inner.stop_info.lock().unwrap();
        let mut si = guard.take().unwrap_or_default();
        si.reason = crate::search::types::TerminationReason::UserStop;
        si.hard_timeout = false;
        *guard = Some(si);
    }

    /// Try to claim finalize token (exactly-once).
    pub fn try_claim_finalize(&self) -> bool {
        self.inner
            .finalize_claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    /// Request out-of-band finalize with a reason.
    pub fn request_finalize(&self, reason: FinalizeReason) {
        let session_id = self.inner.session_id.load(std::sync::atomic::Ordering::Acquire);
        if let Some(tx) = self.inner.finalizer_tx.lock().unwrap().as_ref() {
            let _ = tx.send(FinalizerMsg::Finalize { session_id, reason });
        }

        self.set_external_stop_flag();

        let mut guard = self.inner.stop_info.lock().unwrap();
        let mut si = guard.take().unwrap_or_default();
        use crate::search::types::TerminationReason;
        match reason {
            FinalizeReason::Hard => {
                si.reason = TerminationReason::TimeLimit;
                si.hard_timeout = true;
            }
            FinalizeReason::NearHard
            | FinalizeReason::Planned
            | FinalizeReason::TimeManagerStop => {
                si.reason = TerminationReason::TimeLimit;
                si.hard_timeout = false;
            }
            FinalizeReason::UserStop => {
                si.reason = TerminationReason::UserStop;
                si.hard_timeout = false;
            }
        }
        *guard = Some(si);
    }

    /// Register finalizer channel for USI layer coordination.
    pub fn register_finalizer(&self, tx: std::sync::mpsc::Sender<FinalizerMsg>) {
        let mut guard = self.inner.finalizer_tx.lock().unwrap();
        *guard = Some(tx);
    }

    /// Snapshot (diagnostics only).
    pub fn snapshot(&self) -> StopSnapshot {
        let stop_flag_set = {
            let guard = self.inner.external_stop_flag.lock().unwrap();
            guard
                .as_ref()
                .and_then(|w| w.upgrade())
                .map(|f| f.load(std::sync::atomic::Ordering::Acquire))
                .unwrap_or(false)
        };
        StopSnapshot {
            pending_work_items: 0,
            active_workers: 0,
            stop_flag_set,
        }
    }

    /// Try reading StopInfo snapshot of the current session (not yet wired).
    pub fn try_read_stop_info(&self) -> Option<crate::search::types::StopInfo> {
        self.inner.stop_info.lock().unwrap().clone()
    }

    /// Try reading a consistent root snapshot for the active session (not yet wired).
    pub fn try_read_snapshot(&self) -> Option<crate::search::snapshot::RootSnapshot> {
        self.publisher.try_read()
    }

    /// Publish a root snapshot from a PV line (called by Engine controller on InfoEvent::PV).
    pub fn publish_root_line(
        &self,
        session_id: u64,
        root_key: u64,
        line: &crate::search::types::RootLine,
    ) {
        // Only publish for the best line (multipv_index==1) to keep snapshot simple
        if line.multipv_index != 1 {
            return;
        }
        let mut snap = RootSnapshot {
            search_id: session_id,
            root_key,
            best: Some(line.root_move),
            ..Default::default()
        };
        // Copy PV (SmallVec expected)
        let mut pv_copy: SmallVec<[crate::shogi::Move; 64]> = SmallVec::new();
        pv_copy.extend_from_slice(&line.pv);
        snap.pv = pv_copy;
        snap.depth = (line.depth as u8).min(127);
        snap.score_cp = line.score_cp;
        let total_nodes = line.nodes.unwrap_or(0);
        let total_time_ms = line.time_ms.unwrap_or(0);
        snap.nodes = total_nodes;
        snap.elapsed_ms = total_time_ms.min(u32::MAX as u64) as u32;
        self.publisher.publish(&snap);
        // Update StopInfo snapshot (best-effort)
        let mut guard = self.inner.stop_info.lock().unwrap();
        let mut si = guard.take().unwrap_or_default();
        si.elapsed_ms = total_time_ms;
        si.nodes = total_nodes;
        si.depth_reached = (line.depth as u8).min(127);
        // note: request_finalize/request_stop が reason/hard_timeout を確定させる。
        // publish_root_line は進捗値のみ更新し、理由フラグは上書きしない。
        *guard = Some(si);
    }
}
