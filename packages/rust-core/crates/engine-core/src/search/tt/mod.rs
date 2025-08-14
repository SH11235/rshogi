//! Optimized transposition table with bucket structure
//!
//! This implementation uses a bucket structure to optimize cache performance:
//! - 4 entries per bucket (64 bytes = 1 cache line)
//! - Improved replacement strategy within buckets
//! - Better memory locality

pub mod bucket;
pub mod entry;
pub mod gc;
pub mod metrics;
pub mod prefetch;
pub mod utils;

#[cfg(test)]
mod tests;

use crate::shogi::Move;
use crate::util;
use bucket::{FlexibleTTBucket, TTBucket};
use prefetch::AdaptivePrefetcher;
#[cfg(feature = "tt_metrics")]
use std::sync::atomic::AtomicU64 as StdAtomicU64;
use util::sync_compat::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};
use utils::*;

// No need to import entry module since it's already defined

// Re-export main types for backward compatibility
pub use bucket::BucketSize;
pub use entry::{NodeType, TTEntry, TTEntryParams, AGE_MASK, GENERATION_CYCLE};
#[cfg(feature = "tt_metrics")]
pub use metrics::DetailedTTMetrics;

// Re-export SIMD types
pub use crate::search::tt_simd::{simd_enabled, simd_kind, SimdKind};

/// Number of entries per bucket (default for backward compatibility)
const BUCKET_SIZE: usize = 4;

/// Transposition table implementation
pub struct TranspositionTable {
    /// Buckets for the transposition table (legacy fixed-size)
    buckets: Vec<TTBucket>,
    /// Flexible buckets (new dynamic-size)
    flexible_buckets: Option<Vec<FlexibleTTBucket>>,
    /// Number of buckets (always power of 2)
    num_buckets: usize,
    /// Current age (generation counter)
    age: u8,
    /// Bucket size configuration
    #[allow(dead_code)]
    bucket_size: Option<BucketSize>,
    /// Adaptive prefetcher
    prefetcher: Option<AdaptivePrefetcher>,
    /// TT performance metrics
    #[cfg(feature = "tt_metrics")]
    metrics: Option<DetailedTTMetrics>,
    /// Bitmap for occupied buckets (1 bit per bucket)
    occupied_bitmap: Vec<AtomicU8>,
    /// Hashfull estimate (updated periodically)
    hashfull_estimate: AtomicU16,
    /// Node counter for periodic updates
    node_counter: AtomicU64,
    /// GC flag - set when table is nearly full
    need_gc: AtomicBool,
    /// GC progress - next bucket to process
    gc_progress: AtomicU64,
    /// Age distance threshold for GC (entries with age_distance >= this are cleared)
    gc_threshold_age_distance: u8,
    /// Counter for consecutive high hashfull states
    high_hashfull_counter: AtomicU16,
    /// GC metrics
    #[cfg(feature = "tt_metrics")]
    gc_triggered: StdAtomicU64,
    #[cfg(feature = "tt_metrics")]
    gc_entries_cleared: StdAtomicU64,
    /// Empty slot mode control
    empty_slot_mode_enabled: AtomicBool,
    /// Last hashfull for hysteresis control
    empty_slot_mode_last_hf: AtomicU16,
}

impl TranspositionTable {
    /// Create new transposition table with given size in MB (backward compatible)
    pub fn new(size_mb: usize) -> Self {
        // Use legacy implementation for backward compatibility
        // Each bucket is 64 bytes
        let bucket_size = std::mem::size_of::<TTBucket>();
        debug_assert_eq!(bucket_size, 64);

        let num_buckets = if size_mb == 0 {
            // Minimum size: 64KB = 1024 buckets
            1024
        } else {
            (size_mb * 1024 * 1024) / bucket_size
        };

        // Round to power of 2 for fast indexing
        let num_buckets = num_buckets.next_power_of_two();

        // Allocate buckets
        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            buckets.push(TTBucket::new());
        }

        // Initialize occupied bitmap - 1 bit per bucket
        let bitmap_size = num_buckets.div_ceil(8); // Round up to nearest byte
        let occupied_bitmap = (0..bitmap_size).map(|_| AtomicU8::new(0)).collect();

