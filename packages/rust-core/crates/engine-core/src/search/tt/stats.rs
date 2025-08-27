//! Transposition table statistics for performance analysis

use std::sync::atomic::{AtomicU64, Ordering};

/// Statistics collector for transposition table access patterns
#[derive(Default)]
pub struct TTStats {
    /// Total number of TT probes
    pub probes: AtomicU64,
    /// Number of successful hits
    pub hits: AtomicU64,
    /// Number of prefetch operations
    pub prefetches: AtomicU64,
    /// Number of stores
    pub stores: AtomicU64,
    /// Number of hash collisions detected
    pub collisions: AtomicU64,
}

impl TTStats {
    /// Create new statistics collector
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a probe attempt
    #[inline]
    pub fn record_probe(&self) {
        self.probes.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a successful hit
    #[inline]
    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a prefetch operation
    #[inline]
    pub fn record_prefetch(&self) {
        self.prefetches.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a store operation
    #[inline]
    pub fn record_store(&self) {
        self.stores.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a hash collision
    #[inline]
    pub fn record_collision(&self) {
        self.collisions.fetch_add(1, Ordering::Relaxed);
    }

    /// Get hit rate as percentage
    pub fn hit_rate(&self) -> f64 {
        let probes = self.probes.load(Ordering::Relaxed);
        let hits = self.hits.load(Ordering::Relaxed);
        if probes == 0 {
            0.0
        } else {
            (hits as f64 / probes as f64) * 100.0
        }
    }

    /// Reset all statistics
    pub fn reset(&self) {
        self.probes.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
        self.prefetches.store(0, Ordering::Relaxed);
        self.stores.store(0, Ordering::Relaxed);
        self.collisions.store(0, Ordering::Relaxed);
    }

    /// Get a summary of statistics
    pub fn summary(&self) -> String {
        format!(
            "TT Stats: probes={}, hits={}, hit_rate={:.1}%, prefetches={}, stores={}, collisions={}",
            self.probes.load(Ordering::Relaxed),
            self.hits.load(Ordering::Relaxed),
            self.hit_rate(),
            self.prefetches.load(Ordering::Relaxed),
            self.stores.load(Ordering::Relaxed),
            self.collisions.load(Ordering::Relaxed),
        )
    }
}
