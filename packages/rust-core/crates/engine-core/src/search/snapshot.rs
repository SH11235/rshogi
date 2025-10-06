use crate::search::types::{NodeType, RootLine};
use crate::shogi::Move;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Maximum number of moves stored in the root PV snapshot (64 matches USI expectations).
const MAX_PV: usize = 64;

/// Origin of a published root snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotSource {
    /// Snapshot was produced from a fully committed (exact) iteration.
    Stable,
    /// Snapshot reflects an in-progress iteration (aspiration/partial result).
    Partial,
}

/// Read-only snapshot of current root analysis state.
#[derive(Clone, Debug)]
pub struct RootSnapshot {
    pub search_id: u64,
    pub root_key: u64,
    pub best: Option<Move>,
    pub ponder: Option<Move>,
    pub pv: SmallVec<[Move; MAX_PV]>,
    pub lines: SmallVec<[RootLine; 4]>,
    pub depth: u8,
    pub seldepth: Option<u8>,
    pub score_cp: i32,
    pub node_type: NodeType,
    pub nodes: u64,
    pub elapsed_ms: u64,
    pub version: u64,
    pub source: SnapshotSource,
}

impl Default for RootSnapshot {
    fn default() -> Self {
        Self {
            search_id: 0,
            root_key: 0,
            best: None,
            ponder: None,
            pv: SmallVec::new(),
            lines: SmallVec::new(),
            depth: 0,
            seldepth: None,
            score_cp: 0,
            node_type: NodeType::Exact,
            nodes: 0,
            elapsed_ms: 0,
            version: 0,
            source: SnapshotSource::Partial,
        }
    }
}

impl RootSnapshot {
    #[inline]
    fn clamp_depth(depth: u32) -> u8 {
        depth.min(u8::MAX as u32) as u8
    }

    /// Build a snapshot from a slice of sorted root lines.
    pub fn from_lines(
        search_id: u64,
        root_key: u64,
        lines: &[RootLine],
        nodes: u64,
        elapsed_ms: u64,
        source: SnapshotSource,
    ) -> Self {
        let mut snapshot = RootSnapshot {
            search_id,
            root_key,
            nodes,
            elapsed_ms: elapsed_ms.min(u32::MAX as u64),
            source,
            ..RootSnapshot::default()
        };

        if let Some(first) = lines.first() {
            snapshot.best = Some(first.root_move);
            snapshot.seldepth = first.seldepth;
            snapshot.score_cp = first.score_cp;
            snapshot.node_type = first.bound;
            snapshot.depth = Self::clamp_depth(first.depth);
            let mut pv: SmallVec<[Move; MAX_PV]> = SmallVec::new();
            pv.extend_from_slice(&first.pv);
            if pv.is_empty() {
                pv.push(first.root_move);
            }
            snapshot.ponder = pv.get(1).copied();
            snapshot.pv = pv;
        }

        let mut stored_lines: SmallVec<[RootLine; 4]> = SmallVec::new();
        stored_lines.extend(lines.iter().cloned());
        snapshot.lines = stored_lines;
        snapshot
    }

    /// Build a snapshot from a single root line (typically partial publication).
    pub fn from_line(
        search_id: u64,
        root_key: u64,
        line: &RootLine,
        source: SnapshotSource,
    ) -> Self {
        let nodes = line.nodes.unwrap_or(0);
        let elapsed_ms = line.time_ms.unwrap_or(0);
        Self::from_lines(search_id, root_key, std::slice::from_ref(line), nodes, elapsed_ms, source)
    }
}

/// Shared storage for root snapshots backed by an `Arc<RwLock<...>>`.
#[derive(Clone, Default)]
pub struct RootSnapshotSlot {
    inner: Arc<RwLock<Option<RootSnapshot>>>,
    version: Arc<AtomicU64>,
}

impl RootSnapshotSlot {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn load(&self) -> Option<RootSnapshot> {
        self.inner.read().expect("snapshot rwlock poisoned").clone()
    }

    #[inline]
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = None;
        }
        self.version.store(0, Ordering::Release);
    }

    #[inline]
    pub fn commit(&self, mut snapshot: RootSnapshot) {
        let version = self.version.fetch_add(1, Ordering::AcqRel) + 1;
        snapshot.version = version;
        *self.inner.write().expect("snapshot rwlock poisoned") = Some(snapshot);
    }

    #[inline]
    pub fn handle(&self) -> Arc<RwLock<Option<RootSnapshot>>> {
        Arc::clone(&self.inner)
    }
}
