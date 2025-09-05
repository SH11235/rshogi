//! Flexible bucket implementation for transposition table

use super::bucket::BucketSize;
use super::entry::TTEntry;
#[cfg(feature = "tt_metrics")]
use super::metrics::{record_metric, DetailedTTMetrics, MetricType};
use super::utils::{try_update_entry_generic, UpdateResult};
use crate::search::tt::simd::simd_enabled;
use crate::search::NodeType;
use std::sync::atomic::{AtomicU64, Ordering};

/// Flexible bucket that can hold variable number of entries
/// Note: For optimal performance, consider using fixed-size TTBucket when possible
/// as it guarantees cache line alignment
pub(crate) struct FlexibleTTBucket {
    /// Atomic entries (keys and data interleaved)
    pub(crate) entries: Box<[AtomicU64]>,
    /// Size configuration for this bucket
    pub(crate) size: BucketSize,
}

impl FlexibleTTBucket {
    /// Create new flexible bucket with specified size
    pub(crate) fn new(size: BucketSize) -> Self {
        let entry_count = size.entries() * 2; // key + data for each entry
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            entries.push(AtomicU64::new(0));
        }

        FlexibleTTBucket {
            entries: entries.into_boxed_slice(),
            size,
        }
    }

    /// Clear all entries in the bucket
    pub(crate) fn clear(&mut self) {
        for entry in self.entries.iter() {
            entry.store(0, Ordering::Relaxed);
        }
    }

    /// Probe bucket for matching entry
    pub(crate) fn probe(&self, key: u64) -> Option<TTEntry> {
        match self.size {
            BucketSize::Small => self.probe_small(key),
            BucketSize::Medium => self.probe_medium(key),
            BucketSize::Large => self.probe_large(key),
        }
    }

    /// Optimized probe for small buckets (4 entries)
    fn probe_small(&self, target_key: u64) -> Option<TTEntry> {
        // Fixed-size for better optimization
        for i in 0..4 {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Acquire);
            if key == target_key {
                // Use Relaxed for data since Acquire on key already synchronized
                let data = self.entries[idx + 1].load(Ordering::Relaxed);
                let entry = TTEntry { key, data };
                if entry.depth() > 0 {
                    return Some(entry);
                }
            }
        }
        None
    }

    /// Optimized probe for medium buckets (8 entries)
    fn probe_medium(&self, target_key: u64) -> Option<TTEntry> {
        // Try SIMD if available for 8 entries
        if simd_enabled() {
            // Load all 8 keys
            let mut keys = [0u64; 8];
            for (i, key) in keys.iter_mut().enumerate() {
                *key = self.entries[i * 2].load(Ordering::Acquire);
            }

            if let Some(idx) = crate::search::tt::simd::find_matching_key_8(&keys, target_key) {
                // Use Relaxed for data since Acquire on key already synchronized
                let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
                let entry = TTEntry {
                    key: keys[idx],
                    data,
                };
                if entry.depth() > 0 {
                    return Some(entry);
                }
            }
        } else {
            // Scalar fallback
            for i in 0..8 {
                let idx = i * 2;
                let key = self.entries[idx].load(Ordering::Acquire);
                if key == target_key {
                    // Use Relaxed for data since Acquire on key already synchronized
                    let data = self.entries[idx + 1].load(Ordering::Relaxed);
                    let entry = TTEntry { key, data };
                    if entry.depth() > 0 {
                        return Some(entry);
                    }
                }
            }
        }
        None
    }

    /// Optimized probe for large buckets (16 entries)
    fn probe_large(&self, target_key: u64) -> Option<TTEntry> {
        // Try SIMD if available for 16 entries
        if simd_enabled() {
            // Load all 16 keys
            let mut keys = [0u64; 16];
            for (i, key) in keys.iter_mut().enumerate() {
                *key = self.entries[i * 2].load(Ordering::Acquire);
            }

            if let Some(idx) = crate::search::tt::simd::find_matching_key_16(&keys, target_key) {
                // Use Relaxed for data since Acquire on key already synchronized
                let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
                let entry = TTEntry {
                    key: keys[idx],
                    data,
                };
                if entry.depth() > 0 {
                    return Some(entry);
                }
            }
        } else {
            // Scalar fallback with early termination
            for i in 0..16 {
                let idx = i * 2;
                let key = self.entries[idx].load(Ordering::Acquire);
                if key == target_key {
                    // Use Relaxed for data since Acquire on key already synchronized
                    let data = self.entries[idx + 1].load(Ordering::Relaxed);
                    let entry = TTEntry { key, data };
                    if entry.depth() > 0 {
                        return Some(entry);
                    }
                    break; // Early termination
                }
            }
        }
        None
    }

    /// Store entry with metrics tracking
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
            false,
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        );
    }

    /// Store entry with metrics tracking and explicit empty_slot_mode
    pub(crate) fn store_with_metrics_and_mode(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        match self.size {
            BucketSize::Small => self.store_small(
                new_entry,
                current_age,
                empty_slot_mode,
                #[cfg(feature = "tt_metrics")]
                metrics,
                #[cfg(not(feature = "tt_metrics"))]
                None,
            ),
            BucketSize::Medium => self.store_medium(
                new_entry,
                current_age,
                empty_slot_mode,
                #[cfg(feature = "tt_metrics")]
                metrics,
                #[cfg(not(feature = "tt_metrics"))]
                None,
            ),
            BucketSize::Large => self.store_large(
                new_entry,
                current_age,
                empty_slot_mode,
                #[cfg(feature = "tt_metrics")]
                metrics,
                #[cfg(not(feature = "tt_metrics"))]
                None,
            ),
        }
    }

    /// Store implementation for small buckets
    fn store_small(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        // Same logic as TTBucket but with size 4
        self.store_generic(
            new_entry,
            current_age,
            4,
            empty_slot_mode,
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        );
    }

    /// Store implementation for medium buckets
    fn store_medium(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        self.store_generic(
            new_entry,
            current_age,
            8,
            empty_slot_mode,
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        );
    }

    /// Store implementation for large buckets
    fn store_large(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        self.store_generic(
            new_entry,
            current_age,
            16,
            empty_slot_mode,
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        );
    }

    fn store_generic(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        entries: usize,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        // First pass: look for exact match or empty slot
        for i in 0..entries {
            let idx = i * 2;
            let old_key = self.entries[idx].load(Ordering::Relaxed);

            #[cfg(feature = "tt_metrics")]
            if let Some(m) = metrics {
                record_metric(m, MetricType::AtomicLoad);
            }

            // Try to update existing entry
            let update_result = try_update_entry_generic(
                &self.entries,
                idx,
                old_key,
                &new_entry,
                #[cfg(feature = "tt_metrics")]
                metrics,
                #[cfg(not(feature = "tt_metrics"))]
                None,
            );

            match update_result {
                UpdateResult::Updated | UpdateResult::Filtered => return,
                UpdateResult::NotFound => {}
            }

            if old_key == 0 {
                // Empty slot - direct store
                self.entries[idx + 1].store(new_entry.data, Ordering::Release);
                self.entries[idx].store(new_entry.key, Ordering::Release);

                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    record_metric(m, MetricType::ReplaceEmpty);
                    record_metric(m, MetricType::AtomicStore(2));
                }
                return;
            }
        }

        // If empty slot mode is enabled, skip replacement
        if empty_slot_mode {
            return;
        }

        // Second pass: find worst entry to replace
        let (worst_idx, worst_score) = match self.size {
            BucketSize::Large => self.find_worst_entry_16(current_age),
            BucketSize::Medium => self.find_worst_entry_8(current_age),
            BucketSize::Small => self.find_worst_entry_n(current_age, 4),
        };

        // Replace if new entry is better
        if new_entry.priority_score(current_age) > worst_score {
            let idx = worst_idx * 2;
            self.entries[idx + 1].store(new_entry.data, Ordering::Release);
            self.entries[idx].store(new_entry.key, Ordering::Release);

            #[cfg(feature = "tt_metrics")]
            if let Some(m) = metrics {
                record_metric(m, MetricType::ReplaceWorst);
                record_metric(m, MetricType::AtomicStore(2));
            }
        }
    }

    /// Find worst entry for 16-entry buckets
    fn find_worst_entry_16(&self, current_age: u8) -> (usize, i32) {
        if simd_enabled() {
            self.find_worst_entry_simd_16(current_age)
        } else {
            self.find_worst_entry_n(current_age, 16)
        }
    }

    /// Find worst entry for 8-entry buckets
    fn find_worst_entry_8(&self, current_age: u8) -> (usize, i32) {
        if simd_enabled() {
            self.find_worst_entry_simd_8(current_age)
        } else {
            self.find_worst_entry_n(current_age, 8)
        }
    }

    /// SIMD implementation for finding worst entry in 16-entry bucket
    fn find_worst_entry_simd_16(&self, current_age: u8) -> (usize, i32) {
        use crate::search::tt::simd;

        // Prepare data arrays
        let mut depths = [0u8; 16];
        let mut ages = [0u8; 16];
        let mut is_pv = [false; 16];
        let mut is_exact = [false; 16];

        // Load all entries
        for i in 0..16 {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Acquire);
            if key == 0 {
                depths[i] = 0;
                ages[i] = 0;
            } else {
                let data = self.entries[idx + 1].load(Ordering::Relaxed);
                let entry = TTEntry { key, data };
                depths[i] = entry.depth();
                ages[i] = entry.age();
                is_pv[i] = entry.is_pv();
                is_exact[i] = entry.node_type() == NodeType::Exact;
            }
        }

        // Calculate scores with SIMD - for now just calculate twice and combine
        let scores1 = simd::calculate_priority_scores_8(
            &[
                depths[0], depths[1], depths[2], depths[3], depths[4], depths[5], depths[6],
                depths[7],
            ],
            &[
                ages[0], ages[1], ages[2], ages[3], ages[4], ages[5], ages[6], ages[7],
            ],
            &[
                is_pv[0], is_pv[1], is_pv[2], is_pv[3], is_pv[4], is_pv[5], is_pv[6], is_pv[7],
            ],
            &[
                is_exact[0],
                is_exact[1],
                is_exact[2],
                is_exact[3],
                is_exact[4],
                is_exact[5],
                is_exact[6],
                is_exact[7],
            ],
            current_age,
        );
        let scores2 = simd::calculate_priority_scores_8(
            &[
                depths[8], depths[9], depths[10], depths[11], depths[12], depths[13], depths[14],
                depths[15],
            ],
            &[
                ages[8], ages[9], ages[10], ages[11], ages[12], ages[13], ages[14], ages[15],
            ],
            &[
                is_pv[8], is_pv[9], is_pv[10], is_pv[11], is_pv[12], is_pv[13], is_pv[14],
                is_pv[15],
            ],
            &[
                is_exact[8],
                is_exact[9],
                is_exact[10],
                is_exact[11],
                is_exact[12],
                is_exact[13],
                is_exact[14],
                is_exact[15],
            ],
            current_age,
        );

        let mut scores = [0i32; 16];
        scores[0..8].copy_from_slice(&scores1);
        scores[8..16].copy_from_slice(&scores2);

        // Find minimum
        let mut worst_idx = 0;
        let mut worst_score = scores[0];
        for (i, &score) in scores.iter().enumerate().skip(1) {
            if score < worst_score {
                worst_score = score;
                worst_idx = i;
            }
        }

        (worst_idx, worst_score)
    }

    /// SIMD implementation for finding worst entry in 8-entry bucket
    fn find_worst_entry_simd_8(&self, current_age: u8) -> (usize, i32) {
        use crate::search::tt::simd;

        // Prepare data arrays
        let mut depths = [0u8; 8];
        let mut ages = [0u8; 8];
        let mut is_pv = [false; 8];
        let mut is_exact = [false; 8];

        // Load all entries
        for i in 0..8 {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Acquire);
            if key == 0 {
                depths[i] = 0;
                ages[i] = 0;
            } else {
                let data = self.entries[idx + 1].load(Ordering::Relaxed);
                let entry = TTEntry { key, data };
                depths[i] = entry.depth();
                ages[i] = entry.age();
                is_pv[i] = entry.is_pv();
                is_exact[i] = entry.node_type() == NodeType::Exact;
            }
        }

        // Calculate scores with SIMD
        let scores =
            simd::calculate_priority_scores_8(&depths, &ages, &is_pv, &is_exact, current_age);

        // Find minimum
        let mut worst_idx = 0;
        let mut worst_score = scores[0];
        for (i, &score) in scores.iter().enumerate().skip(1) {
            if score < worst_score {
                worst_score = score;
                worst_idx = i;
            }
        }

        (worst_idx, worst_score)
    }

    /// Generic scalar implementation for finding worst entry
    fn find_worst_entry_n(&self, current_age: u8, entries: usize) -> (usize, i32) {
        let mut worst_idx = 0;
        let mut worst_score = i32::MAX;

        for i in 0..entries {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Acquire);

            let score = if key == 0 {
                i32::MIN // Empty entries have lowest priority
            } else {
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
    #[inline(always)]
    pub(crate) fn prefetch(&self, hint: i32) {
        use super::prefetch::prefetch_multiple;
        use core::mem::size_of;

        let addr = self.entries.as_ptr() as *const u8;

        // Calculate size in bytes and round up to cache lines
        let bytes = self.entries.len() * size_of::<AtomicU64>();
        let cache_lines = bytes.div_ceil(64); // Round up to next cache line

        prefetch_multiple(addr, cache_lines, hint);
    }
}
