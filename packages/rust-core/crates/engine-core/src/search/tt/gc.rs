//! Garbage collection methods for TranspositionTable

use super::constants::{AGE_MASK, GENERATION_CYCLE};
use super::*;
use std::sync::atomic::Ordering;

impl TranspositionTable {
    /// Check if bucket is empty (all entries have key == 0)
    fn is_bucket_empty(&self, bucket_idx: usize) -> bool {
        if let Some(ref flexible_buckets) = self.flexible_buckets {
            let bucket = &flexible_buckets[bucket_idx];
            let entries_per_bucket = bucket.size.entries();

            for i in 0..entries_per_bucket {
                let key_idx = i * 2;
                if bucket.entries[key_idx].load(Ordering::Relaxed) != 0 {
                    return false;
                }
            }
        } else {
            let bucket = &self.buckets[bucket_idx];

            for i in 0..BUCKET_SIZE {
                let key_idx = i * 2;
                if bucket.entries[key_idx].load(Ordering::Relaxed) != 0 {
                    return false;
                }
            }
        }

        true
    }

    /// Clear old entries in a single bucket
    fn clear_old_entries_in_bucket(&self, bucket_idx: usize) {
        let current_age = self.current_age();
        let threshold_age_distance = self.gc_threshold_age_distance;

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            let bucket = &flexible_buckets[bucket_idx];
            let entries_per_bucket = bucket.size.entries();

            for i in 0..entries_per_bucket {
                let key_idx = i * 2;
                let data_idx = key_idx + 1;

                // Load key first
                let key = bucket.entries[key_idx].load(Ordering::Acquire);
                if key == 0 {
                    continue; // Empty entry
                }

                // Load data
                let data = bucket.entries[data_idx].load(Ordering::Acquire);
                let entry = TTEntry { key, data };

                // Calculate age distance
                let entry_age = entry.age();
                let age_distance = ((GENERATION_CYCLE + current_age as u16 - entry_age as u16)
                    & (AGE_MASK as u16)) as u8;

                // Clear if too old
                if age_distance >= threshold_age_distance {
                    // Clear the entry atomically (data→key の順で公開)
                    bucket.entries[data_idx].store(0, Ordering::Release);
                    bucket.entries[key_idx].store(0, Ordering::Release);

                    #[cfg(feature = "tt_metrics")]
                    self.gc_entries_cleared.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        } else {
            let bucket = &self.buckets[bucket_idx];

            for i in 0..BUCKET_SIZE {
                let key_idx = i * 2;
                let data_idx = key_idx + 1;

                // Load key first
                let key = bucket.entries[key_idx].load(Ordering::Acquire);
                if key == 0 {
                    continue; // Empty entry
                }

                // Load data
                let data = bucket.entries[data_idx].load(Ordering::Acquire);
                let entry = TTEntry { key, data };

                // Calculate age distance
                let entry_age = entry.age();
                let age_distance = ((GENERATION_CYCLE + current_age as u16 - entry_age as u16)
                    & (AGE_MASK as u16)) as u8;

                // Clear if too old
                if age_distance >= threshold_age_distance {
                    // Clear the entry atomically (data→key の順で公開)
                    bucket.entries[data_idx].store(0, Ordering::Release);
                    bucket.entries[key_idx].store(0, Ordering::Release);

                    #[cfg(feature = "tt_metrics")]
                    self.gc_entries_cleared.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        // Update bitmap if bucket is now empty
        if self.is_bucket_empty(bucket_idx) {
            self.clear_bucket_occupied(bucket_idx);
        }
    }

    /// Perform incremental garbage collection
    /// Returns true if GC is complete
    pub fn perform_incremental_gc(&self, buckets_per_call: usize) -> bool {
        if !self.need_gc.load(Ordering::Relaxed) {
            return true; // GC not needed
        }

        let start_bucket =
            self.gc_progress.fetch_add(buckets_per_call as u64, Ordering::Relaxed) as usize;
        let end_bucket = (start_bucket + buckets_per_call).min(self.num_buckets);

        // Process buckets
        for bucket_idx in start_bucket..end_bucket {
            self.clear_old_entries_in_bucket(bucket_idx);
        }

        // Check if we've processed all buckets
        if end_bucket >= self.num_buckets {
            // GC complete - reset state
            self.gc_progress.store(0, Ordering::Relaxed);
            self.need_gc.store(false, Ordering::Relaxed);
            self.high_hashfull_counter.store(0, Ordering::Relaxed);

            // Update hashfull estimate after GC
            self.update_hashfull_estimate();

            return true;
        }

        false // GC still in progress
    }

    /// Check if GC should be triggered
    pub fn should_trigger_gc(&self) -> bool {
        self.need_gc.load(Ordering::Relaxed)
    }

    /// Trigger GC if needed based on hashfull and conditions
    pub fn trigger_gc_if_needed(&self) {
        let hf = self.hashfull_estimate();

        // Update high hashfull counter
        if hf >= 900 {
            self.high_hashfull_counter.fetch_add(1, Ordering::Relaxed);
        } else {
            self.high_hashfull_counter.store(0, Ordering::Relaxed);
        }

        // Trigger GC if table is getting full and we've been full for a while
        if hf >= 950 && self.high_hashfull_counter.load(Ordering::Relaxed) >= 10 {
            self.need_gc.store(true, Ordering::Relaxed);
            #[cfg(feature = "tt_metrics")]
            self.gc_triggered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Set the age distance threshold for GC
    pub fn set_gc_threshold(&mut self, threshold: u8) {
        self.gc_threshold_age_distance = threshold.min(AGE_MASK);
    }

    /// Get current GC progress (bucket index)
    pub fn gc_progress(&self) -> usize {
        self.gc_progress.load(Ordering::Relaxed) as usize
    }

    /// Get total number of entries cleared by GC
    #[cfg(feature = "tt_metrics")]
    pub fn gc_entries_cleared(&self) -> u64 {
        self.gc_entries_cleared.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get number of times GC was triggered
    #[cfg(feature = "tt_metrics")]
    pub fn gc_triggered_count(&self) -> u64 {
        self.gc_triggered.load(std::sync::atomic::Ordering::Relaxed)
    }
}
