//! Detailed metrics collection for transposition table performance analysis

#[cfg(feature = "tt_metrics")]
use std::sync::atomic::AtomicU64 as StdAtomicU64;

/// Detailed metrics for TT performance analysis
#[cfg(feature = "tt_metrics")]
#[derive(Default)]
pub struct DetailedTTMetrics {
    // CAS-related (future use)
    pub cas_attempts: StdAtomicU64,
    pub cas_successes: StdAtomicU64,
    pub cas_failures: StdAtomicU64,
    pub cas_key_match: StdAtomicU64, // CAS failed but key matched (Phase 5 optimization)

    // Update pattern analysis
    pub update_existing: StdAtomicU64, // Updates to existing entries
    pub replace_empty: StdAtomicU64,   // Using empty entries
    pub replace_worst: StdAtomicU64,   // Replacing worst entries

    // Atomic operation statistics
    pub atomic_stores: StdAtomicU64, // Number of store operations
    pub atomic_loads: StdAtomicU64,  // Number of load operations

    // Prefetch statistics
    pub prefetch_count: StdAtomicU64, // Number of prefetch executions
    pub prefetch_hits: StdAtomicU64,  // Prefetch hit count

    // Optimization filters
    pub depth_filtered: StdAtomicU64, // Updates skipped due to depth filter
    pub hashfull_filtered: StdAtomicU64, // Updates skipped due to hashfull filter
    pub effective_updates: StdAtomicU64, // Updates that improved the entry
}

#[cfg(feature = "tt_metrics")]
impl DetailedTTMetrics {
    /// Create new metrics instance
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all metrics to zero
    pub fn reset(&self) {
        use std::sync::atomic::Ordering::Relaxed;

        self.cas_attempts.store(0, Relaxed);
        self.cas_successes.store(0, Relaxed);
        self.cas_failures.store(0, Relaxed);
        self.cas_key_match.store(0, Relaxed);
        self.update_existing.store(0, Relaxed);
        self.replace_empty.store(0, Relaxed);
        self.replace_worst.store(0, Relaxed);
        self.atomic_stores.store(0, Relaxed);
        self.atomic_loads.store(0, Relaxed);
        self.prefetch_count.store(0, Relaxed);
        self.prefetch_hits.store(0, Relaxed);
        self.depth_filtered.store(0, Relaxed);
        self.hashfull_filtered.store(0, Relaxed);
        self.effective_updates.store(0, Relaxed);
    }

    /// Print metrics summary
    pub fn print_summary(&self) {
        use std::sync::atomic::Ordering::Relaxed;

        let total_updates = self.update_existing.load(Relaxed)
            + self.replace_empty.load(Relaxed)
            + self.replace_worst.load(Relaxed);

        log::info!("=== TT Detailed Metrics ===");
        log::info!("Update patterns:");
        log::info!(
            "  Existing updates: {} ({:.1}%)",
            self.update_existing.load(Relaxed),
            self.update_existing.load(Relaxed) as f64 / total_updates as f64 * 100.0
        );
        log::info!(
            "  Empty slots used: {} ({:.1}%)",
            self.replace_empty.load(Relaxed),
            self.replace_empty.load(Relaxed) as f64 / total_updates as f64 * 100.0
        );
        log::info!(
            "  Worst replaced: {} ({:.1}%)",
            self.replace_worst.load(Relaxed),
            self.replace_worst.load(Relaxed) as f64 / total_updates as f64 * 100.0
        );

        log::info!("\nAtomic operations:");
        log::info!("  Stores: {}", self.atomic_stores.load(Relaxed));
        log::info!("  Loads: {}", self.atomic_loads.load(Relaxed));

        log::info!("\nPrefetch statistics:");
        log::info!("  Prefetch count: {}", self.prefetch_count.load(Relaxed));

        if self.cas_attempts.load(Relaxed) > 0 {
            log::info!("\nCAS operations:");
            log::info!("  Attempts: {}", self.cas_attempts.load(Relaxed));
            log::info!("  Successes: {}", self.cas_successes.load(Relaxed));
            log::info!("  Failures: {}", self.cas_failures.load(Relaxed));
            log::info!(
                "  Key matches: {} ({:.1}% of failures)",
                self.cas_key_match.load(Relaxed),
                if self.cas_failures.load(Relaxed) > 0 {
                    self.cas_key_match.load(Relaxed) as f64 / self.cas_failures.load(Relaxed) as f64
                        * 100.0
                } else {
                    0.0
                }
            );
        }

        let depth_filtered = self.depth_filtered.load(Relaxed);
        let hashfull_filtered = self.hashfull_filtered.load(Relaxed);
        if depth_filtered > 0 || hashfull_filtered > 0 {
            log::info!("\nOptimization filters:");
            log::info!("  Depth filtered: {depth_filtered}");
            log::info!("  Hashfull filtered: {hashfull_filtered}");
            log::info!("  Effective updates: {}", self.effective_updates.load(Relaxed));
        }
    }

