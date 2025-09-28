//! Optimized transposition table with bucket structure (single table)
//!
//! Overview
//! - Single-table design with cache-friendly buckets (no sharding)
//! - Lock-free writes via atomic publication order (value -> meta -> key/flag)
//! - Generation (age) management and incremental GC
//! - EXACT-chain PV reconstruction integrated here
//!
//! Bucket structure
//! This implementation uses a bucket structure to optimize cache performance:
//! - 4 entries per bucket (64 bytes = 1 cache line)
//! - Improved replacement strategy within buckets
//! - Better memory locality

pub mod bucket;
pub mod bucket_simd;
pub mod budget;
pub mod constants;
pub mod entry;
pub mod filter;
pub mod flexible_bucket;
pub mod gc;
pub mod metrics;
pub mod prefetch;
// pub mod pv_reconstruction; // merged into this module
pub mod utils;

#[cfg(test)]
mod tests;

use crate::Position;
use crate::{search::SEARCH_INF, shogi::Move};
use bucket::TTBucket;
use constants::ABDADA_CUT_FLAG;
use flexible_bucket::FlexibleTTBucket;
use prefetch::PrefetchStatsTracker;
// Integrated PV reconstruction here
#[cfg(feature = "tt_metrics")]
use std::sync::atomic::AtomicU64 as StdAtomicU64;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};
use utils::*;

// No need to import entry module since it's already defined

// Re-export main types for backward compatibility
use crate::search::NodeType;
pub use bucket::BucketSize;
pub use constants::{AGE_MASK, GENERATION_CYCLE};
pub use entry::{TTEntry, TTEntryParams};
#[cfg(feature = "tt_metrics")]
pub use metrics::DetailedTTMetrics;

// Re-export SIMD types
pub mod simd;
pub use simd::{simd_enabled, simd_kind, SimdKind};

