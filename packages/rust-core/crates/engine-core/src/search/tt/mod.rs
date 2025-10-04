//! Optimized transposition table with bucket structure (single table)
//!
//! Overview
//! - Single-table design with cache-friendly buckets (no sharding)
//! - Lock-free writes via atomic publication order
//! - Generation (age) management and incremental GC
//! - EXACT-chain PV reconstruction integrated here
//!
//! Memory ordering invariants (reader/writer contract)
//! - Reader (probe): `key.load(Acquire)` → if `key!=0` then `data.load(Relaxed)` and validate
//! - Empty insert: publish `data.store(new, Release)` → then `key.store(new, Release)`
//! - Replacement (worst entry): `data.store(0, Release)` → `key.compare_exchange(old, new, Release, Acquire)` →
//!   on success `data.store(new, Release)`
//! - Deletion/GC: `data.store(0, Release)` → `key.store(0, Release)`
//!   These rules ensure readers never observe a "new key + old data(depth>0)" combination.
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
use crate::search::tt::filter::{boost_pv_depth, boost_tt_depth, should_skip_tt_store_dyn};

use crate::Position;
use crate::{search::SEARCH_INF, shogi::Move, Color};
use bucket::TTBucket;
use constants::ABDADA_CUT_FLAG;
use flexible_bucket::FlexibleTTBucket;
use prefetch::PrefetchStatsTracker;
// Integrated PV reconstruction here
#[cfg(feature = "tt_metrics")]
use std::sync::atomic::AtomicU64 as StdAtomicU64;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};

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
    /// Debug/diagnostic table id
    id: u64,
    /// Buckets for the transposition table (legacy fixed-size)
    buckets: Vec<TTBucket>,
    /// Flexible buckets (new dynamic-size)
    flexible_buckets: Option<Vec<FlexibleTTBucket>>,
    /// Number of buckets (always power of 2)
    num_buckets: usize,
    /// Current age (generation counter)
    age: AtomicU8,
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