    /// Build a human-readable summary string (multiline) without logging
    #[cfg(feature = "tt_metrics")]
    pub fn to_summary_string(&self) -> String {
        use std::sync::atomic::Ordering::Relaxed;

        let mut out = String::new();
        let total_updates = self.update_existing.load(Relaxed)
            + self.replace_empty.load(Relaxed)
            + self.replace_worst.load(Relaxed);

        out.push_str("TT metrics: update_patterns\n");
        if total_updates > 0 {
            let ex = self.update_existing.load(Relaxed) as f64 / total_updates as f64 * 100.0;
            let em = self.replace_empty.load(Relaxed) as f64 / total_updates as f64 * 100.0;
            let wr = self.replace_worst.load(Relaxed) as f64 / total_updates as f64 * 100.0;
            out.push_str(&format!(
                "  existing={} ({:.1}%)\n  empty={} ({:.1}%)\n  worst={} ({:.1}%)\n",
                self.update_existing.load(Relaxed),
                ex,
                self.replace_empty.load(Relaxed),
                em,
                self.replace_worst.load(Relaxed),
                wr,
            ));
        } else {
            out.push_str("  (no updates recorded)\n");
        }

        out.push_str("atomic_ops\n");
        out.push_str(&format!(
            "  loads={} stores={}\n",
            self.atomic_loads.load(Relaxed),
            self.atomic_stores.load(Relaxed)
        ));

        let cas_attempts = self.cas_attempts.load(Relaxed);
        if cas_attempts > 0 {
            let cas_failures = self.cas_failures.load(Relaxed);
            let key_match = self.cas_key_match.load(Relaxed);
            let key_match_rate = if cas_failures > 0 {
                key_match as f64 / cas_failures as f64 * 100.0
            } else {
                0.0
            };
            out.push_str("cas\n");
            out.push_str(&format!(
                "  attempts={} success={} failure={} key_match={:.1}%\n",
                cas_attempts,
                self.cas_successes.load(Relaxed),
                cas_failures,
                key_match_rate
            ));
        }

        let depth_filtered = self.depth_filtered.load(Relaxed);
        let hashfull_filtered = self.hashfull_filtered.load(Relaxed);
        if depth_filtered > 0 || hashfull_filtered > 0 {
            out.push_str("filters\n");
            out.push_str(&format!(
                "  depth_filtered={} hashfull_filtered={} effective_updates={}\n",
                depth_filtered,
                hashfull_filtered,
                self.effective_updates.load(Relaxed)
            ));
        }

        out
    }
}

/// Metrics update types
#[cfg(feature = "tt_metrics")]
#[derive(Debug, Clone, Copy)]
pub(crate) enum MetricType {
    AtomicLoad,
    AtomicStore(u32), // Parameter: number of stores
    DepthFiltered,
    UpdateExisting,
    EffectiveUpdate,
    CasAttempt,
    CasSuccess,
    CasFailure,
    ReplaceEmpty,
    ReplaceWorst,
}

/// Record metrics - cold path to minimize overhead
#[cfg(feature = "tt_metrics")]
#[cold]
#[inline(never)]
pub(crate) fn record_metric(metrics: &DetailedTTMetrics, metric_type: MetricType) {
    use std::sync::atomic::Ordering::Relaxed;
    match metric_type {
        MetricType::AtomicLoad => metrics.atomic_loads.fetch_add(1, Relaxed),
        MetricType::AtomicStore(n) => metrics.atomic_stores.fetch_add(n as u64, Relaxed),
        MetricType::DepthFiltered => metrics.depth_filtered.fetch_add(1, Relaxed),
        MetricType::UpdateExisting => metrics.update_existing.fetch_add(1, Relaxed),
        MetricType::EffectiveUpdate => metrics.effective_updates.fetch_add(1, Relaxed),
        MetricType::CasAttempt => metrics.cas_attempts.fetch_add(1, Relaxed),
        MetricType::CasSuccess => metrics.cas_successes.fetch_add(1, Relaxed),
        MetricType::CasFailure => metrics.cas_failures.fetch_add(1, Relaxed),
        MetricType::ReplaceEmpty => metrics.replace_empty.fetch_add(1, Relaxed),
        MetricType::ReplaceWorst => metrics.replace_worst.fetch_add(1, Relaxed),
    };
}