// Re-export BUCKET_SIZE from constants
pub use constants::BUCKET_SIZE;

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
    /// Prefetch statistics tracker (legacy, lightweight)
    prefetcher: Option<PrefetchStatsTracker>,
    /// TT performance metrics
    #[cfg(feature = "tt_metrics")]
    metrics: Option<DetailedTTMetrics>,
    /// Bitmap for occupied buckets (1 bit per bucket)
    occupied_bitmap: Vec<AtomicU8>,
    /// Hashfull estimate (updated periodically)
    hashfull_estimate: AtomicU16,
    /// Node counter for periodic updates
    node_counter: AtomicU64,
    /// Diagnostic: number of TT store attempts (filtered含む)
    store_attempts: AtomicU64,
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
            store_attempts: AtomicU64::new(0),
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
            store_attempts: AtomicU64::new(0),
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
        self.prefetcher = Some(PrefetchStatsTracker::new());
    }

    /// Enable detailed metrics collection
    #[cfg(feature = "tt_metrics")]
    pub fn enable_metrics(&mut self) {
        self.metrics = Some(DetailedTTMetrics::new());
    }

    /// Build TT metrics summary string (if metrics enabled)
    #[cfg(feature = "tt_metrics")]
    pub fn metrics_summary_string(&self) -> Option<String> {
        self.metrics.as_ref().map(|m| m.to_summary_string())
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
        if let Some(ref prefetcher) = self.prefetcher {
            // Prefetch next bucket directly (more efficient than rehashing)
            let next_idx = (idx + 1) & (self.num_buckets - 1);
            self.prefetch_bucket(next_idx, 3); // L1 cache hint
            prefetcher.record_call(); // Record the prefetch call for statistics

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
        self.store_attempts.store(0, Ordering::Relaxed);
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
        self.age = self.age.wrapping_add(1) & AGE_MASK;

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

    /// Alias for clarity: returns occupancy in permille (0..=1000)
    #[inline]
    pub fn hashfull_permille(&self) -> u16 {
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

    /// Diagnostic: store attempts counter（Relaxed）
    pub fn store_attempts(&self) -> u64 {
        self.store_attempts.load(Ordering::Relaxed)
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
                    bucket.entries[data_idx].fetch_or(ABDADA_CUT_FLAG, Ordering::Release);
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
                    bucket.entries[data_idx].fetch_or(ABDADA_CUT_FLAG, Ordering::Release);
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
                        let new_data = old_data & !ABDADA_CUT_FLAG;

                        // Record CAS attempt
                        #[cfg(feature = "tt_metrics")]
                        if let Some(ref m) = self.metrics {
                            use metrics::{record_metric, MetricType};
                            record_metric(m, MetricType::CasAttempt);
                        }

                        match bucket.entries[data_idx].compare_exchange_weak(
                            old_data,
                            new_data,
                            Ordering::Release,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => {
                                #[cfg(feature = "tt_metrics")]
                                if let Some(ref m) = self.metrics {
                                    use metrics::{record_metric, MetricType};
                                    record_metric(m, MetricType::CasSuccess);
                                }
                                return true;
                            }
                            Err(_) => {
                                #[cfg(feature = "tt_metrics")]
                                if let Some(ref m) = self.metrics {
                                    use metrics::{record_metric, MetricType};
                                    record_metric(m, MetricType::CasFailure);
                                }
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
                        let new_data = old_data & !ABDADA_CUT_FLAG;

                        // Record CAS attempt
                        #[cfg(feature = "tt_metrics")]
                        if let Some(ref m) = self.metrics {
                            use metrics::{record_metric, MetricType};
                            record_metric(m, MetricType::CasAttempt);
                        }

                        match bucket.entries[data_idx].compare_exchange_weak(
                            old_data,
                            new_data,
                            Ordering::Release,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => {
                                #[cfg(feature = "tt_metrics")]
                                if let Some(ref m) = self.metrics {
                                    use metrics::{record_metric, MetricType};
                                    record_metric(m, MetricType::CasSuccess);
                                }
                                return true;
                            }
                            Err(_) => {
                                #[cfg(feature = "tt_metrics")]
                                if let Some(ref m) = self.metrics {
                                    use metrics::{record_metric, MetricType};
                                    record_metric(m, MetricType::CasFailure);
                                }
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

    /// Check if ABDADA exact cut flag is set for the given hash
    pub fn has_exact_cut(&self, hash: u64) -> bool {
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
                    // Entry found, check ABDADA flag
                    let data = bucket.entries[data_idx].load(Ordering::Acquire);
                    return (data & ABDADA_CUT_FLAG) != 0;
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
                    // Entry found, check ABDADA flag
                    let data = bucket.entries[data_idx].load(Ordering::Acquire);
                    return (data & ABDADA_CUT_FLAG) != 0;
                }
            }
        }

        false // Entry not found or flag not set
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
        // Lightweight diagnostics: count store attempts even if filtered
        self.store_attempts.fetch_add(1, Ordering::Relaxed);
        #[cfg(not(feature = "tt_metrics"))]
        let _metrics: Option<&()> = None;
        // Debug assertions to validate input values
        debug_assert!(params.key != 0, "Attempting to store entry with zero hash");
        debug_assert!(params.depth <= 127, "Depth value out of reasonable range: {}", params.depth);
        debug_assert!(
            params.score.abs() <= SEARCH_INF as i16,
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
        if node_count.is_multiple_of(256) {
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

    /// Reconstruct PV from transposition table using only EXACT entries
    ///
    /// This function follows the best moves stored in EXACT TT entries to build
    /// a principal variation. It stops at the first non-EXACT entry to ensure
    /// PV reliability.
    ///
    /// # Arguments
    /// * `pos` - Current position to start reconstruction from
    /// * `max_depth` - Maximum depth to search (prevents infinite loops)
    ///
    /// # Returns
    /// * Vector of moves forming the PV (empty if no PV found)
    pub fn reconstruct_pv_from_tt(&self, pos: &mut Position, max_depth: u8) -> Vec<Move> {
        reconstruct_pv_generic(self, pos, max_depth)
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
            prefetcher.record_call();
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

    /// Prefetch a specific bucket by index
    #[inline]
    fn prefetch_bucket(&self, bucket_idx: usize, hint: i32) {
        if let Some(ref flexible_buckets) = self.flexible_buckets {
            flexible_buckets[bucket_idx].prefetch(hint);
        } else {
            self.buckets[bucket_idx].prefetch(hint);
        }
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

impl TTProbe for TranspositionTable {
    fn probe(&self, hash: u64) -> Option<TTEntry> {
        self.probe(hash)
    }
}

#[cfg(test)]
mod pv_reconstruction_tests {
    use super::*;
    use crate::{
        movegen::MoveGenerator,
        search::test_utils::test_helpers::legal_usi,
        shogi::{Move, Position},
        usi::{move_to_usi, parse_usi_square},
        PieceType,
    };

    #[test]
    fn test_reconstruct_pv_from_tt_exact_only() {
        // Create a TT with some capacity
        let mut tt = TranspositionTable::new(1); // 1MB

        // Initialize TT for new search (sets age)
        tt.new_search();

        // Create a position
        let mut pos = Position::startpos();

        // First, test basic TT functionality
        let test_hash = pos.zobrist_hash;
        let test_move = legal_usi(&pos, "7g7f");
        tt.store(test_hash, Some(test_move), 100, 50, 10, NodeType::Exact);

        // Verify the entry was stored
        let probe_result = tt.probe(test_hash);
        assert!(probe_result.is_some(), "TT probe should find the entry");
        let entry = probe_result.unwrap();
        assert!(entry.matches(test_hash), "Entry should match the hash");
        // TT stores moves as 16-bit, so piece type info is lost. Compare USI strings instead.
        let stored_move = entry.get_move().unwrap();
        assert_eq!(move_to_usi(&stored_move), move_to_usi(&test_move), "Move USI should match");
        assert_eq!(entry.node_type(), NodeType::Exact, "Node type should be Exact");

        // Clear for actual test
        tt.clear();
        tt.new_search();

        // Generate legal moves and find the ones we want
        let move_gen = MoveGenerator::new();
        let moves = move_gen.generate_all(&pos).expect("Should be able to generate moves in test");

        let move1 = moves
            .as_slice()
            .iter()
            .find(|m| move_to_usi(m) == "7g7f")
            .cloned()
            .expect("7g7f should be legal");

        let undo1 = pos.do_move(move1);
        let move_gen2 = MoveGenerator::new();
        let moves2 =
            move_gen2.generate_all(&pos).expect("Should be able to generate moves in test");

        let move2 = moves2
            .as_slice()
            .iter()
            .find(|m| move_to_usi(m) == "3c3d")
            .cloned()
            .expect("3c3d should be legal after 7g7f");

        let undo2 = pos.do_move(move2);
        let move_gen3 = MoveGenerator::new();
        let moves3 =
            move_gen3.generate_all(&pos).expect("Should be able to generate moves in test");

        let move3 = moves3
            .as_slice()
            .iter()
            .find(|m| move_to_usi(m) == "6g6f")
            .cloned()
            .expect("6g6f should be legal after 7g7f 3c3d");

        // Undo to get back to start
        pos.undo_move(move2, undo2);
        pos.undo_move(move1, undo1);

        // Store some entries in TT
        let hash1 = pos.zobrist_hash;
        tt.store(hash1, Some(move1), 100, 50, 10, NodeType::Exact);

        // Make move1
        let undo1 = pos.do_move(move1);
        let hash2 = pos.zobrist_hash;
        tt.store(hash2, Some(move2), -50, -25, 9, NodeType::Exact);

        // Make move2
        let undo2 = pos.do_move(move2);
        let hash3 = pos.zobrist_hash;
        tt.store(hash3, Some(move3), 25, 20, 8, NodeType::Exact);

        // Undo moves to get back to root
        pos.undo_move(move2, undo2);
        pos.undo_move(move1, undo1);

        // Reconstruct PV
        let pv = tt.reconstruct_pv_from_tt(&mut pos, 10);

        // Should get all 3 moves since they're all EXACT
        assert_eq!(pv.len(), 3);
        // Compare USI strings since TT loses piece type info
        assert_eq!(move_to_usi(&pv[0]), "7g7f");
        assert_eq!(move_to_usi(&pv[1]), "3c3d");
        assert_eq!(move_to_usi(&pv[2]), "6g6f");
    }

    #[test]
    fn test_reconstruct_pv_stops_at_non_exact() {
        // Create a TT
        let mut tt = TranspositionTable::new(1);

        // Initialize TT for new search (sets age)
        tt.new_search();

        // Create a position
        let mut pos = Position::startpos();

        // Generate legal moves and find the ones we want
        let move_gen = MoveGenerator::new();
        let moves = move_gen.generate_all(&pos).expect("Should be able to generate moves in test");

        let move1 = moves
            .as_slice()
            .iter()
            .find(|m| move_to_usi(m) == "7g7f")
            .cloned()
            .expect("7g7f should be legal");

        let undo1_temp = pos.do_move(move1);
        let move_gen2 = MoveGenerator::new();
        let moves2 =
            move_gen2.generate_all(&pos).expect("Should be able to generate moves in test");

        let move2 = moves2
            .as_slice()
            .iter()
            .find(|m| move_to_usi(m) == "3c3d")
            .cloned()
            .expect("3c3d should be legal after 7g7f");

        // Undo to get back to start
        pos.undo_move(move1, undo1_temp);

        // Store first move as EXACT
        let hash1 = pos.zobrist_hash;
        tt.store(hash1, Some(move1), 100, 50, 10, NodeType::Exact);

        // Make move1
        let undo1 = pos.do_move(move1);
        let hash2 = pos.zobrist_hash;

        // Store second move as LOWERBOUND (not EXACT)
        tt.store(hash2, Some(move2), -50, -25, 9, NodeType::LowerBound);

        // Undo to root
        pos.undo_move(move1, undo1);

        // Reconstruct PV
        let pv = tt.reconstruct_pv_from_tt(&mut pos, 10);

        // Should only get first move since second is not EXACT
        assert_eq!(pv.len(), 1);
        // Compare USI strings since TT loses piece type info
        assert_eq!(move_to_usi(&pv[0]), "7g7f");
    }

    #[test]
    fn test_reconstruct_pv_handles_cycles() {
        // Create a TT
        let mut tt = TranspositionTable::new(1);

        // Initialize TT for new search (sets age)
        tt.new_search();

        // Create a position
        let mut pos = Position::startpos();

        // Get legal moves that can create a cycle (using pieces that can move back)
        // Using Gold general (5i5h, then 5h5i is possible)
        let move1 = legal_usi(&pos, "5i5h");

        let undo1 = pos.do_move(move1);
        let move2 = legal_usi(&pos, "5a5b");

        let undo2 = pos.do_move(move2);
        let move3 = legal_usi(&pos, "5h5i"); // Gold can move back

        let undo3 = pos.do_move(move3);
        let move4 = legal_usi(&pos, "5b5a"); // Gold can move back

        // Undo moves to get back to earlier positions for storing
        pos.undo_move(move3, undo3);
        pos.undo_move(move2, undo2);
        pos.undo_move(move1, undo1);

        // Store entries that would create a cycle
        let hash1 = pos.zobrist_hash;
        tt.store(hash1, Some(move1), 0, 0, 10, NodeType::Exact);

        let undo1 = pos.do_move(move1);
        let hash2 = pos.zobrist_hash;
        tt.store(hash2, Some(move2), 0, 0, 9, NodeType::Exact);

        let undo2 = pos.do_move(move2);
        let hash3 = pos.zobrist_hash;
        tt.store(hash3, Some(move3), 0, 0, 8, NodeType::Exact);

        let undo3 = pos.do_move(move3);
        let hash4 = pos.zobrist_hash;
        tt.store(hash4, Some(move4), 0, 0, 7, NodeType::Exact);

        // Make move4 to complete the cycle
        let undo4 = pos.do_move(move4);

        // Add a move that would create a cycle back to start position
        tt.store(pos.zobrist_hash, Some(move1), 0, 0, 6, NodeType::Exact);

        // Undo all moves to get back to start
        pos.undo_move(move4, undo4);
        pos.undo_move(move3, undo3);
        pos.undo_move(move2, undo2);
        pos.undo_move(move1, undo1);

        // Reconstruct PV - should stop when cycle is detected
        let pv = tt.reconstruct_pv_from_tt(&mut pos, 20);

        // The PV should stop when it detects that the next position would be a repeat
        // In this case, after move1, move2, move3, move4, we would be back at a position
        // we've already seen (the position after move1), so it should stop there
        assert!(pv.len() >= 4, "PV should have at least 4 moves, got {}", pv.len());
    }

    #[test]
    fn test_reconstruct_pv_stops_on_illegal_tt_move() {
        // Test that PV reconstruction stops when TT contains an illegal move
        let mut tt = TranspositionTable::new(1);

        // Initialize TT for new search (sets age)
        tt.new_search();

        // Create a position
        let mut pos = Position::startpos();

        // Get a legal move
        let move1 = legal_usi(&pos, "7g7f");

        // Create an illegal move (moving a piece that doesn't exist)
        // This simulates TT corruption or a hash collision
        let illegal_move = Move::normal_with_piece(
            parse_usi_square("5e").unwrap(), // Empty square
            parse_usi_square("5d").unwrap(),
            false,
            PieceType::Pawn,
            None,
        );

        // Store first move
        let hash1 = pos.zobrist_hash;
        tt.store(hash1, Some(move1), 100, 50, 10, NodeType::Exact);

        // Make first move
        let undo1 = pos.do_move(move1);
        let hash2 = pos.zobrist_hash;

        // Store illegal move for second position
        tt.store(hash2, Some(illegal_move), 50, 25, 9, NodeType::Exact);

        // Undo to get back to start
        pos.undo_move(move1, undo1);

        // Reconstruct PV
        let pv = tt.reconstruct_pv_from_tt(&mut pos, 10);

        // PV should contain only the first legal move and stop at the illegal move
        assert_eq!(pv.len(), 1, "PV should stop at illegal move");
        // Compare USI strings since TT loses piece type info
        assert_eq!(move_to_usi(&pv[0]), "7g7f", "PV should contain the first legal move");
    }
}

// Helper functions and additional implementations are in utils.rs

// --- Integrated PV reconstruction (moved from pv_reconstruction.rs) ---
pub trait TTProbe {
    fn probe(&self, hash: u64) -> Option<TTEntry>;
}

/// Generic PV reconstruction from transposition table
pub fn reconstruct_pv_generic<T: TTProbe>(tt: &T, pos: &mut Position, max_depth: u8) -> Vec<Move> {
    use crate::movegen::MoveGenerator;
    use crate::search::NodeType;
    use std::collections::HashSet;

    let mut pv = Vec::new();
    let mut visited: HashSet<u64> = HashSet::new();
    let max_len = max_depth.min(crate::search::constants::MAX_PLY as u8) as usize;

    for _ in 0..max_len {
        let hash = pos.zobrist_hash;
        if !visited.insert(hash) {
            break;
        }
        let entry = match tt.probe(hash) {
            Some(e) if e.matches(hash) => e,
            _ => break,
        };
        if entry.node_type() != NodeType::Exact {
            break;
        }
        const MIN_DEPTH_FOR_PV_TRUST: u8 = 4;
        if entry.depth() < MIN_DEPTH_FOR_PV_TRUST && !pv.is_empty() {
            break;
        }
        let Some(best) = entry.get_move() else { break };
        let mg = MoveGenerator::new();
        let Ok(legals) = mg.generate_all(pos) else {
            break;
        };
        let Some(found) = legals.as_slice().iter().find(|m| m.equals_without_piece_type(&best))
        else {
            break;
        };
        let mv = *found;
        pv.push(mv);
        let _undo = pos.do_move(mv);
        // terminal check
        let mg2 = MoveGenerator::new();
        if mg2.generate_all(pos).map(|v| v.is_empty()).unwrap_or(true) {
            break;
        }
    }
    pv
}
