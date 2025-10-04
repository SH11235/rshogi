//! Stop controller for coordinating search finalization and stop requests.
//!
//! Provides a unified interface for stopping searches, publishing snapshots,
//! and issuing finalize requests with priority handling.

use crate::search::snapshot::{RootSnapshot, RootSnapshotPublisher};
use crate::search::types::StopInfo;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

/// Reason for an out-of-band finalize request issued by time management or other guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Stop controller facade coordinating finalize/stop operations.
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
    finalize_priority: Arc<AtomicU8>,
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

    /// Publish the current session (external stop flag + session id).
    pub fn publish_session(&self, external_stop: Option<&Arc<AtomicBool>>, session_id: u64) {
        self.update_external_stop_flag(external_stop);
        self.inner.stop_info.lock().unwrap().take();
        self.inner.session_id.store(session_id, std::sync::atomic::Ordering::Release);
        self.inner.finalize_claimed.store(false, std::sync::atomic::Ordering::Release);
        self.inner.finalize_priority.store(0, Ordering::Release);
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
        self.inner.finalize_priority.store(0, Ordering::Release);
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

    #[inline]
    fn finalize_reason_priority(reason: FinalizeReason) -> u8 {
        match reason {
            FinalizeReason::Hard => 5,
            FinalizeReason::NearHard => 4,
            FinalizeReason::Planned => 3,
            FinalizeReason::TimeManagerStop => 2,
            FinalizeReason::UserStop => 1,
        }
    }

    fn should_accept_finalize(&self, reason: FinalizeReason) -> bool {
        let priority = Self::finalize_reason_priority(reason);
        loop {
            let prev = self.inner.finalize_priority.load(Ordering::Acquire);
            if prev >= priority {
                return false;
            }
            if self
                .inner
                .finalize_priority
                .compare_exchange(prev, priority, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Clear all handles after the session finishes.
    pub fn clear(&self) {
        self.inner.stop_info.lock().unwrap().take();
        let mut guard = self.inner.external_stop_flag.lock().unwrap();
        *guard = None;
        self.inner.finalize_claimed.store(false, std::sync::atomic::Ordering::Release);
        self.inner.finalize_priority.store(0, Ordering::Release);
    }

    /// Force clear references (advances session epoch).
    ///
    /// Callers are expected to follow this with `publish_session()` so a fresh
    /// stop flag replaces the one we force-set here; this prevents the previous
    /// session's flag value from leaking into the next session.
    pub fn force_clear(&self) {
        {
            let mut guard = self.inner.external_stop_flag.lock().unwrap();
            if let Some(flag) = guard.as_ref().and_then(|w| w.upgrade()) {
                // Ensure current observers see a stop before the handle is dropped.
                flag.store(true, std::sync::atomic::Ordering::Release);
            }
            *guard = None;
        }
        self.inner.stop_info.lock().unwrap().take();
        self.inner.finalize_claimed.store(false, std::sync::atomic::Ordering::Release);
        self.inner.finalize_priority.store(0, Ordering::Release);
        self.inner.session_id.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Request immediate stop (best-effort broadcast).
    pub fn request_stop(&self) {
        self.set_external_stop_flag();
        let mut guard = self.inner.stop_info.lock().unwrap();
        let mut si = guard.take().unwrap_or_default();
        use crate::search::types::TerminationReason;
        if matches!(si.reason, TerminationReason::Completed) {
            si.reason = TerminationReason::UserStop;
            si.hard_timeout = false;
        }
        *guard = Some(si);
    }

    /// Request only the external stop flag without mutating StopInfo.
    pub fn request_stop_flag_only(&self) {
        self.set_external_stop_flag();
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
        if !self.should_accept_finalize(reason) {
            return;
        }
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

    /// Try reading StopInfo snapshot of the current session.
    pub fn try_read_stop_info(&self) -> Option<crate::search::types::StopInfo> {
        self.inner.stop_info.lock().unwrap().clone()
    }

    /// Try reading a consistent root snapshot for the active session.
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
        let current_snapshot = self.publisher.try_read();
        let depth_u8 = (line.depth as u8).min(127);
        // Publish only if we do NOT have an existing snapshot for this session whose depth is already deeper.
        let should_publish = !matches!(
            current_snapshot.as_ref(),
            Some(existing)
                if existing.search_id == session_id && existing.depth > depth_u8
        );

        let total_nodes = line.nodes.unwrap_or(0);
        let total_time_ms = line.time_ms.unwrap_or(0);

        if should_publish {
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
            snap.depth = depth_u8;
            snap.score_cp = line.score_cp;
            snap.nodes = total_nodes;
            snap.elapsed_ms = total_time_ms.min(u32::MAX as u64) as u32;
            self.publisher.publish(&snap);
        }
        // Update StopInfo snapshot (best-effort)
        let mut guard = self.inner.stop_info.lock().unwrap();
        let mut si = guard.take().unwrap_or_default();
        si.elapsed_ms = si.elapsed_ms.max(total_time_ms);
        si.nodes = si.nodes.max(total_nodes);
        si.depth_reached = si.depth_reached.max(depth_u8);
        // note: request_finalize/request_stop が reason/hard_timeout を確定させる。
        // publish_root_line は進捗値のみ更新し、理由フラグは上書きしない。
        *guard = Some(si);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::types::StopInfo;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::{mpsc, Arc};

    #[test]
    fn finalize_priority_prefers_hard() {
        let ctrl = StopController::new();
        let ext_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        ctrl.register_finalizer(tx);
        ctrl.publish_session(Some(&ext_flag), 7);

        match rx.recv().unwrap() {
            FinalizerMsg::SessionStart { session_id } => assert_eq!(session_id, 7),
            other => panic!("expected SessionStart, got {other:?}"),
        }

        ctrl.request_finalize(FinalizeReason::Planned);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => assert_eq!(reason, FinalizeReason::Planned),
            other => panic!("expected Planned finalize, got {other:?}"),
        }
        assert!(ext_flag.load(AtomicOrdering::Relaxed));

        ext_flag.store(false, AtomicOrdering::Relaxed);
        ctrl.request_finalize(FinalizeReason::Hard);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => assert_eq!(reason, FinalizeReason::Hard),
            other => panic!("expected Hard finalize, got {other:?}"),
        }
        assert!(ext_flag.load(AtomicOrdering::Relaxed));

        ext_flag.store(false, AtomicOrdering::Relaxed);
        ctrl.request_finalize(FinalizeReason::TimeManagerStop);
        assert!(rx.try_recv().is_err(), "lower-priority finalize should be ignored");
        assert!(!ext_flag.load(AtomicOrdering::Relaxed));
    }

    #[test]
    fn finalize_priority_resets_per_session() {
        let ctrl = StopController::new();
        let ext_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        ctrl.register_finalizer(tx);

        ctrl.publish_session(Some(&ext_flag), 1);
        let _ = rx.recv().unwrap(); // SessionStart
        ctrl.request_finalize(FinalizeReason::Hard);
        let _ = rx.recv().unwrap(); // Hard finalize

        ctrl.publish_session(Some(&ext_flag), 2);
        match rx.recv().unwrap() {
            FinalizerMsg::SessionStart { session_id } => assert_eq!(session_id, 2),
            other => panic!("expected SessionStart, got {other:?}"),
        }

        ctrl.request_finalize(FinalizeReason::Planned);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => assert_eq!(reason, FinalizeReason::Planned),
            other => panic!("expected Planned finalize, got {other:?}"),
        }
    }

    #[test]
    fn finalize_priority_nearhard_escalates_to_hard() {
        let ctrl = StopController::new();
        let ext_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        ctrl.register_finalizer(tx);
        ctrl.publish_session(Some(&ext_flag), 99);
        let _ = rx.recv().unwrap(); // SessionStart

        ctrl.request_finalize(FinalizeReason::NearHard);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => assert_eq!(reason, FinalizeReason::NearHard),
            other => panic!("expected NearHard finalize, got {other:?}"),
        }
        assert!(ext_flag.load(AtomicOrdering::Relaxed));

        ext_flag.store(false, AtomicOrdering::Relaxed);
        ctrl.request_finalize(FinalizeReason::Hard);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => assert_eq!(reason, FinalizeReason::Hard),
            other => panic!("expected Hard finalize, got {other:?}"),
        }
        assert!(ext_flag.load(AtomicOrdering::Relaxed));

        ext_flag.store(false, AtomicOrdering::Relaxed);
        ctrl.request_finalize(FinalizeReason::Planned);
        assert!(rx.try_recv().is_err());
        assert!(!ext_flag.load(AtomicOrdering::Relaxed));
    }

    #[test]
    fn finalize_priority_hard_persists_after_user_stop() {
        use crate::search::types::TerminationReason;

        let ctrl = StopController::new();
        let ext_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        ctrl.register_finalizer(tx);
        ctrl.publish_session(Some(&ext_flag), 77);
        let _ = rx.recv().unwrap(); // SessionStart
        ctrl.prime_stop_info(StopInfo::default());

        ctrl.request_finalize(FinalizeReason::Hard);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => assert_eq!(reason, FinalizeReason::Hard),
            other => panic!("expected Hard finalize, got {other:?}"),
        }
        let info_after_hard = ctrl.try_read_stop_info().expect("stop info present");
        assert!(info_after_hard.hard_timeout);
        assert_eq!(info_after_hard.reason, TerminationReason::TimeLimit);

        // Later user stop should be ignored (no downgrade)
        ctrl.request_finalize(FinalizeReason::UserStop);
        assert!(rx.try_recv().is_err(), "UserStop must not emit after Hard");
        let info_after_user = ctrl.try_read_stop_info().expect("stop info present");
        assert!(info_after_user.hard_timeout);
        assert_eq!(info_after_user.reason, TerminationReason::TimeLimit);
    }

    #[test]
    fn finalize_priority_planned_escalates_to_nearhard() {
        let ctrl = StopController::new();
        let ext_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        ctrl.register_finalizer(tx);
        ctrl.publish_session(Some(&ext_flag), 11);
        let _ = rx.recv().unwrap();

        ctrl.request_finalize(FinalizeReason::Planned);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => assert_eq!(reason, FinalizeReason::Planned),
            other => panic!("expected Planned finalize, got {other:?}"),
        }

        ext_flag.store(false, AtomicOrdering::Relaxed);
        ctrl.request_finalize(FinalizeReason::NearHard);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => assert_eq!(reason, FinalizeReason::NearHard),
            other => panic!("expected NearHard finalize, got {other:?}"),
        }
    }

    #[test]
    fn finalize_priority_same_reason_is_idempotent() {
        let ctrl = StopController::new();
        let ext_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        ctrl.register_finalizer(tx);
        ctrl.publish_session(Some(&ext_flag), 12);
        let _ = rx.recv().unwrap();

        ctrl.request_finalize(FinalizeReason::TimeManagerStop);
        match rx.recv().unwrap() {
            FinalizerMsg::Finalize { reason, .. } => {
                assert_eq!(reason, FinalizeReason::TimeManagerStop)
            }
            other => panic!("expected TimeManagerStop finalize, got {other:?}"),
        }

        assert!(rx.try_recv().is_err());
        ctrl.request_finalize(FinalizeReason::TimeManagerStop);
        assert!(rx.try_recv().is_err(), "duplicate TimeManagerStop must be ignored");
    }

    #[test]
    fn publish_session_clears_stop_info() {
        let ctrl = StopController::new();
        ctrl.prime_stop_info(StopInfo::default());
        assert!(ctrl.try_read_stop_info().is_some());
        ctrl.publish_session(None, 1);
        assert!(ctrl.try_read_stop_info().is_none());
    }

    #[test]
    fn clear_and_force_clear_reset_stop_info() {
        let ctrl = StopController::new();
        ctrl.prime_stop_info(StopInfo::default());
        ctrl.clear();
        assert!(ctrl.try_read_stop_info().is_none());

        ctrl.prime_stop_info(StopInfo::default());
        ctrl.force_clear();
        assert!(ctrl.try_read_stop_info().is_none());
    }

    #[test]
    fn root_snapshot_skips_shallower_updates() {
        use crate::search::types::{Bound, RootLine};
        use crate::shogi::Move;
        use smallvec::SmallVec;

        let ctrl = StopController::new();
        ctrl.publish_session(None, 5);
        ctrl.prime_stop_info(StopInfo::default());

        let mut pv = SmallVec::<[Move; 32]>::new();
        pv.push(Move::null());
        let base_line = |depth: u32, nodes: u64, time_ms: u64| RootLine {
            multipv_index: 1,
            root_move: Move::null(),
            score_internal: 0,
            score_cp: 12,
            bound: Bound::Exact,
            depth,
            seldepth: Some(depth as u8),
            pv: pv.clone(),
            nodes: Some(nodes),
            time_ms: Some(time_ms),
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        };

        ctrl.publish_root_line(5, 0xABC, &base_line(14, 10_000, 120));
        let snap_high = ctrl.try_read_snapshot().expect("snapshot present");
        assert_eq!(snap_high.depth, 14);

        ctrl.publish_root_line(5, 0xABC, &base_line(9, 20_000, 180));
        let snap_after = ctrl.try_read_snapshot().expect("snapshot present");
        assert_eq!(snap_after.depth, 14, "shallower update must be ignored");

        let stop_info = ctrl.try_read_stop_info().expect("stop info present");
        assert_eq!(stop_info.depth_reached, 14);
        assert!(stop_info.nodes >= 20_000);
        assert!(stop_info.elapsed_ms >= 180);
    }
}
