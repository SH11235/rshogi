//! Bucket implementations for transposition table

use super::entry::{NodeType, TTEntry, AGE_MASK, GENERATION_CYCLE};
#[cfg(feature = "tt_metrics")]
use super::metrics::{record_metric, DetailedTTMetrics, MetricType};
use super::utils::{try_update_entry_generic, UpdateResult};
use crate::search::tt_simd::{simd_enabled, simd_kind, SimdKind};
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
        // Load all 4 keys at once for SIMD comparison
        // Use Acquire ordering for key loads to ensure proper synchronization with Release stores
        let mut keys = [0u64; BUCKET_SIZE];
        for (i, key) in keys.iter_mut().enumerate() {
            *key = self.entries[i * 2].load(Ordering::Acquire);
        }

        // Use SIMD to find matching key
        if let Some(idx) = crate::search::tt_simd::simd::find_matching_key(&keys, target_key) {
            // Use Acquire ordering on data load for synchronization
            let data = self.entries[idx * 2 + 1].load(Ordering::Acquire);
            let entry = TTEntry {
                key: keys[idx],
                data,
            };

            if entry.depth() > 0 {
                return Some(entry);
            }
        }

        None
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
                // Use Relaxed for speculative read in CAS loop
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
                } else if old_key != 0 {
                    // Slot is occupied by different position, try next
                    break;
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
                m.cas_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                        m.cas_successes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        m.atomic_stores.fetch_add(2, std::sync::atomic::Ordering::Relaxed);
                        m.replace_worst.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                            m.cas_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

    /// Get SIMD kind for choosing optimal implementation
    #[inline]
    #[allow(dead_code)]
    fn store_simd_kind(&self) -> SimdKind {
        simd_kind()
    }

    /// Find worst entry using SIMD priority calculation
    fn find_worst_entry_simd(&self, current_age: u8) -> (usize, i32) {
        // Prepare data for SIMD priority calculation
        let mut depths = [0u8; BUCKET_SIZE];
        let mut ages = [0u8; BUCKET_SIZE];
        let mut is_pv = [false; BUCKET_SIZE];
        let mut is_exact = [false; BUCKET_SIZE];
        let mut is_empty = [false; BUCKET_SIZE];

        // Load all entries at once
        // Use Acquire ordering on key load to ensure we see consistent data
        for i in 0..BUCKET_SIZE {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Acquire);
            if key == 0 {
                // Mark empty slots
                is_empty[i] = true;
                depths[i] = 0;
                ages[i] = 0;
                is_pv[i] = false;
                is_exact[i] = false;
            } else {
                // Use Relaxed for data since Acquire on key already synchronized
                let data = self.entries[idx + 1].load(Ordering::Relaxed);
                let entry = TTEntry { key, data };
                depths[i] = entry.depth();
                ages[i] = entry.age();
                is_pv[i] = entry.is_pv();
                is_exact[i] = entry.node_type() == NodeType::Exact;
            }
        }

        // Calculate all priority scores using SIMD
        let mut scores = crate::search::tt_simd::simd::calculate_priority_scores(
            &depths,
            &ages,
            &is_pv,
            &is_exact,
            current_age,
        );

        // Set empty entries to minimum priority (they should be replaced first)
        for (i, empty) in is_empty.iter().enumerate() {
            if *empty {
                scores[i] = i32::MIN;
            }
        }

        // Find minimum score and its index
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
        #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
        unsafe {
            use std::arch::x86_64::{
                _mm_prefetch, _MM_HINT_NTA, _MM_HINT_T0, _MM_HINT_T1, _MM_HINT_T2,
            };

            let addr = self.entries.as_ptr() as *const i8;

            match hint {
                0 => _mm_prefetch(addr, _MM_HINT_NTA), // Non-temporal
                1 => _mm_prefetch(addr, _MM_HINT_T2),  // L3
                2 => _mm_prefetch(addr, _MM_HINT_T1),  // L2
                3 => _mm_prefetch(addr, _MM_HINT_T0),  // L1
                _ => {}                                // Invalid hint, do nothing
            }
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
        {
            // No prefetch available on this architecture
            let _ = hint;
        }
    }
}

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
        for entry in self.entries.iter_mut() {
            *entry = AtomicU64::new(0);
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
                let data = self.entries[idx + 1].load(Ordering::Acquire);
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

            if let Some(idx) = crate::search::tt_simd::simd::find_matching_key_8(&keys, target_key)
            {
                let data = self.entries[idx * 2 + 1].load(Ordering::Acquire);
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
                    let data = self.entries[idx + 1].load(Ordering::Acquire);
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

            if let Some(idx) = crate::search::tt_simd::simd::find_matching_key_16(&keys, target_key)
            {
                let data = self.entries[idx * 2 + 1].load(Ordering::Acquire);
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
                    let data = self.entries[idx + 1].load(Ordering::Acquire);
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
    pub(crate) fn store_with_metrics(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        match self.size {
            BucketSize::Small => self.store_small(
                new_entry,
                current_age,
                #[cfg(feature = "tt_metrics")]
                metrics,
                #[cfg(not(feature = "tt_metrics"))]
                None,
            ),
            BucketSize::Medium => self.store_medium(
                new_entry,
                current_age,
                #[cfg(feature = "tt_metrics")]
                metrics,
                #[cfg(not(feature = "tt_metrics"))]
                None,
            ),
            BucketSize::Large => self.store_large(
                new_entry,
                current_age,
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
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        // Same logic as TTBucket but with size 4
        self.store_generic(
            new_entry,
            current_age,
            4,
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        )
    }

    /// Store implementation for medium buckets
    fn store_medium(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        self.store_generic(
            new_entry,
            current_age,
            8,
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        )
    }

    /// Store implementation for large buckets
    fn store_large(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        self.store_generic(
            new_entry,
            current_age,
            16,
            #[cfg(feature = "tt_metrics")]
            metrics,
            #[cfg(not(feature = "tt_metrics"))]
            None,
        )
    }

    /// Generic store implementation
    fn store_generic(
        &self,
        new_entry: TTEntry,
        current_age: u8,
        entries: usize,
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

        // Second pass: find worst entry to replace
        let (worst_idx, worst_score) = match entries {
            16 => self.find_worst_entry_16(current_age),
            8 => self.find_worst_entry_8(current_age),
            _ => self.find_worst_entry_n(current_age, entries),
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
        use crate::search::tt_simd::simd;

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
        use crate::search::tt_simd::simd;

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
    pub(crate) fn prefetch(&self, hint: i32) {
        #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
        unsafe {
            use std::arch::x86_64::{
                _mm_prefetch, _MM_HINT_NTA, _MM_HINT_T0, _MM_HINT_T1, _MM_HINT_T2,
            };

            let addr = self.entries.as_ptr() as *const i8;

            match hint {
                0 => _mm_prefetch(addr, _MM_HINT_NTA), // Non-temporal
                1 => _mm_prefetch(addr, _MM_HINT_T2),  // L3
                2 => _mm_prefetch(addr, _MM_HINT_T1),  // L2
                3 => _mm_prefetch(addr, _MM_HINT_T0),  // L1
                _ => {}                                // Invalid hint, do nothing
            }

            // For large buckets, prefetch additional cache lines
            if self.size == BucketSize::Large {
                let addr2 = addr.add(64);
                let addr3 = addr.add(128);
                let addr4 = addr.add(192);

                match hint {
                    0 => {
                        _mm_prefetch(addr2, _MM_HINT_NTA);
                        _mm_prefetch(addr3, _MM_HINT_NTA);
                        _mm_prefetch(addr4, _MM_HINT_NTA);
                    }
                    1 => {
                        _mm_prefetch(addr2, _MM_HINT_T2);
                        _mm_prefetch(addr3, _MM_HINT_T2);
                        _mm_prefetch(addr4, _MM_HINT_T2);
                    }
                    2 => {
                        _mm_prefetch(addr2, _MM_HINT_T1);
                        _mm_prefetch(addr3, _MM_HINT_T1);
                        _mm_prefetch(addr4, _MM_HINT_T1);
                    }
                    3 => {
                        _mm_prefetch(addr2, _MM_HINT_T0);
                        _mm_prefetch(addr3, _MM_HINT_T0);
                        _mm_prefetch(addr4, _MM_HINT_T0);
                    }
                    _ => {}
                }
            } else if self.size == BucketSize::Medium {
                // Medium buckets need 2 cache lines
                let addr2 = addr.add(64);

                match hint {
                    0 => _mm_prefetch(addr2, _MM_HINT_NTA),
                    1 => _mm_prefetch(addr2, _MM_HINT_T2),
                    2 => _mm_prefetch(addr2, _MM_HINT_T1),
                    3 => _mm_prefetch(addr2, _MM_HINT_T0),
                    _ => {}
                }
            }
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
        {
            // No prefetch available on this architecture
            let _ = hint;
        }
    }
}

// Extension methods for TTEntry to calculate priority scores
impl TTEntry {
    /// Calculate priority score for replacement decision
    pub(crate) fn priority_score(&self, current_age: u8) -> i32 {
        // Calculate cyclic age distance (Apery-style)
        let age_distance = ((GENERATION_CYCLE + current_age as u16 - self.age() as u16)
            & (AGE_MASK as u16)) as i32;

        // Base priority: depth minus age distance
        let mut priority = self.depth() as i32 - age_distance;

        // Bonus for PV nodes
        if self.is_pv() {
            priority += 32;
        }

        // Bonus for exact entries
        if self.node_type() == NodeType::Exact {
            priority += 16;
        }

        priority
    }
}