// Report `key == 0` へのストアは設計上あり得ないため、一度でも発生したらログに流して即座に切り分けできるようにする。
static ZERO_KEY_LOG_ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl TranspositionTable {
    /// Create new transposition table with given size in MB (backward compatible)
    pub fn new(size_mb: usize) -> Self {
        // Use legacy implementation for backward compatibility
        // Each bucket is 64 bytes
        let bucket_size = std::mem::size_of::<TTBucket>();
        debug_assert_eq!(bucket_size, 64);

        let mut num_buckets = if size_mb == 0 {
            // Minimum size: 64KB = 1024 buckets
            1024
        } else {
            // 飽和乗算で極端なサイズ指定時のオーバーフローを防止
            size_mb.saturating_mul(1024 * 1024) / bucket_size
        };
        // Round up to next power of two for fast indexing
        if !num_buckets.is_power_of_two() {
            num_buckets = num_buckets.next_power_of_two();
        }

        // Allocate buckets
        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            buckets.push(TTBucket::new());
        }

        // Initialize occupied bitmap - 1 bit per bucket
        let bitmap_size = num_buckets.div_ceil(8); // Round up to nearest byte
        let occupied_bitmap = (0..bitmap_size).map(|_| AtomicU8::new(0)).collect();

        // Assign unique TT id
        static NEXT_TT_ID: AtomicU64 = AtomicU64::new(1);
        let my_id = NEXT_TT_ID.fetch_add(1, Ordering::Relaxed);

        // Basic health asserts and init log
        debug_assert!(num_buckets.is_power_of_two(), "num_buckets must be power of two");
        debug_assert_eq!(bitmap_size, num_buckets.div_ceil(8));
        log::info!(
            "TT init: size_mb={} num_buckets={} mask=0x{:x} fixed_bucket_size={}B",
            size_mb,
            num_buckets,
            num_buckets - 1,
            bucket_size
        );

        TranspositionTable {
            id: my_id,
            buckets,
            flexible_buckets: None,
            num_buckets,
            age: AtomicU8::new(0),
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

        let mut num_buckets = if size_mb == 0 {
            // Minimum size depends on bucket size
            match bucket_size {
                BucketSize::Small => 1024, // 64KB minimum
                BucketSize::Medium => 512, // 64KB minimum
                BucketSize::Large => 256,  // 64KB minimum
            }
        } else {
            // 飽和乗算で極端なサイズ指定時のオーバーフローを防止
            size_mb.saturating_mul(1024 * 1024) / bytes_per_bucket
        };
        // Round to power of 2 for fast indexing
        if !num_buckets.is_power_of_two() {
            num_buckets = num_buckets.next_power_of_two();
        }

        // Allocate flexible buckets
        let mut flexible_buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            flexible_buckets.push(FlexibleTTBucket::new(bucket_size));
        }

        // Initialize occupied bitmap - 1 bit per bucket
        let bitmap_size = num_buckets.div_ceil(8); // Round up to nearest byte
        let occupied_bitmap = (0..bitmap_size).map(|_| AtomicU8::new(0)).collect();

        // Assign unique TT id
        static NEXT_TT_ID: AtomicU64 = AtomicU64::new(1);
        let my_id = NEXT_TT_ID.fetch_add(1, Ordering::Relaxed);

        debug_assert!(num_buckets.is_power_of_two(), "num_buckets must be power of two");
        debug_assert_eq!(bitmap_size, num_buckets.div_ceil(8));
        log::info!(
            "TT init(flex): size_mb={} num_buckets={} mask=0x{:x} bucket_size={:?} entry_bytes=16",
            size_mb,
            num_buckets,
            num_buckets - 1,
            bucket_size
        );

        TranspositionTable {
            id: my_id,
            buckets: Vec::new(),
            flexible_buckets: Some(flexible_buckets),
            num_buckets,
            age: AtomicU8::new(0),
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
    fn bucket_index(&self, hash: u64, _side_to_move: Color) -> usize {
        // Standard approach: Use hash directly for indexing
        // Position's Zobrist hash already includes side_to_move (64-bit random key),
        // so no additional XOR is needed. Adding 1-bit XOR would reduce entropy.
        //
        // Note: side_to_move parameter kept for API compatibility but unused.
        let idx = (hash as usize) & (self.num_buckets - 1);

        // 診断ログ追加
        #[cfg(feature = "diagnostics")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static INDEX_COUNT: AtomicU64 = AtomicU64::new(0);
            let count = INDEX_COUNT.fetch_add(1, Ordering::Relaxed);
            if count < 20 {
                // 最初の20回だけログ
                eprintln!(
                    "[TT_DIAG] bucket_index: hash={hash:016x} -> idx={idx} (num_buckets={})",
                    self.num_buckets
                );
            }
        }

        idx
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

    /// Clear bucket occupied bit
    #[inline]
    fn clear_bucket_occupied(&self, bucket_idx: usize) {
        let byte_idx = bucket_idx / 8;
        let bit_idx = bucket_idx % 8;
        let mask = !(1u8 << bit_idx);

        self.occupied_bitmap[byte_idx].fetch_and(mask, Ordering::Relaxed);
    }

    /// Update hashfull estimate by sampling actual keys (key != 0) in buckets
    ///
    /// 本実装の hashfull は「物理占有率（キー非ゼロのバケット割合）」の推定値です。
    /// YaneuraOu/Stockfish系の `hashfull(maxAge)` は「現行世代（相対年齢が閾値以下）の活性度」を
    /// 意味し設計上の解釈が異なります。そのため YYO 想定のしきい値(例: 600/800/900/950)を
    /// 直接流用すると意図とズレる可能性があります。本実装側のフィルタはこの“物理占有率”に
    /// 前提を置いて較正してあります。
    fn update_hashfull_estimate(&self) {
        // Sample ~1% of buckets (minimum 64, maximum 1024), but never exceed num_buckets
        let mut sample_size = (self.num_buckets / 100).clamp(64, 1024);
        if sample_size > self.num_buckets {
            sample_size = self.num_buckets;
        }
        let mut occupied_count = 0usize;

        // Deterministic sampling start based on node counter
        let start_idx =
            (self.node_counter.load(Ordering::Relaxed) as usize) & (self.num_buckets - 1);

        for i in 0..sample_size {
            let idx = (start_idx.wrapping_add(i * 97)) & (self.num_buckets - 1); // 97 is prime
                                                                                 // Always compute from actual keys (bitmapを使わない)
            let any = if let Some(ref flex) = self.flexible_buckets {
                flex[idx].any_key_nonzero_acquire()
            } else {
                self.buckets[idx].any_key_nonzero_acquire()
            };
            if any {
                occupied_count += 1;
            }
        }

        let mut hf = if sample_size > 0 {
            (occupied_count * 1000) / sample_size
        } else {
            0
        };

        if hf == 0 {
            let store_attempts = self.store_attempts.load(Ordering::Relaxed);
            if self.num_buckets > 0 {
                let approx = (store_attempts.min(self.num_buckets as u64) * 1000)
                    / (self.num_buckets as u64);
                hf = hf.max(approx as usize);
            }

            if hf == 0 {
                hf = self.hashfull_physical_permille() as usize;
            }

            if hf == 0 && store_attempts > 0 {
                hf = 1;
            }
        }

        self.hashfull_estimate.store(hf as u16, Ordering::Relaxed);
    }

    /// Get current hashfull estimate
    pub fn hashfull_estimate(&self) -> u16 {
        self.hashfull_estimate.load(Ordering::Relaxed)
    }

    /// Probe transposition table
    pub fn probe_entry(&self, hash: u64, side_to_move: Color) -> Option<TTEntry> {
        debug_assert!(hash != 0, "Attempting to probe with zero hash");

        let idx = self.bucket_index(hash, side_to_move);

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

        let result = if let Some(ref flexible_buckets) = self.flexible_buckets {
            #[cfg(feature = "tt_metrics")]
            {
                flexible_buckets[idx].probe_with_metrics(hash, self.metrics.as_ref())
            }
            #[cfg(not(feature = "tt_metrics"))]
            {
                flexible_buckets[idx].probe(hash)
            }
        } else {
            #[cfg(feature = "tt_metrics")]
            {
                self.buckets[idx].probe_with_metrics(hash, self.metrics.as_ref())
            }
            #[cfg(not(feature = "tt_metrics"))]
            {
                self.buckets[idx].probe(hash)
            }
        };

        // 診断ログ追加（ヒット/ミスの情報）
        #[cfg(feature = "diagnostics")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};

            const PROBE_LOG_LIMIT: u64 = 200;

            static PROBE_COUNT: AtomicU64 = AtomicU64::new(0);
            static HIT_COUNT: AtomicU64 = AtomicU64::new(0);

            let seq = PROBE_COUNT.fetch_add(1, Ordering::Relaxed);
            if seq < PROBE_LOG_LIMIT {
                match result {
                    Some(entry) => {
                        let hit_seq = HIT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                        eprintln!(
                            "[TT_TRACE] probe#{seq} hit#{hit_seq} idx={idx} hash={hash:016x} side={side_to_move:?} depth={} node_type={:?} mv={:?}",
                            entry.depth(),
                            entry.node_type(),
                            entry.get_move()
                        );
                    }
                    None => {
                        eprintln!(
                            "[TT_TRACE] probe#{seq} miss    idx={idx} hash={hash:016x} side={side_to_move:?}"
                        );
                    }
                }
            } else if result.is_some() {
                let hit_seq = HIT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                if hit_seq <= PROBE_LOG_LIMIT {
                    if let Some(entry) = result {
                        eprintln!(
                            "[TT_TRACE] probe-hit(extra) hit#{hit_seq} idx={idx} hash={hash:016x} side={side_to_move:?} depth={} node_type={:?} mv={:?}",
                            entry.depth(),
                            entry.node_type(),
                            entry.get_move()
                        );
                    }
                }
            }
        }

        result
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
        self.age.store(0, Ordering::Relaxed);
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

    /// Clear the entire table in-place without requiring exclusive ownership of Arc
    /// This keeps the Arc identity stable so参照側と保存側の不一致を避けられる
    pub fn clear_in_place(&self) {
        // Buckets
        if let Some(ref flexible_buckets) = self.flexible_buckets {
            for bucket in flexible_buckets.iter() {
                bucket.clear_atomic();
            }
        } else {
            for bucket in self.buckets.iter() {
                bucket.clear_atomic();
            }
        }

        // Occupied bitmap
        for byte in self.occupied_bitmap.iter() {
            byte.store(0, Ordering::Relaxed);
        }

        // Atomic counters/state (including age)
        self.age.store(0, Ordering::Relaxed);
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
        let next = (self.age.load(Ordering::Relaxed) & AGE_MASK).wrapping_add(1) & AGE_MASK;
        self.age.store(next, Ordering::Relaxed);

        // Reset GC state for new search
        self.need_gc.store(false, Ordering::Relaxed);
        self.gc_progress.store(0, Ordering::Relaxed);
        self.high_hashfull_counter.store(0, Ordering::Relaxed);
    }

    /// Get current age
    pub fn current_age(&self) -> u8 {
        self.age.load(Ordering::Relaxed) & AGE_MASK
    }

    /// Bump age with shared reference (for shared TT with Arc)
    pub fn bump_age(&self) {
        let _ = self.age.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some(v.wrapping_add(1) & AGE_MASK)
        });
        // Reset GC state for new search
        self.need_gc.store(false, Ordering::Relaxed);
        self.gc_progress.store(0, Ordering::Relaxed);
        self.high_hashfull_counter.store(0, Ordering::Relaxed);
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

    /// Compute physical occupancy by scanning the bitmap
    pub fn hashfull_physical_permille(&self) -> u16 {
        let mut occupied_bits: u64 = 0;
        for byte in &self.occupied_bitmap {
            occupied_bits += byte.load(Ordering::Relaxed).count_ones() as u64;
        }
        if self.num_buckets == 0 {
            return 0;
        }
        let permille = (occupied_bits * 1000).div_ceil(self.num_buckets as u64);
        permille.min(1000) as u16
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
    pub fn set_exact_cut(&self, hash: u64, side_to_move: Color) -> bool {
        let idx = self.bucket_index(hash, side_to_move);

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
    pub fn clear_exact_cut(&self, hash: u64, side_to_move: Color) -> bool {
        let idx = self.bucket_index(hash, side_to_move);

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
                            record_metric(m, MetricType::CasAttemptData);
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
                                    record_metric(m, MetricType::CasSuccessData);
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
                            record_metric(m, MetricType::CasAttemptData);
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
                                    record_metric(m, MetricType::CasSuccessData);
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
    pub fn has_exact_cut(&self, hash: u64, side_to_move: Color) -> bool {
        let idx = self.bucket_index(hash, side_to_move);

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

    /// Store entry in transposition table (convenience method)
    pub fn store(&self, args: TTStoreArgs) {
        let params: TTEntryParams = args.into_params(self.current_age());

        // 診断ログ追加
        #[cfg(feature = "diagnostics")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            const STORE_LOG_LIMIT: u64 = 200;
            static STORE_COUNT: AtomicU64 = AtomicU64::new(0);
            let log_seq = STORE_COUNT.fetch_add(1, Ordering::Relaxed);
            if log_seq < STORE_LOG_LIMIT {
                let bucket_idx = self.bucket_index(params.key, params.side_to_move);
                eprintln!(
                    "[TT_TRACE] store#{log_seq} idx={bucket_idx} hash={:016x} side={:?} depth={} node_type={:?} mv={:?}",
                    params.key,
                    params.side_to_move,
                    params.depth,
                    params.node_type,
                    params.mv
                );
            }
        }

        self.store_with_params(params);
    }

    /// Store entry and return whether it was a new entry (convenience method)
    pub fn store_and_check_new(&self, args: TTStoreArgs) -> bool {
        let params: TTEntryParams = args.into_params(self.current_age());
        self.store_and_check_new_with_params(params)
    }

    /// Store entry and return whether it was a new entry (with params)
    pub fn store_and_check_new_with_params(&self, params: TTEntryParams) -> bool {
        self.store_entry_and_check_new(params)
    }

    /// Store entry in transposition table with parameters
    pub fn store_with_params(&self, mut params: TTEntryParams) {
        // Override age with current table age
        params.age = self.current_age();
        self.store_entry(params);
    }

    /// Store entry using parameters and return whether it was a new entry
    fn store_entry_and_check_new(&self, params: TTEntryParams) -> bool {
        // First check if entry already exists
        let idx = self.bucket_index(params.key, params.side_to_move);
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
    fn store_entry(&self, mut params: TTEntryParams) {
        // Lightweight diagnostics: count store attempts even if filtered
        self.store_attempts.fetch_add(1, Ordering::Relaxed);
        if params.key == 0 {
            if ZERO_KEY_LOG_ONCE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                log::warn!(
                    "info string tt_store_key_zero side={:?} depth={} node_type={:?} is_pv={}",
                    params.side_to_move,
                    params.depth,
                    params.node_type,
                    params.is_pv
                );
            }
            return;
        }
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

        // Unified, always-on filtering based on current hashfull estimate
        // Note: Our hashfull is a physical occupancy estimate (see above doc).
        let hf = self.hashfull_estimate();
        if should_skip_tt_store_dyn(params.depth, params.is_pv, params.node_type, hf) {
            #[cfg(feature = "tt_metrics")]
            if let Some(ref metrics) = self.metrics {
                metrics.hashfull_filtered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            return;
        }

        // Depth boosting for important nodes (EXACT / PV)
        let mut eff_depth = params.depth;
        eff_depth = boost_tt_depth(eff_depth, params.node_type);
        eff_depth = boost_pv_depth(eff_depth, params.is_pv);
        if eff_depth != params.depth {
            params.depth = eff_depth.min(127);
        }

        let idx = self.bucket_index(params.key, params.side_to_move);

        // Mark bucket as occupied
        self.mark_bucket_occupied(idx);

        // Update node counter and check if we need to update hashfull estimate
        let node_count = self.node_counter.fetch_add(1, Ordering::Relaxed);
        if (node_count & 255) == 0 {
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
                self.current_age(),
                empty_slot_mode,
                #[cfg(feature = "tt_metrics")]
                self.metrics.as_ref(),
                #[cfg(not(feature = "tt_metrics"))]
                _metrics,
            );
            // Note: Debug assertion removed because store_internal may skip replacement
            // if new entry has lower priority than worst entry in bucket (normal behavior)
        } else {
            // Propagate empty_slot_mode to bucket store
            let empty_slot_mode = self.empty_slot_mode_enabled.load(Ordering::Relaxed);
            self.buckets[idx].store_with_mode(
                new_entry,
                self.current_age(),
                empty_slot_mode,
                #[cfg(feature = "tt_metrics")]
                self.metrics.as_ref(),
                #[cfg(not(feature = "tt_metrics"))]
                _metrics,
            );
            // Note: Debug assertion removed because store_internal may skip replacement
            // if new entry has lower priority than worst entry in bucket (normal behavior)
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
    pub fn prefetch_l1(&self, hash: u64, side_to_move: Color) {
        self.prefetch(hash, side_to_move, 3); // Temporal locality hint (L1)
    }

    /// Prefetch a hash into L2 cache
    #[inline]
    pub fn prefetch_l2(&self, hash: u64, side_to_move: Color) {
        self.prefetch(hash, side_to_move, 2); // Moderate temporal locality (L2)
    }

    /// Prefetch a hash into L3 cache
    #[inline]
    pub fn prefetch_l3(&self, hash: u64, side_to_move: Color) {
        self.prefetch(hash, side_to_move, 1); // L3 cache
    }

    /// Prefetch implementation with locality hint
    pub fn prefetch(&self, hash: u64, side_to_move: Color, hint: i32) {
        debug_assert!(hash != 0, "Attempting to prefetch with zero hash");

        let idx = self.bucket_index(hash, side_to_move);

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

    /// Debug helpers
    #[cfg(any(debug_assertions, feature = "tt_metrics"))]
    pub fn debug_roundtrip(&self, key: u64) -> bool {
        use crate::shogi::Move;
        use crate::Color;
        // Store an EXACT entry at depth 10 and probe it back
        self.store(TTStoreArgs::new(key, None::<Move>, 0, 0, 10, NodeType::Exact, Color::Black));
        match self.probe_entry(key, Color::Black) {
            Some(e) => e.key == key && e.depth() >= 10,
            None => false,
        }
    }

    /// Get TT id for diagnostics
    pub fn id(&self) -> u64 {
        self.id
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

/// 引数過多警告(clippy::too_many_arguments)を避けるためのストア用引数構造体
#[derive(Clone, Copy)]
pub struct TTStoreArgs {
    pub hash: u64,
    pub mv: Option<Move>,
    pub score: i16,
    pub eval: i16,
    pub depth: u8,
    pub node_type: NodeType,
    pub side_to_move: Color,
    pub is_pv: bool, // 将来的に PV ストア時に利用
    // 拡張フラグ（任意）
    pub singular_extension: bool,
    pub null_move: bool,
    pub tt_move_tried: bool,
    pub mate_threat: bool,
}

impl Default for TTStoreArgs {
    fn default() -> Self {
        Self {
            hash: 0,
            mv: None,
            score: 0,
            eval: 0,
            depth: 0,
            node_type: NodeType::Exact,
            side_to_move: Color::Black,
            is_pv: false,
            singular_extension: false,
            null_move: false,
            tt_move_tried: false,
            mate_threat: false,
        }
    }
}

impl TTStoreArgs {
    pub fn new(
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
        side_to_move: Color,
    ) -> Self {
        Self {
            hash,
            mv,
            score,
            eval,
            depth,
            node_type,
            side_to_move,
            ..Default::default()
        }
    }

    fn into_params(self, current_age: u8) -> TTEntryParams {
        TTEntryParams {
            key: self.hash,
            mv: self.mv,
            score: self.score,
            eval: self.eval,
            depth: self.depth,
            node_type: self.node_type,
            age: current_age,
            is_pv: self.is_pv,
            side_to_move: self.side_to_move,
            singular_extension: self.singular_extension,
            null_move: self.null_move,
            tt_move_tried: self.tt_move_tried,
            mate_threat: self.mate_threat,
        }
    }
}

impl TTProbe for TranspositionTable {
    #[inline]
    fn probe(&self, hash: u64, side_to_move: Color) -> Option<TTEntry> {
        // 明示的に固有メソッドを呼び出して可読性と誤解防止を図る
        TranspositionTable::probe_entry(self, hash, side_to_move)
    }
}

// --- Integrated PV reconstruction (moved from pv_reconstruction.rs) ---
pub trait TTProbe {
    fn probe(&self, hash: u64, side_to_move: Color) -> Option<TTEntry>;
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
        let entry = match tt.probe(hash, pos.side_to_move) {
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

#[cfg(test)]
mod tests {
    use super::{TTStoreArgs, TranspositionTable};
    use crate::search::NodeType;
    use crate::shogi::Move;
    use crate::Color;

    #[test]
    fn tt_store_probe_smoke() {
        let tt = TranspositionTable::new(4);
        let key = 0x1234_5678_90AB_CDEF;
        // Store an EXACT node with moderate depth and ensure immediate probe succeeds.
        tt.store(TTStoreArgs::new(key, None::<Move>, 42, 18, 12, NodeType::Exact, Color::Black));

        let entry = tt
            .probe_entry(key, Color::Black)
            .expect("TT probe should hit freshly stored entry");

        assert!(entry.matches(key), "stored key must round-trip");
        assert_eq!(entry.node_type(), NodeType::Exact);
        assert!(
            entry.depth() >= 12,
            "stored depth should be preserved (expected >=12, got {})",
            entry.depth()
        );
    }
}
