use crate::shogi::Move;
use smallvec::SmallVec;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

/// Fixed-capacity PV length
const MAX_PV: usize = 64;

/// Read-only snapshot of current root analysis state
#[derive(Clone, Debug, Default)]
pub struct RootSnapshot {
    pub search_id: u64,
    pub root_key: u64,
    pub best: Option<Move>,
    pub pv: SmallVec<[Move; MAX_PV]>,
    pub depth: u8,
    pub score_cp: i32,
    pub nodes: u64,
    pub elapsed_ms: u32,
}

/// Seqlock + double-buffer publisher for RootSnapshot
pub struct RootSnapshotPublisher {
    seq: AtomicU64,
    active_idx: AtomicU8, // 0 or 1
    buf0: UnsafeCell<RootSnapshot>,
    buf1: UnsafeCell<RootSnapshot>,
}

unsafe impl Sync for RootSnapshotPublisher {}

impl Default for RootSnapshotPublisher {
    fn default() -> Self {
        Self {
            seq: AtomicU64::new(0),
            active_idx: AtomicU8::new(0),
            buf0: UnsafeCell::new(RootSnapshot::default()),
            buf1: UnsafeCell::new(RootSnapshot::default()),
        }
    }
}

impl RootSnapshotPublisher {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    fn inactive_ptr(&self) -> *mut RootSnapshot {
        let idx = self.active_idx.load(Ordering::Relaxed);
        if idx == 0 {
            self.buf1.get()
        } else {
            self.buf0.get()
        }
    }

    #[inline]
    fn active_ref(&self) -> &RootSnapshot {
        if self.active_idx.load(Ordering::Relaxed) == 0 {
            unsafe { &*self.buf0.get() }
        } else {
            unsafe { &*self.buf1.get() }
        }
    }

    /// Publish a new snapshot (seqlock write path)
    pub fn publish(&self, snap: &RootSnapshot) {
        // Begin write: odd seq
        self.seq.fetch_add(1, Ordering::AcqRel);
        // Write to inactive buffer (raw pointer write to avoid &mut from &self)
        let dst = self.inactive_ptr();
        unsafe {
            std::ptr::write(dst, snap.clone());
        }
        // Flip active index
        let new_idx = self.active_idx.load(Ordering::Relaxed) ^ 1;
        self.active_idx.store(new_idx, Ordering::Release);
        // End write: even seq
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// Try to read a consistent snapshot (seqlock read path)
    pub fn try_read(&self) -> Option<RootSnapshot> {
        for _ in 0..3 {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 != 0 {
                continue;
            }
            let src = self.active_ref();
            let copy = src.clone();
            let s2 = self.seq.load(Ordering::Acquire);
            if s1 == s2 && (s2 & 1) == 0 {
                return Some(copy);
            }
        }
        None
    }
}
