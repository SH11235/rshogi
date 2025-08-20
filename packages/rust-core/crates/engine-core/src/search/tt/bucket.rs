//! Bucket implementations for transposition table

use super::bucket_simd;
use super::entry::TTEntry;
#[cfg(feature = "tt_metrics")]
use super::metrics::DetailedTTMetrics;
use super::prefetch::prefetch_memory;
use super::utils::{try_update_entry_generic, UpdateResult};
use crate::search::tt_simd::simd_enabled;
use crate::util::sync_compat::{AtomicU64, Ordering};

/// Number of entries per bucket (default for backward compatibility)
const BUCKET_SIZE: usize = 4;

/// Dynamic bucket size configuration
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BucketSize {
    /// 4 entries (64 bytes) - 1 cache line, optimal for small tables (≤8MB)
    Small = 4,
    /// 8 entries (128 bytes) - 2 cache lines, optimal for medium tables (9-32MB)
    Medium = 8,
    /// 16 entries (256 bytes) - 4 cache lines, optimal for large tables (>32MB)
    Large = 16,
}

impl BucketSize {
    /// Determine optimal bucket size based on table size
    pub fn optimal_for_size(table_size_mb: usize) -> Self {
        match table_size_mb {
            0..=8 => BucketSize::Small,
            9..=32 => BucketSize::Medium,
            _ => BucketSize::Large,
        }
    }

    /// Get number of entries in this bucket size
    pub fn entries(&self) -> usize {
        *self as usize
    }

    /// Get size in bytes for this bucket size
    pub fn bytes(&self) -> usize {
        self.entries() * 16 // Each entry is 16 bytes (key + data)
    }
}

/// Bucket containing multiple TT entries (64 bytes = 1 cache line)
#[repr(C, align(64))]
pub(crate) struct TTBucket {
    pub(crate) entries: [AtomicU64; BUCKET_SIZE * 2], // 4 entries * 2 u64s each = 64 bytes
}