        TranspositionTable {
            buckets,
            flexible_buckets: None,
            num_buckets,
            age: 0,
            bucket_size: None,
            prefetcher: None,
            #[cfg(feature = "tt_metrics")]
            metrics: None,
            occupied_bitmap,
            hashfull_estimate: AtomicU16::new(0),
            node_counter: AtomicU64::new(0),
            need_gc: AtomicBool::new(false),
            gc_progress: AtomicU64::new(0),
            gc_threshold_age_distance: 4, // Default: clear entries with age distance >= 4
            high_hashfull_counter: AtomicU16::new(0),
            #[cfg(feature = "tt_metrics")]
            gc_triggered: StdAtomicU64::new(0),
            #[cfg(feature = "tt_metrics")]
            gc_entries_cleared: StdAtomicU64::new(0),
            empty_slot_mode_enabled: AtomicBool::new(false),
            empty_slot_mode_last_hf: AtomicU16::new(0),
        }
    }

    /// Create new transposition table with dynamic bucket sizing
    pub fn new_with_config(size_mb: usize, bucket_size: Option<BucketSize>) -> Self {
        let bucket_size = bucket_size.unwrap_or_else(|| BucketSize::optimal_for_size(size_mb));
        let bytes_per_bucket = bucket_size.bytes();

        let num_buckets = if size_mb == 0 {
            // Minimum size depends on bucket size
            match bucket_size {
                BucketSize::Small => 1024, // 64KB minimum
                BucketSize::Medium => 512, // 64KB minimum
                BucketSize::Large => 256,  // 64KB minimum
            }
        } else {
            (size_mb * 1024 * 1024) / bytes_per_bucket
        };

        // Round to power of 2 for fast indexing
        let num_buckets = num_buckets.next_power_of_two();

        // Allocate flexible buckets
        let mut flexible_buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            flexible_buckets.push(FlexibleTTBucket::new(bucket_size));
        }

        // Initialize occupied bitmap - 1 bit per bucket
        let bitmap_size = num_buckets.div_ceil(8); // Round up to nearest byte
        let occupied_bitmap = (0..bitmap_size).map(|_| AtomicU8::new(0)).collect();

        TranspositionTable {
            buckets: Vec::new(),
            flexible_buckets: Some(flexible_buckets),
            num_buckets,
            age: 0,
            bucket_size: Some(bucket_size),
            prefetcher: None,
            #[cfg(feature = "tt_metrics")]
            metrics: None,
            occupied_bitmap,
            hashfull_estimate: AtomicU16::new(0),
            node_counter: AtomicU64::new(0),
            need_gc: AtomicBool::new(false),
            gc_progress: AtomicU64::new(0),
            gc_threshold_age_distance: 4,
            high_hashfull_counter: AtomicU16::new(0),
            #[cfg(feature = "tt_metrics")]
            gc_triggered: StdAtomicU64::new(0),
            #[cfg(feature = "tt_metrics")]
            gc_entries_cleared: StdAtomicU64::new(0),
            empty_slot_mode_enabled: AtomicBool::new(false),
            empty_slot_mode_last_hf: AtomicU16::new(0),
        }
    }

    /// Enable adaptive prefetcher
    pub fn enable_prefetcher(&mut self) {
        self.prefetcher = Some(AdaptivePrefetcher::new());
    }

    /// Enable detailed metrics collection
    #[cfg(feature = "tt_metrics")]
    pub fn enable_metrics(&mut self) {
        self.metrics = Some(DetailedTTMetrics::new());
    }

    /// Get bucket index from hash
    #[inline(always)]
    fn bucket_index(&self, hash: u64) -> usize {
        // Use fast masking since num_buckets is always power of 2
        (hash as usize) & (self.num_buckets - 1)
    }

    /// Mark bucket as occupied in bitmap
    #[inline]
    fn mark_bucket_occupied(&self, bucket_idx: usize) {
        let byte_idx = bucket_idx / 8;
        let bit_idx = bucket_idx % 8;
        let mask = 1u8 << bit_idx;

        // Use fetch_or to atomically set the bit
        self.occupied_bitmap[byte_idx].fetch_or(mask, Ordering::Relaxed);
    }

    /// Check if bucket is occupied in bitmap
    #[inline]
    fn is_bucket_occupied(&self, bucket_idx: usize) -> bool {
        let byte_idx = bucket_idx / 8;
        let bit_idx = bucket_idx % 8;
        let mask = 1u8 << bit_idx;

        (self.occupied_bitmap[byte_idx].load(Ordering::Relaxed) & mask) != 0
    }

    /// Clear bucket occupied bit
    #[inline]
    fn clear_bucket_occupied(&self, bucket_idx: usize) {
        let byte_idx = bucket_idx / 8;
        let bit_idx = bucket_idx % 8;
        let mask = !(1u8 << bit_idx);

        self.occupied_bitmap[byte_idx].fetch_and(mask, Ordering::Relaxed);
    }

    /// Update hashfull estimate based on occupied bitmap sampling
    fn update_hashfull_estimate(&self) {
        // Sample ~1% of buckets (minimum 64, maximum 1024)
        let sample_size = (self.num_buckets / 100).clamp(64, 1024);
        let mut occupied_count = 0;

        // Use deterministic sampling based on node counter for consistency
        let start_idx = (self.node_counter.load(Ordering::Relaxed) as usize) % self.num_buckets;

        for i in 0..sample_size {
            let bucket_idx = (start_idx + i * 97) % self.num_buckets; // 97 is prime for good distribution
            if self.is_bucket_occupied(bucket_idx) {
                occupied_count += 1;
            }
        }

        // Calculate hashfull (permille)
        let hashfull = (occupied_count * 1000) / sample_size;
        self.hashfull_estimate.store(hashfull as u16, Ordering::Relaxed);
    }

    /// Get current hashfull estimate
    pub fn hashfull_estimate(&self) -> u16 {
        self.hashfull_estimate.load(Ordering::Relaxed)
    }

    /// Probe transposition table
    pub fn probe(&self, hash: u64) -> Option<TTEntry> {
        debug_assert!(hash != 0, "Attempting to probe with zero hash");

        let idx = self.bucket_index(hash);

        #[cfg(feature = "tt_metrics")]
        if let Some(ref metrics) = self.metrics {
            use metrics::record_metric;
            record_metric(metrics, metrics::MetricType::AtomicLoad);
        }

        // Use prefetcher if enabled
        if let Some(ref _prefetcher) = self.prefetcher {
            // Prefetch next likely bucket
            let next_hash = hash.wrapping_add(1);
            self.prefetch_l1(next_hash);

            #[cfg(feature = "tt_metrics")]
            if let Some(ref metrics) = self.metrics {
                metrics.prefetch_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            flexible_buckets[idx].probe(hash)
        } else {
            self.buckets[idx].probe(hash)
        }
    }

    /// Clear the entire table
    pub fn clear(&mut self) {
        if let Some(ref mut flexible_buckets) = self.flexible_buckets {
            for bucket in flexible_buckets.iter_mut() {
                bucket.clear();
            }
        } else {
            for bucket in self.buckets.iter_mut() {
                bucket.clear();
            }
        }

        // Clear occupied bitmap
        for byte in self.occupied_bitmap.iter() {
            byte.store(0, Ordering::Relaxed);
        }

        // Reset counters
        self.age = 0;
        self.hashfull_estimate.store(0, Ordering::Relaxed);
        self.node_counter.store(0, Ordering::Relaxed);
        self.need_gc.store(false, Ordering::Relaxed);
        self.gc_progress.store(0, Ordering::Relaxed);
        self.high_hashfull_counter.store(0, Ordering::Relaxed);
        self.empty_slot_mode_enabled.store(false, Ordering::Relaxed);
        self.empty_slot_mode_last_hf.store(0, Ordering::Relaxed);

        #[cfg(feature = "tt_metrics")]
        if let Some(ref metrics) = self.metrics {
            metrics.reset();
        }
    }

    /// Increment age (called at the start of each search)
    pub fn increment_age(&mut self) {
        self.age = self.age.wrapping_add(1) & entry::AGE_MASK;

        // Reset GC state for new search
        self.need_gc.store(false, Ordering::Relaxed);
        self.gc_progress.store(0, Ordering::Relaxed);
        self.high_hashfull_counter.store(0, Ordering::Relaxed);
    }

    /// Get current age
    pub fn current_age(&self) -> u8 {
        self.age
    }

    /// Get hashfull in permille (0-1000)
    pub fn hashfull(&self) -> u16 {
        self.hashfull_estimate()
    }

    /// Get size in bytes
    pub fn size_bytes(&self) -> usize {
        if let Some(ref flexible_buckets) = self.flexible_buckets {
            flexible_buckets.len() * flexible_buckets[0].size.bytes()
        } else {
            self.buckets.len() * std::mem::size_of::<TTBucket>()
        }
    }

    /// Set ABDADA exact cut flag for the given hash
    pub fn set_exact_cut(&self, hash: u64) -> bool {
        let idx = self.bucket_index(hash);

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            let bucket = &flexible_buckets[idx];
            let entries_per_bucket = bucket.size.entries();

            // Find the entry with matching key
            for i in 0..entries_per_bucket {
                let key_idx = i * 2;
                let data_idx = key_idx + 1;

                let stored_key = bucket.entries[key_idx].load(Ordering::Acquire);
                if stored_key == hash {
                    // Entry found, set ABDADA flag
                    bucket.entries[data_idx].fetch_or(entry::ABDADA_CUT_FLAG, Ordering::Release);
                    return true;
                }
            }
        } else {
            // Legacy bucket implementation
            let bucket = &self.buckets[idx];

            // Find the entry with matching key
            for i in 0..BUCKET_SIZE {
                let key_idx = i * 2;
                let data_idx = key_idx + 1;

                let stored_key = bucket.entries[key_idx].load(Ordering::Acquire);
                if stored_key == hash {
                    // Entry found, set ABDADA flag
                    bucket.entries[data_idx].fetch_or(entry::ABDADA_CUT_FLAG, Ordering::Release);
                    return true;
                }
            }
        }

        false
    }

    /// Clear ABDADA exact cut flag for the given hash (used during age update)
    pub fn clear_exact_cut(&self, hash: u64) -> bool {
        let idx = self.bucket_index(hash);

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            let bucket = &flexible_buckets[idx];
            let entries_per_bucket = bucket.size.entries();

            // Find the entry with matching key
            for i in 0..entries_per_bucket {
                let key_idx = i * 2;
                let data_idx = key_idx + 1;

                let stored_key = bucket.entries[key_idx].load(Ordering::Acquire);
                if stored_key == hash {
                    // Entry found, clear ABDADA flag with infinite retry
                    // This is a rare path (only on exact hash match), so spinning is acceptable
                    loop {
                        let old_data = bucket.entries[data_idx].load(Ordering::Acquire);
                        let new_data = old_data & !entry::ABDADA_CUT_FLAG;

                        match bucket.entries[data_idx].compare_exchange_weak(
                            old_data,
                            new_data,
                            Ordering::Release,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return true,
                            Err(_) => {
                                // In high contention, yield to OS scheduler
                                std::hint::spin_loop();
                            }
                        }
                    }
                }
            }
        } else {
            // Legacy bucket implementation
            let bucket = &self.buckets[idx];

            // Find the entry with matching key
            for i in 0..BUCKET_SIZE {
                let key_idx = i * 2;
                let data_idx = key_idx + 1;

                let stored_key = bucket.entries[key_idx].load(Ordering::Acquire);
                if stored_key == hash {
                    // Entry found, clear ABDADA flag with infinite retry
                    // This is a rare path (only on exact hash match), so spinning is acceptable
                    loop {
                        let old_data = bucket.entries[data_idx].load(Ordering::Acquire);
                        let new_data = old_data & !entry::ABDADA_CUT_FLAG;

                        match bucket.entries[data_idx].compare_exchange_weak(
                            old_data,
                            new_data,
                            Ordering::Release,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return true,
                            Err(_) => {
                                // In high contention, yield to OS scheduler
                                std::hint::spin_loop();
                            }
                        }
                    }
                }
            }
        }

        false // Entry not found
    }

    /// Store entry in transposition table
    pub fn store(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) {
        let params = TTEntryParams {
            key: hash,
            mv,
            score,
            eval,
            depth,
            node_type,
            age: self.age,
            is_pv: false,
            ..Default::default()
        };
        self.store_entry(params);
    }

    /// Store entry and return whether it was a new entry
    pub fn store_and_check_new(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) -> bool {
        let params = TTEntryParams {
            key: hash,
            mv,
            score,
            eval,
            depth,
            node_type,
            age: self.age,
            is_pv: false,
            ..Default::default()
        };
        self.store_entry_and_check_new(params)
    }

    /// Store entry in transposition table with parameters
    pub fn store_with_params(&self, mut params: TTEntryParams) {
        // Override age with current table age
        params.age = self.age;
        self.store_entry(params);
    }

    /// Store entry using parameters and return whether it was a new entry
    fn store_entry_and_check_new(&self, params: TTEntryParams) -> bool {
        // First check if entry already exists
        let idx = self.bucket_index(params.key);
        let existing = if let Some(ref flexible_buckets) = self.flexible_buckets {
            flexible_buckets[idx].probe(params.key)
        } else {
            self.buckets[idx].probe(params.key)
        };

        // Store the entry
        self.store_entry(params);

        // Return true if this was a new entry (not found before)
        existing.is_none()
    }

    /// Store entry using parameters
    fn store_entry(&self, params: TTEntryParams) {
        #[cfg(not(feature = "tt_metrics"))]
        let _metrics: Option<&()> = None;
        // Debug assertions to validate input values
        debug_assert!(params.key != 0, "Attempting to store entry with zero hash");
        debug_assert!(params.depth <= 127, "Depth value out of reasonable range: {}", params.depth);
        debug_assert!(
            params.score.abs() <= 30000,
            "Score value out of reasonable range: {}",
            params.score
        );

        // Hashfull-based filtering with dynamic depth LUT
        #[cfg(feature = "hashfull_filter")]
        {
            let hf = self.hashfull_estimate();

            // Get depth threshold using optimized branch
            let depth_threshold = get_depth_threshold(hf);

            // Filter based on dynamic depth threshold
            if depth_threshold > 0 && params.depth < depth_threshold {
                #[cfg(feature = "tt_metrics")]
                if let Some(ref metrics) = self.metrics {
                    metrics.hashfull_filtered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return;
            }

            // Additional filtering for non-exact entries at high hashfull
            if hf >= 850 && params.node_type != NodeType::Exact {
                #[cfg(feature = "tt_metrics")]
                if let Some(ref metrics) = self.metrics {
                    metrics.hashfull_filtered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return;
            }
        }

        let idx = self.bucket_index(params.key);

        // Mark bucket as occupied
        self.mark_bucket_occupied(idx);

        // Update node counter and check if we need to update hashfull estimate
        let node_count = self.node_counter.fetch_add(1, Ordering::Relaxed);
        if node_count % 256 == 0 {
            self.update_hashfull_estimate();

            // Check GC trigger conditions
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
                {
                    self.gc_triggered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        let new_entry = TTEntry::from_params(params);

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            // Propagate empty_slot_mode to bucket store
            let empty_slot_mode = self.empty_slot_mode_enabled.load(Ordering::Relaxed);
            flexible_buckets[idx].store_with_metrics_and_mode(
                new_entry,
                self.age,
                empty_slot_mode,
                #[cfg(feature = "tt_metrics")]
                self.metrics.as_ref(),
                #[cfg(not(feature = "tt_metrics"))]
                _metrics,
            );
        } else {
            // Propagate empty_slot_mode to bucket store
            let empty_slot_mode = self.empty_slot_mode_enabled.load(Ordering::Relaxed);
            self.buckets[idx].store_with_mode(
                new_entry,
                self.age,
                empty_slot_mode,
                #[cfg(feature = "tt_metrics")]
                self.metrics.as_ref(),
                #[cfg(not(feature = "tt_metrics"))]
                _metrics,
            );
        }
    }

    /// Prefetch a hash into L1 cache
    #[inline]
    pub fn prefetch_l1(&self, hash: u64) {
        self.prefetch(hash, 3); // Temporal locality hint (L1)
    }

    /// Prefetch a hash into L2 cache
    #[inline]
    pub fn prefetch_l2(&self, hash: u64) {
        self.prefetch(hash, 2); // Moderate temporal locality (L2)
    }

    /// Prefetch a hash into L3 cache
    #[inline]
    pub fn prefetch_l3(&self, hash: u64) {
        self.prefetch(hash, 1); // L3 cache
    }

    /// Prefetch implementation with locality hint
    pub fn prefetch(&self, hash: u64, hint: i32) {
        debug_assert!(hash != 0, "Attempting to prefetch with zero hash");

        let idx = self.bucket_index(hash);

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            flexible_buckets[idx].prefetch(hint);
        } else {
            self.buckets[idx].prefetch(hint);
        }

        // Update prefetcher state if enabled
        if let Some(ref prefetcher) = self.prefetcher {
            prefetcher.record_hit();
        }
    }

    /// Get TT metrics (if enabled)
    #[cfg(feature = "tt_metrics")]
    pub fn metrics(&self) -> Option<&DetailedTTMetrics> {
        self.metrics.as_ref()
    }

    /// Get prefetch statistics
    pub fn prefetch_stats(&self) -> Option<prefetch::PrefetchStats> {
        self.prefetcher.as_ref().map(|p| p.stats())
    }

    /// Start a new search (increment age)
    pub fn new_search(&mut self) {
        self.increment_age();
    }

    /// Get size in MB
    pub fn size(&self) -> usize {
        self.size_bytes() / (1024 * 1024)
    }
}

// Helper functions and additional implementations are in utils.rs
