//! Stop controller for coordinating search finalization and stop requests.
//!
//! Provides a unified interface for stopping searches, publishing snapshots,
//! and issuing finalize requests with priority handling.

use crate::search::snapshot::{RootSnapshot, RootSnapshotSlot, SnapshotSource};
use crate::search::types::{Bound, StopInfo};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const STRICT_SESSION_ASSERT: bool = cfg!(feature = "strict-stop-session-assert");

/// Reason for an out-of-band finalize request issued by time management or other guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeReason {
    /// Hard deadline reached (elapsed >= hard)
    Hard,
    /// Approaching hard deadline window (near-hard finalize window)
    NearHard,
    /// Planned rounded stop time reached/near
    Planned,
    /// Planned stop due to detected short mate (distance <= K)
    ///
    /// Carries mate distance (plies, positive when we mate the opponent) and whether
    /// the session was in ponder mode when triggered. Treated similarly to Planned
    /// for priority and StopInfo semantics; primarily used for diagnostics.
    PlannedMate { distance: i32, was_ponder: bool },
    /// Generic time-manager stop (e.g., node limit or emergency)
    TimeManagerStop,
    /// User/GUI stop propagation (for consistency)
    UserStop,
    /// Ponder to move transition (ponderhit with search already completed)
    PonderToMove,
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
    snapshot_slot: RootSnapshotSlot,
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
            snapshot_slot: RootSnapshotSlot::new(),
        }
    }

    /// Publish the current session (external stop flag + session id).
    pub fn publish_session(&self, external_stop: Option<&Arc<AtomicBool>>, session_id: u64) {
        self.update_external_stop_flag(external_stop);
        // 古いセッションの StopInfo が残らないよう明示的にクリアしてから ID を更新。
        self.inner.stop_info.lock().unwrap().take();
        self.inner.session_id.store(session_id, std::sync::atomic::Ordering::Release);
        self.inner.finalize_claimed.store(false, std::sync::atomic::Ordering::Release);
        self.inner.finalize_priority.store(0, Ordering::Release);
        self.snapshot_slot.clear();
        if let Some(tx) = self.inner.finalizer_tx.lock().unwrap().as_ref() {
            let _ = tx.send(FinalizerMsg::SessionStart { session_id });
        }
    }

    /// Initialize StopInfo snapshot for the upcoming session.
    ///
    /// # Order
    ///
    /// `publish_session()` clears any pending snapshot via `stop_info.take()`. Call
    /// this method *after* `publish_session()` so that the primed value remains
    /// visible to the backend workers.
    pub fn prime_stop_info(&self, info: StopInfo) {
        let mut guard = self.inner.stop_info.lock().unwrap();
        *guard = Some(info);
    }

    /// Update only the external stop flag reference.
    ///
    /// # Contract
    ///
    /// セッション境界専用。Exactly-once セマンティクスは呼び出し元（`publish_session()`）で
    /// `finalize_claimed` / `finalize_priority` をリセットすることで維持する。
    fn update_external_stop_flag(&self, external_stop: Option<&Arc<AtomicBool>>) {
        let mut guard = self.inner.external_stop_flag.lock().unwrap();
        *guard = external_stop.map(Arc::downgrade);
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
            // Treat PlannedMate a bit higher than generic Planned to preempt lower-priority caps
            FinalizeReason::PlannedMate { .. } => 4,
            FinalizeReason::Planned => 3,
            FinalizeReason::PonderToMove => 3,
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
        self.snapshot_slot.clear();
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
        self.snapshot_slot.clear();
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
    ///
    /// `StopInfo.reason` follows a "highest accepted priority wins" policy: lower
    /// priority finalize requests that are rejected by `should_accept_finalize()` do
    /// not overwrite the snapshot. As a result the recorded reason always reflects
    /// the most severe finalize that was actually accepted for the session.
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
                si.stop_tag.get_or_insert_with(|| "hard_deadline".to_string());
            }
            FinalizeReason::NearHard
            | FinalizeReason::Planned
            | FinalizeReason::PlannedMate { .. }
            | FinalizeReason::PonderToMove
            | FinalizeReason::TimeManagerStop => {
                si.reason = TerminationReason::TimeLimit;
                si.hard_timeout = false;
            }
            FinalizeReason::UserStop => {
                si.reason = TerminationReason::UserStop;
                si.hard_timeout = false;
            }
        }
        // Attach a diagnostic tag for PlannedMate if available
        if let FinalizeReason::PlannedMate { distance, .. } = reason {
            si.stop_tag = Some(format!("planned_mate K={}", distance));
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
        self.snapshot_slot.load()
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
        let expected_sid = self.inner.session_id.load(Ordering::Acquire);
        if expected_sid != session_id {
            if STRICT_SESSION_ASSERT {
                debug_assert_eq!(
                    expected_sid, session_id,
                    "publish_root_line received mismatched session_id"
                );
            }
            static WARN_FILTER: OnceLock<Mutex<HashSet<(u64, u64, u64)>>> = OnceLock::new();
            let should_warn = {
                let guard = WARN_FILTER.get_or_init(|| Mutex::new(HashSet::new()));
                let mut cache = guard.lock().unwrap();
                if cache.len() > 1024 {
                    cache.clear();
                }
                cache.insert((expected_sid, session_id, root_key))
            };
            if should_warn {
                log::warn!(
                    "publish_root_line session_id mismatch expected={} got={} root_key={:016x}",
                    expected_sid,
                    session_id,
                    root_key
                );
            }
            return;
        }

        let candidate =
            RootSnapshot::from_line(session_id, root_key, line, SnapshotSource::Partial);
        self.consider_snapshot(candidate);

        let depth_u8 = line.depth.min(u8::MAX as u32) as u8;
        let total_nodes = line.nodes.unwrap_or(0);
        let total_time_ms = line.time_ms.unwrap_or(0);

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

    pub fn publish_committed_snapshot(
        &self,
        session_id: u64,
        root_key: u64,
        lines: &[crate::search::types::RootLine],
        nodes: u64,
        elapsed_ms: u64,
    ) {
        if lines.is_empty() {
            return;
        }
        let expected_sid = self.inner.session_id.load(Ordering::Acquire);
        if expected_sid != session_id {
            if STRICT_SESSION_ASSERT {
                debug_assert_eq!(
                    expected_sid, session_id,
                    "publish_committed_snapshot received mismatched session_id"
                );
            }
            static WARN_FILTER: OnceLock<Mutex<HashSet<(u64, u64, u64)>>> = OnceLock::new();
            let should_warn = {
                let guard = WARN_FILTER.get_or_init(|| Mutex::new(HashSet::new()));
                let mut cache = guard.lock().unwrap();
                if cache.len() > 1024 {
                    cache.clear();
                }
                cache.insert((expected_sid, session_id, root_key))
            };
            if should_warn {
                log::warn!(
                    "publish_committed_snapshot session_id mismatch expected={} got={} root_key={:016x}",
                    expected_sid,
                    session_id,
                    root_key
                );
            }
            return;
        }

        let Some(first) = lines.first() else {
            return;
        };
        if !matches!(first.bound, Bound::Exact) {
            return;
        }

        let snapshot = RootSnapshot::from_lines(
            session_id,
            root_key,
            lines,
            nodes,
            elapsed_ms,
            SnapshotSource::Stable,
        );
        self.consider_snapshot(snapshot);

        let mut guard = self.inner.stop_info.lock().unwrap();
        let mut si = guard.take().unwrap_or_default();
        let depth_u8 = first.depth.min(u8::MAX as u32) as u8;
        si.elapsed_ms = si.elapsed_ms.max(elapsed_ms);
        si.nodes = si.nodes.max(nodes);
        si.depth_reached = si.depth_reached.max(depth_u8);
        *guard = Some(si);
    }

    fn consider_snapshot(&self, candidate: RootSnapshot) {
        let should_publish = match self.snapshot_slot.load() {
            Some(existing)
                if existing.search_id == candidate.search_id
                    && existing.root_key == candidate.root_key =>
            {
                match (existing.source, candidate.source) {
                    (SnapshotSource::Stable, SnapshotSource::Partial) => false,
                    (SnapshotSource::Stable, SnapshotSource::Stable) => {
                        candidate.depth >= existing.depth
                    }
                    (SnapshotSource::Partial, SnapshotSource::Stable) => true,
                    (SnapshotSource::Partial, SnapshotSource::Partial) => {
                        // 同深さ Partial → Partial は PV 先頭手が変わらなくても診断メトリクス
                        // 更新を優先する。ログの揺れは増えるが、進捗監視の粒度を落とさないため
                        // のポリシー。より安定表示が必要になった場合はここで手番比較などを追加。
                        candidate.depth >= existing.depth
                    }
                }
            }
            _ => true,
        };

        if should_publish {
            self.snapshot_slot.commit(candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::types::{StopInfo, TerminationReason};
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
    fn try_claim_finalize_resets_per_session() {
        let ctrl = StopController::new();

        ctrl.publish_session(None, 1);
        assert!(ctrl.try_claim_finalize());
        assert!(!ctrl.try_claim_finalize(), "second claim within session must fail");

        ctrl.publish_session(None, 2);
        assert!(ctrl.try_claim_finalize(), "new session must allow finalize claim again");
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

    #[test]
    fn root_snapshot_allows_equal_depth_updates() {
        use crate::search::types::{Bound, RootLine};
        use crate::shogi::Move;
        use smallvec::SmallVec;

        let ctrl = StopController::new();
        ctrl.publish_session(None, 12);

        let mut pv = SmallVec::<[Move; 32]>::new();
        pv.push(Move::null());
        let base_line = |nodes: u64, time_ms: u64| RootLine {
            multipv_index: 1,
            root_move: Move::null(),
            score_internal: 0,
            score_cp: 50,
            bound: Bound::Exact,
            depth: 16,
            seldepth: Some(16),
            pv: pv.clone(),
            nodes: Some(nodes),
            time_ms: Some(time_ms),
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        };

        ctrl.publish_root_line(12, 0x1234, &base_line(20_000, 150));
        let first = ctrl.try_read_snapshot().expect("snapshot present");
        assert_eq!(first.depth, 16);
        assert_eq!(first.nodes, 20_000);

        ctrl.publish_root_line(12, 0x1234, &base_line(28_000, 210));
        let updated = ctrl.try_read_snapshot().expect("snapshot present");
        assert_eq!(updated.depth, 16);
        assert_eq!(updated.nodes, 28_000, "equal depth update should refresh metrics");
        assert_eq!(updated.elapsed_ms, 210);

        let info = ctrl.try_read_stop_info().expect("stop info present");
        assert_eq!(info.depth_reached, 16);
        assert_eq!(info.nodes, 28_000);
        assert_eq!(info.elapsed_ms, 210);
    }

    #[test]
    fn root_snapshot_progress_is_monotonic() {
        use crate::search::types::{Bound, RootLine};
        use crate::shogi::Move;
        use smallvec::SmallVec;

        let ctrl = StopController::new();
        ctrl.publish_session(None, 77);
        ctrl.prime_stop_info(StopInfo::default());

        let mut pv = SmallVec::<[Move; 32]>::new();
        pv.push(Move::null());

        let make_line = |depth: u32, nodes: u64, time_ms: u64| RootLine {
            multipv_index: 1,
            root_move: Move::null(),
            score_internal: 0,
            score_cp: 32,
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

        let updates = [(8, 1_000u64, 20u64), (12, 5_000, 60), (15, 12_000, 120)];
        let mut last_depth = 0;
        let mut last_nodes = 0;
        let mut last_time = 0;

        for (depth, nodes, elapsed) in updates {
            ctrl.publish_root_line(77, 0xAA55_AA55, &make_line(depth, nodes, elapsed));
            let snapshot = ctrl.try_read_snapshot().expect("snapshot present");
            assert!(snapshot.depth >= last_depth);
            assert!(snapshot.nodes >= last_nodes);
            assert!(snapshot.elapsed_ms >= last_time);
            last_depth = snapshot.depth;
            last_nodes = snapshot.nodes;
            last_time = snapshot.elapsed_ms;

            let stop_info = ctrl.try_read_stop_info().expect("stop info present");
            assert!(stop_info.depth_reached >= last_depth);
            assert!(stop_info.nodes >= last_nodes);
            assert!(stop_info.elapsed_ms >= last_time);
        }
    }

    #[test]
    fn request_stop_flag_only_keeps_stop_info_reason() {
        let ctrl = StopController::new();
        let info = StopInfo {
            reason: TerminationReason::TimeLimit,
            hard_timeout: true,
            ..Default::default()
        };
        ctrl.prime_stop_info(info.clone());

        ctrl.request_stop_flag_only();

        let after = ctrl.try_read_stop_info().expect("stop info present");
        assert_eq!(after.reason, info.reason);
        assert_eq!(after.hard_timeout, info.hard_timeout);
    }

    #[test]
    fn finalize_concurrency_prefers_highest_priority() {
        use std::thread;

        let ctrl = StopController::new();
        let (tx, rx) = mpsc::channel();
        ctrl.register_finalizer(tx);
        let external = Arc::new(AtomicBool::new(false));
        ctrl.publish_session(Some(&external), 99);

        // Drain initial SessionStart
        match rx.recv().unwrap() {
            FinalizerMsg::SessionStart { session_id } => assert_eq!(session_id, 99),
            other => panic!("unexpected message: {other:?}"),
        }

        let ctrl_clone1 = ctrl.clone();
        let ctrl_clone2 = ctrl.clone();
        let ctrl_clone3 = ctrl.clone();

        let t1 = thread::spawn(move || {
            ctrl_clone1.request_finalize(FinalizeReason::Planned);
        });
        let t2 = thread::spawn(move || {
            ctrl_clone2.request_finalize(FinalizeReason::NearHard);
        });
        let t3 = thread::spawn(move || {
            ctrl_clone3.request_finalize(FinalizeReason::Hard);
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        // Collect all finalize messages and ensure the highest priority (Hard) wins last.
        let mut reasons = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let FinalizerMsg::Finalize { reason, .. } = msg {
                reasons.push(reason);
            }
        }

        assert!(reasons.contains(&FinalizeReason::Hard));
        if let Some(idx) = reasons.iter().position(|&r| r == FinalizeReason::Hard) {
            for r in &reasons[idx..] {
                assert_eq!(
                    *r,
                    FinalizeReason::Hard,
                    "no lower-priority finalize should appear after Hard",
                );
            }
        }
    }

    #[test]
    fn publish_root_line_ignores_mismatched_session() {
        use crate::search::types::{Bound, RootLine};
        use crate::shogi::Move;
        use smallvec::SmallVec;

        let ctrl = StopController::new();
        let external = Arc::new(AtomicBool::new(false));
        ctrl.publish_session(Some(&external), 10);
        ctrl.prime_stop_info(StopInfo {
            depth_reached: 5,
            nodes: 100,
            elapsed_ms: 40,
            ..Default::default()
        });

        let mut pv = SmallVec::<[Move; 32]>::new();
        pv.push(Move::null());
        let line = RootLine {
            multipv_index: 1,
            root_move: Move::null(),
            score_internal: 0,
            score_cp: 12,
            bound: Bound::Exact,
            depth: 8,
            seldepth: Some(8),
            pv: pv.clone(),
            nodes: Some(500),
            time_ms: Some(120),
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        };

        let before = ctrl.try_read_snapshot();

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ctrl.publish_root_line(11, 0xABC, &line);
        }));

        if cfg!(all(debug_assertions, feature = "strict-stop-session-assert")) {
            assert!(
                res.is_err(),
                "mismatched session should panic when strict session assert is enabled"
            );
        } else {
            assert!(res.is_ok());
            let after = ctrl.try_read_snapshot();
            assert_eq!(
                before.as_ref().map(|s| s.search_id),
                after.as_ref().map(|s| s.search_id),
                "snapshot search_id must remain unchanged for mismatched session"
            );
            let info = ctrl.try_read_stop_info().expect("stop info");
            assert_eq!(info.depth_reached, 5);
            assert_eq!(info.nodes, 100);
            assert_eq!(info.elapsed_ms, 40);
        }
    }

    #[test]
    fn request_finalize_planned_mate_sets_time_limit_non_hard() {
        use crate::search::types::TerminationReason;
        let ctrl = StopController::new();
        let external = Arc::new(AtomicBool::new(false));
        ctrl.publish_session(Some(&external), 2025);
        // Prime a default StopInfo so request_finalize updates fields
        ctrl.prime_stop_info(StopInfo::default());

        ctrl.request_finalize(FinalizeReason::PlannedMate {
            distance: 1,
            was_ponder: false,
        });

        let info = ctrl.try_read_stop_info().expect("stop info present");
        assert_eq!(info.reason, TerminationReason::TimeLimit);
        assert!(!info.hard_timeout);
        // External stop flag should be set
        assert!(external.load(AtomicOrdering::Acquire));
    }

    #[test]
    fn snapshot_depth_is_clamped_to_u8_max() {
        use crate::search::types::{Bound, RootLine};
        use crate::shogi::Move;
        use smallvec::SmallVec;

        let ctrl = StopController::new();
        let external = Arc::new(AtomicBool::new(false));
        ctrl.publish_session(Some(&external), 4242);
        ctrl.prime_stop_info(StopInfo::default());

        let mut pv = SmallVec::<[Move; 32]>::new();
        pv.push(Move::null());
        let deep_line = RootLine {
            multipv_index: 1,
            root_move: Move::null(),
            score_internal: 0,
            score_cp: 0,
            bound: Bound::Exact,
            depth: 300,
            seldepth: Some(64),
            pv: pv.clone(),
            nodes: Some(42_000),
            time_ms: Some(120),
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        };

        ctrl.publish_root_line(4242, 0xDEADBEEF, &deep_line);
        let partial_info =
            ctrl.try_read_stop_info().expect("stop info present after partial publish");
        assert_eq!(partial_info.depth_reached, u8::MAX, "depth must saturate at u8::MAX");

        let mut lines = SmallVec::<[RootLine; 4]>::new();
        lines.push(deep_line.clone());
        ctrl.publish_committed_snapshot(4242, 0xDEADBEEF, lines.as_slice(), 42_000, 180);

        let stable = ctrl.try_read_snapshot().expect("stable snapshot should be published");
        assert_eq!(stable.depth, u8::MAX, "snapshot depth should be clamped");
        let stable_info =
            ctrl.try_read_stop_info().expect("stop info present after stable publish");
        assert_eq!(stable_info.depth_reached, u8::MAX, "StopInfo depth should remain clamped");
    }
}