impl TTBucket {
    /// Create new empty bucket
    pub(crate) fn new() -> Self {
        TTBucket {
            entries: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }

    /// Clear all entries in the bucket
    pub(crate) fn clear(&mut self) {
        for entry in self.entries.iter_mut() {
            *entry = AtomicU64::new(0);
        }
    }

    /// Probe bucket for matching entry using SIMD when available
    pub(crate) fn probe(&self, key: u64) -> Option<TTEntry> {
        // Try SIMD-optimized path first
        if self.probe_simd_available() {
            return self.probe_simd_impl(key);
        }

        // Fallback to scalar implementation
        self.probe_scalar(key)
    }

    /// Check if SIMD probe is available
    /// This is inlined and the feature detection is cached by the CPU
    #[inline(always)]
    fn probe_simd_available(&self) -> bool {
        // The is_x86_feature_detected! macro is already optimized:
        // It caches the result in a static variable after first call
        #[cfg(target_arch = "x86_64")]
        {
            std::is_x86_feature_detected!("avx2") || std::is_x86_feature_detected!("sse2")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    /// SIMD-optimized probe implementation
    fn probe_simd_impl(&self, target_key: u64) -> Option<TTEntry> {
        bucket_simd::probe_simd(&self.entries, target_key)
    }

    /// Scalar fallback probe implementation (hybrid: early termination + single fence)
    fn probe_scalar(&self, target_key: u64) -> Option<TTEntry> {
        // Hybrid approach: early termination to minimize memory access
        let mut matching_idx = None;

        // Load keys with early termination using Acquire ordering
        for i in 0..BUCKET_SIZE {
            let key = self.entries[i * 2].load(Ordering::Acquire);
            if key == target_key {
                matching_idx = Some(i);
                break; // Early termination - key optimization
            }
        }

        // If we found a match, load data with fence + Relaxed
        if let Some(idx) = matching_idx {
            // Use Acquire ordering on data load for synchronization
            let data = self.entries[idx * 2 + 1].load(Ordering::Acquire);
            let entry = TTEntry {
                key: target_key,
                data,
            };

            if entry.depth() > 0 {
                return Some(entry);
            }
        }

        None
    }

    /// Store entry in bucket with metrics tracking
    #[cfg(test)]
    pub(crate) fn store_with_metrics(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        self.store_with_metrics_and_mode(
            new_entry,
            current_age,
            false, // empty_slot_mode = false
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        );
    }

    /// Store entry in bucket with explicit empty_slot_mode control
    pub(crate) fn store_with_mode(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        self.store_with_metrics_and_mode(
            new_entry,
            current_age,
            empty_slot_mode,
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        );
    }

    /// Store entry in bucket with metrics tracking and mode
    fn store_with_metrics_and_mode(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        #[cfg(feature = "tt_metrics")]
        self.store_internal(new_entry, current_age, empty_slot_mode, metrics);
        #[cfg(not(feature = "tt_metrics"))]
        self.store_internal(new_entry, current_age, empty_slot_mode, None)
    }

    /// Store entry in bucket (used in tests)
    #[cfg(test)]
    pub(crate) fn store(&self, new_entry: TTEntry, current_age: u8) {
        self.store_internal(new_entry, current_age, false, None)
    }

    /// Try to update an existing entry with depth filtering
    #[inline]
    fn try_update_entry(
        &self,
        idx: usize,
        old_key: u64,
        new_entry: &TTEntry,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) -> UpdateResult {
        #[cfg(feature = "tt_metrics")]
        let result = try_update_entry_generic(&self.entries, idx, old_key, new_entry, metrics);
        #[cfg(not(feature = "tt_metrics"))]
        let result = try_update_entry_generic(&self.entries, idx, old_key, new_entry, None);
        result
    }

    /// Internal store implementation with optional metrics
    fn store_internal(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        let _target_key = new_entry.key;

        // First pass: look for exact match or empty slot
        for i in 0..BUCKET_SIZE {
            let idx = i * 2;

            // We no longer use CAS for empty slots - direct store with proper ordering
            {
                // Use Relaxed for empty slot detection
                let old_key = self.entries[idx].load(Ordering::Relaxed);

                // Record atomic load
                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    m.atomic_loads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }

                // Try to update existing entry
                #[cfg(feature = "tt_metrics")]
                let update_result = self.try_update_entry(idx, old_key, &new_entry, metrics);
                #[cfg(not(feature = "tt_metrics"))]
                let update_result = self.try_update_entry(idx, old_key, &new_entry, None);
                match update_result {
                    UpdateResult::Updated | UpdateResult::Filtered => return,
                    UpdateResult::NotFound => {} // Continue to next check
                }

                if old_key == 0 {
                    // Empty slot - use store ordering to ensure data visibility
                    // Write data first with Release ordering
                    self.entries[idx + 1].store(new_entry.data, Ordering::Release);

                    // Then publish key with Release ordering to ensure data is visible
                    self.entries[idx].store(new_entry.key, Ordering::Release);

                    // Record metrics
                    #[cfg(feature = "tt_metrics")]
                    if let Some(m) = metrics {
                        m.atomic_stores.fetch_add(2, std::sync::atomic::Ordering::Relaxed);
                        m.replace_empty.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    return;
                } else {
                    // Slot is occupied by different position, try next
                    continue;
                }
            }
        }

        // If empty slot mode is enabled, skip replacement
        if empty_slot_mode {
            return;
        }

        // Second pass: find least valuable entry to replace using SIMD if available
        let (worst_idx, worst_score) = if self.store_simd_available() {
            self.find_worst_entry_simd(current_age)
        } else {
            self.find_worst_entry_scalar(current_age)
        };

        // Check if new entry is more valuable than the worst existing entry
        if new_entry.priority_score(current_age) > worst_score {
            let idx = worst_idx * 2;

            // Use CAS to ensure atomic replacement
            // Note: We don't retry here as we've already determined this is the best slot to replace
            let old_key = self.entries[idx].load(Ordering::Relaxed);

            // Record CAS attempt
            #[cfg(feature = "tt_metrics")]
            if let Some(m) = metrics {
                use super::metrics::{record_metric, MetricType};
                record_metric(m, MetricType::CasAttempt);
            }

            // Attempt atomic update of the key
            match self.entries[idx].compare_exchange(
                old_key,
                new_entry.key,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // CAS succeeded - write data with Release
                    // This ensures readers see the complete entry
                    self.entries[idx + 1].store(new_entry.data, Ordering::Release);

                    // Record metrics
                    #[cfg(feature = "tt_metrics")]
                    if let Some(m) = metrics {
                        use super::metrics::{record_metric, MetricType};
                        record_metric(m, MetricType::CasSuccess);
                        record_metric(m, MetricType::AtomicStore(2));
                        record_metric(m, MetricType::ReplaceWorst);
                    }
                }
                Err(current) => {
                    // Phase 5 optimization: Check if another thread wrote the same key
                    if current == new_entry.key {
                        // Same key - just update the data
                        // Use Release ordering to ensure reader sees the updated data
                        self.entries[idx + 1].store(new_entry.data, Ordering::Release);

                        #[cfg(feature = "tt_metrics")]
                        if let Some(m) = metrics {
                            m.cas_key_match.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            m.update_existing.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            m.atomic_stores.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    } else {
                        // CAS failed with different key
                        #[cfg(feature = "tt_metrics")]
                        if let Some(m) = metrics {
                            use super::metrics::{record_metric, MetricType};
                            record_metric(m, MetricType::CasFailure);
                        }
                    }
                    // If CAS failed, another thread updated this entry - we accept this race
                    // as it's not critical (both threads are storing valid entries)
                }
            }
        }
    }

    /// Check if SIMD store optimization is available
    #[inline]
    fn store_simd_available(&self) -> bool {
        simd_enabled()
    }

    /// Find worst entry using SIMD priority calculation
    fn find_worst_entry_simd(&self, current_age: u8) -> (usize, i32) {
        bucket_simd::find_worst_entry_simd(&self.entries, current_age)
    }

    /// Find worst entry using scalar priority calculation
    fn find_worst_entry_scalar(&self, current_age: u8) -> (usize, i32) {
        let mut worst_idx = 0;
        let mut worst_score = i32::MAX;

        for i in 0..BUCKET_SIZE {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Acquire);

            let score = if key == 0 {
                i32::MIN // Empty entries have lowest priority
            } else {
                // Use Relaxed for data since Acquire on key already synchronized
                let data = self.entries[idx + 1].load(Ordering::Relaxed);
                let entry = TTEntry { key, data };
                entry.priority_score(current_age)
            };

            if score < worst_score {
                worst_score = score;
                worst_idx = i;
            }
        }

        (worst_idx, worst_score)
    }

    /// Prefetch bucket into cache
    pub(crate) fn prefetch(&self, hint: i32) {
        let addr = self.entries.as_ptr() as *const u8;
        prefetch_memory(addr, hint);
    }
}
