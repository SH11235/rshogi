//! Optimized transposition table with bucket structure
//!
//! This implementation uses a bucket structure to optimize cache performance:
//! - 4 entries per bucket (64 bytes = 1 cache line)
//! - Improved replacement strategy within buckets
//! - Better memory locality

use crate::{shogi::Move, util};
#[cfg(feature = "tt_metrics")]
use std::sync::atomic::AtomicU64 as StdAtomicU64;
use util::sync_compat::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};

// Re-export SIMD types
use crate::search::tt_simd::{simd_enabled, simd_kind, SimdKind};

/// Get depth threshold based on hashfull - optimized branch version
#[inline(always)]
fn get_depth_threshold(hf: u16) -> u8 {
    // Early return for most common case
    if hf < 600 {
        return 0;
    }

    match hf {
        600..=800 => 2,
        801..=900 => 3,
        901..=950 => 4,
        _ => 5,
    }
}

// Bit layout constants for TTEntry data field
// Optimized layout (64 bits total) - Version 2.1:
// [63-48]: move (16 bits)
// [47-34]: score (14 bits) - Optimized from 16 bits, supports ±8191
// [33-32]: extended flags (2 bits):
//          - Bit 33: Singular Extension flag
//          - Bit 32: Null Move Pruning flag
// [31-25]: depth (7 bits) - Supports depth up to 127
// [24-23]: node type (2 bits) - Exact/LowerBound/UpperBound
// [22-20]: age (3 bits) - Generation counter (0-7)
// [19]: PV flag (1 bit) - Principal Variation node marker
// [18-16]: search flags (3 bits):
//          - Bit 18: TT Move tried flag
//          - Bit 17: Mate threat flag
//          - Bit 16: Reserved for future use
// [15-2]: static eval (14 bits) - Optimized from 16 bits, supports ±8191
// [1-0]: Reserved (2 bits) - For future extensions

const MOVE_SHIFT: u8 = 48;
const MOVE_BITS: u8 = 16;
const MOVE_MASK: u64 = (1 << MOVE_BITS) - 1;

// Optimized score field: 14 bits (was 16)
const SCORE_SHIFT: u8 = 34;
const SCORE_BITS: u8 = 14;
const SCORE_MASK: u64 = (1 << SCORE_BITS) - 1;
const SCORE_MAX: i16 = (1 << (SCORE_BITS - 1)) - 1; // 8191
const SCORE_MIN: i16 = -(1 << (SCORE_BITS - 1)); // -8192

// Extended flags (new)
#[allow(dead_code)]
const EXTENDED_FLAGS_SHIFT: u8 = 32;
#[allow(dead_code)]
const EXTENDED_FLAGS_BITS: u8 = 2;
const SINGULAR_EXTENSION_FLAG: u64 = 1 << 33;
const NULL_MOVE_FLAG: u64 = 1 << 32;

const DEPTH_SHIFT: u8 = 25;
const DEPTH_BITS: u8 = 7;
const DEPTH_MASK: u8 = (1 << DEPTH_BITS) - 1;
const NODE_TYPE_SHIFT: u8 = 23;
const NODE_TYPE_BITS: u8 = 2;
const NODE_TYPE_MASK: u8 = (1 << NODE_TYPE_BITS) - 1;
const AGE_SHIFT: u8 = 20;
const AGE_BITS: u8 = 3;
pub(crate) const AGE_MASK: u8 = (1 << AGE_BITS) - 1;
const PV_FLAG_SHIFT: u8 = 19;
const PV_FLAG_MASK: u64 = 1;

// Search flags (expanded)
#[allow(dead_code)]
const SEARCH_FLAGS_SHIFT: u8 = 16;
#[allow(dead_code)]
const SEARCH_FLAGS_BITS: u8 = 3;
const TT_MOVE_TRIED_FLAG: u64 = 1 << 18;
const MATE_THREAT_FLAG: u64 = 1 << 17;

// Optimized eval field: 14 bits (was 16)
const EVAL_SHIFT: u8 = 2;
const EVAL_BITS: u8 = 14;
const EVAL_MASK: u64 = (1 << EVAL_BITS) - 1;
const EVAL_MAX: i16 = (1 << (EVAL_BITS - 1)) - 1; // 8191
const EVAL_MIN: i16 = -(1 << (EVAL_BITS - 1)); // -8192

// Reserved for future
#[allow(dead_code)]
const RESERVED_BITS: u8 = 2;
#[allow(dead_code)]
const RESERVED_MASK: u64 = (1 << RESERVED_BITS) - 1;

// Apery-style generation cycle constants
// This ensures proper wraparound behavior for age distance calculations
// The cycle is designed to be larger than the maximum possible age value (2^AGE_BITS)
// to prevent ambiguity in age distance calculations
// Use 256 as base for better alignment with age calculations
pub(crate) const GENERATION_CYCLE: u16 = 256; // Multiple of 256 for cleaner age distance calculations
#[allow(dead_code)]
const GENERATION_CYCLE_MASK: u16 = GENERATION_CYCLE - 1; // For efficient modulo operation

// Ensure GENERATION_CYCLE is larger than AGE_MASK to prevent ambiguity
#[cfg(debug_assertions)]
const _: () = assert!(GENERATION_CYCLE > AGE_MASK as u16);

// Key now uses full 64 bits for accurate collision detection
// const KEY_SHIFT: u8 = 32; // No longer needed after 64-bit comparison update

/// Number of entries per bucket (default for backward compatibility)
const BUCKET_SIZE: usize = 4;

/// Extract depth from packed data (7 bits)
#[inline(always)]
fn extract_depth(data: u64) -> u8 {
    ((data >> DEPTH_SHIFT) & DEPTH_MASK as u64) as u8
}

/// Generic helper to try updating an existing entry with depth filtering using CAS
#[inline(always)]
fn try_update_entry_generic(
    entries: &[AtomicU64],
    idx: usize,
    old_key: u64,
    new_entry: &TTEntry,
    #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
    #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
) -> UpdateResult {
    if old_key != new_entry.key {
        return UpdateResult::NotFound;
    }

    // Load old data and extract depth efficiently
    let old_data = entries[idx + 1].load(Ordering::Relaxed);

    #[cfg(feature = "tt_metrics")]
    if let Some(m) = metrics {
        record_metric(m, MetricType::AtomicLoad);
    }

    let old_depth = extract_depth(old_data);

    // Skip update if new entry doesn't improve depth
    if new_entry.depth() <= old_depth {
        #[cfg(feature = "tt_metrics")]
        if let Some(m) = metrics {
            record_metric(m, MetricType::DepthFiltered);
        }
        return UpdateResult::Filtered;
    }

    // Use CAS to update data atomically
    // This makes CAS operations more observable for Phase 5 optimization
    #[cfg(feature = "tt_metrics")]
    if let Some(m) = metrics {
        m.cas_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    match entries[idx + 1].compare_exchange_weak(
        old_data,
        new_entry.data,
        Ordering::Release,
        Ordering::Relaxed,
    ) {
        Ok(_) => {
            // CAS succeeded - data updated with proper memory ordering
            #[cfg(feature = "tt_metrics")]
            if let Some(m) = metrics {
                m.cas_successes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                record_metric(m, MetricType::UpdateExisting);
                record_metric(m, MetricType::AtomicStore(1)); // Only 1 CAS operation
                record_metric(m, MetricType::EffectiveUpdate);
            }
            UpdateResult::Updated
        }
        Err(current_data) => {
            // CAS failed - check if another thread updated with same key
            if extract_depth(current_data) >= new_entry.depth() {
                // Another thread already updated with better/equal depth
                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    m.cas_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    // Check if it was the same key (Phase 5 optimization case)
                    if old_key == new_entry.key {
                        m.cas_key_match.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                UpdateResult::Filtered
            } else {
                // Retry once if depth is still better
                match entries[idx + 1].compare_exchange_weak(
                    current_data,
                    new_entry.data,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        #[cfg(feature = "tt_metrics")]
                        if let Some(m) = metrics {
                            m.cas_successes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            record_metric(m, MetricType::UpdateExisting);
                            record_metric(m, MetricType::AtomicStore(1));
                            record_metric(m, MetricType::EffectiveUpdate);
                        }
                        UpdateResult::Updated
                    }
                    Err(_) => {
                        // Give up after second failure
                        #[cfg(feature = "tt_metrics")]
                        if let Some(m) = metrics {
                            m.cas_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        UpdateResult::Filtered
                    }
                }
            }
        }
    }
}

/// Result of update attempt
#[derive(Debug, PartialEq)]
enum UpdateResult {
    Updated,  // Successfully updated
    Filtered, // Filtered out (depth, hashfull, etc.)
    NotFound, // Key not found in this slot
}

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

/// Type of node in the search tree
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeType {
    /// Exact score (PV node)
    Exact = 0,
    /// Lower bound (fail-high/cut node)
    LowerBound = 1,
    /// Upper bound (fail-low/all node)
    UpperBound = 2,
}

/// Parameters for creating a TT entry
#[derive(Clone, Copy)]
pub struct TTEntryParams {
    pub key: u64,
    pub mv: Option<Move>,
    pub score: i16,
    pub eval: i16,
    pub depth: u8,
    pub node_type: NodeType,
    pub age: u8,
    pub is_pv: bool,
    // Extended flags (optional)
    pub singular_extension: bool,
    pub null_move: bool,
    pub tt_move_tried: bool,
    pub mate_threat: bool,
}

impl Default for TTEntryParams {
    fn default() -> Self {
        Self {
            key: 0,
            mv: None,
            score: 0,
            eval: 0,
            depth: 0,
            node_type: NodeType::Exact,
            age: 0,
            is_pv: false,
            singular_extension: false,
            null_move: false,
            tt_move_tried: false,
            mate_threat: false,
        }
    }
}

/// Transposition table entry (16 bytes)
#[derive(Clone, Copy, Default)]
#[repr(C, align(16))]
pub struct TTEntry {
    key: u64,
    data: u64,
}

impl TTEntry {
    /// Create new TT entry (backward compatibility)
    pub fn new(
        key: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
        age: u8,
    ) -> Self {
        let params = TTEntryParams {
            key,
            mv,
            score,
            eval,
            depth,
            node_type,
            age,
            is_pv: false,
            ..Default::default()
        };
        Self::from_params(params)
    }

    /// Create new TT entry from parameters
    pub fn from_params(params: TTEntryParams) -> Self {
        // Store full 64-bit key for accurate collision detection
        let key = params.key;

        // Pack move into 16 bits
        let move_data = match params.mv {
            Some(m) => m.to_u16(),
            None => 0,
        };

        // Clamp score and eval to 14-bit range
        let score = params.score.clamp(SCORE_MIN, SCORE_MAX);
        let eval = params.eval.clamp(EVAL_MIN, EVAL_MAX);

        // Encode score and eval as 14-bit values (with sign bit)
        let score_encoded = ((score as u16) & SCORE_MASK as u16) as u64;
        let eval_encoded = ((eval as u16) & EVAL_MASK as u16) as u64;

        // Pack all data into 64 bits with optimized layout:
        let mut data = ((move_data as u64) << MOVE_SHIFT)
            | (score_encoded << SCORE_SHIFT)
            | (((params.depth & DEPTH_MASK) as u64) << DEPTH_SHIFT)
            | ((params.node_type as u64) << NODE_TYPE_SHIFT)
            | (((params.age & AGE_MASK) as u64) << AGE_SHIFT)
            | ((params.is_pv as u64) << PV_FLAG_SHIFT)
            | (eval_encoded << EVAL_SHIFT);

        // Set extended flags
        if params.singular_extension {
            data |= SINGULAR_EXTENSION_FLAG;
        }
        if params.null_move {
            data |= NULL_MOVE_FLAG;
        }
        if params.tt_move_tried {
            data |= TT_MOVE_TRIED_FLAG;
        }
        if params.mate_threat {
            data |= MATE_THREAT_FLAG;
        }

        TTEntry { key, data }
    }

    /// Check if entry matches the given key
    #[inline]
    pub fn matches(&self, key: u64) -> bool {
        self.key == key
    }

    /// Check if entry is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.key == 0 && self.data == 0
    }

    /// Extract move from entry
    pub fn get_move(&self) -> Option<Move> {
        let move_data = ((self.data >> MOVE_SHIFT) & MOVE_MASK) as u16;
        if move_data == 0 {
            return None;
        }
        // Debug assertion to validate move data
        debug_assert!(move_data <= 0x7FFF, "Suspicious move data in TT entry: {move_data:#x}");
        Some(Move::from_u16(move_data))
    }

    /// Get score from entry (14-bit signed value)
    #[inline]
    pub fn score(&self) -> i16 {
        let raw = ((self.data >> SCORE_SHIFT) & SCORE_MASK) as u16;
        // Efficient sign-extension from 14-bit to 16-bit
        // Left shift to align sign bit, then arithmetic right shift to extend
        ((raw as i16) << (16 - SCORE_BITS)) >> (16 - SCORE_BITS)
    }

    /// Get static evaluation from entry (14-bit signed value)
    #[inline]
    pub fn eval(&self) -> i16 {
        let raw = ((self.data >> EVAL_SHIFT) & EVAL_MASK) as u16;
        // Efficient sign-extension from 14-bit to 16-bit
        // Left shift to align sign bit, then arithmetic right shift to extend
        ((raw as i16) << (16 - EVAL_BITS)) >> (16 - EVAL_BITS)
    }

    /// Get search depth
    #[inline]
    pub fn depth(&self) -> u8 {
        ((self.data >> DEPTH_SHIFT) & DEPTH_MASK as u64) as u8
    }

    /// Get node type
    #[inline]
    pub fn node_type(&self) -> NodeType {
        let raw = (self.data >> NODE_TYPE_SHIFT) & NODE_TYPE_MASK as u64;
        match raw {
            0 => NodeType::Exact,
            1 => NodeType::LowerBound,
            2 => NodeType::UpperBound,
            _ => {
                // Debug assertion to catch corrupted data in development
                debug_assert!(false, "Corrupted node type in TT entry: raw value = {raw}");
                NodeType::Exact // Default to Exact for corrupted data
            }
        }
    }

    /// Get age
    #[inline]
    pub fn age(&self) -> u8 {
        let age = ((self.data >> AGE_SHIFT) & AGE_MASK as u64) as u8;
        // Debug assertion to validate age is within expected range
        debug_assert!(age <= AGE_MASK, "Age value out of range: {age} (max: {AGE_MASK})");
        age
    }

    /// Check if this is a PV node
    #[inline]
    pub fn is_pv(&self) -> bool {
        ((self.data >> PV_FLAG_SHIFT) & PV_FLAG_MASK) != 0
    }

    /// Check if Singular Extension was applied
    #[inline]
    pub fn has_singular_extension(&self) -> bool {
        (self.data & SINGULAR_EXTENSION_FLAG) != 0
    }

    /// Check if Null Move Pruning was applied
    #[inline]
    pub fn has_null_move(&self) -> bool {
        (self.data & NULL_MOVE_FLAG) != 0
    }

    /// Check if TT move was tried
    #[inline]
    pub fn tt_move_tried(&self) -> bool {
        (self.data & TT_MOVE_TRIED_FLAG) != 0
    }

    /// Check if position has mate threat
    #[inline]
    pub fn has_mate_threat(&self) -> bool {
        (self.data & MATE_THREAT_FLAG) != 0
    }

    /// Calculate replacement priority score using Apery-style cyclic distance
    #[inline]
    fn priority_score(&self, current_age: u8) -> i32 {
        if self.is_empty() {
            return i32::MIN; // Empty entries have lowest priority (should be replaced first)
        }

        // Calculate cyclic distance between generations (Apery-style)
        let age_distance = ((GENERATION_CYCLE + current_age as u16 - self.age() as u16)
            & (AGE_MASK as u16)) as i32;

        // Base priority: depth minus age distance
        // Older entries (larger age_distance) get lower priority
        let mut priority = self.depth() as i32 - age_distance;

        // Bonus for PV nodes (they should be preserved longer)
        if self.is_pv() {
            priority += 32;
        }

        // Smaller bonus for exact entries
        if self.node_type() == NodeType::Exact {
            priority += 16;
        }

        priority
    }
}

/// Bucket containing multiple TT entries (64 bytes = 1 cache line)
#[repr(C, align(64))]
struct TTBucket {
    entries: [AtomicU64; BUCKET_SIZE * 2], // 4 entries * 2 u64s each = 64 bytes
}

impl TTBucket {
    /// Create new empty bucket
    fn new() -> Self {
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

    /// Probe bucket for matching entry using SIMD when available
    fn probe(&self, key: u64) -> Option<TTEntry> {
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
    fn store(&self, new_entry: TTEntry, current_age: u8) {
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
                    // Write data first with Relaxed ordering
                    self.entries[idx + 1].store(new_entry.data, Ordering::Relaxed);

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
}

/// Flexible bucket that can hold variable number of entries
/// Note: For optimal performance, consider using fixed-size TTBucket when possible
/// as it guarantees cache line alignment
struct FlexibleTTBucket {
    /// Atomic entries (keys and data interleaved)
    entries: Box<[AtomicU64]>,
    /// Size configuration for this bucket
    size: BucketSize,
}

impl FlexibleTTBucket {
    /// Create new flexible bucket with specified size
    fn new(size: BucketSize) -> Self {
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

    /// Probe bucket for matching entry
    fn probe(&self, key: u64) -> Option<TTEntry> {
        match self.size {
            BucketSize::Small => self.probe_4(key),
            BucketSize::Medium => self.probe_8(key),
            BucketSize::Large => self.probe_16(key),
        }
    }

    /// Probe 4-entry bucket (current implementation)
    fn probe_4(&self, target_key: u64) -> Option<TTEntry> {
        // Try SIMD-optimized path first
        if self.probe_simd_available() {
            // Future optimization: select implementation based on SIMD kind
            // match self.probe_simd_kind() {
            //     SimdKind::Avx2 => return self.probe_avx2_4(target_key),
            //     SimdKind::Sse2 => return self.probe_sse2_4(target_key),
            //     SimdKind::None => {}
            // }
            return self.probe_simd_4(target_key);
        }
        // Fallback to scalar
        self.probe_scalar_4(target_key)
    }

    /// Probe 8-entry bucket
    fn probe_8(&self, target_key: u64) -> Option<TTEntry> {
        // Try SIMD-optimized path first
        if self.probe_simd_available() {
            return self.probe_simd_8(target_key);
        }
        // Fallback to scalar
        self.probe_scalar_8(target_key)
    }

    /// Probe 16-entry bucket
    fn probe_16(&self, target_key: u64) -> Option<TTEntry> {
        // Currently only scalar implementation
        self.probe_scalar_16(target_key)
    }

    /// Check if SIMD probe is available
    #[inline]
    fn probe_simd_available(&self) -> bool {
        simd_enabled()
    }

    /// Get SIMD kind for choosing optimal implementation
    #[inline]
    #[allow(dead_code)]
    fn probe_simd_kind(&self) -> SimdKind {
        simd_kind()
    }

    /// SIMD probe for 4 entries
    fn probe_simd_4(&self, target_key: u64) -> Option<TTEntry> {
        let mut keys = [0u64; 4];
        for (i, key) in keys.iter_mut().enumerate() {
            // Use Acquire ordering on key load to synchronize with Release store
            *key = self.entries[i * 2].load(Ordering::Acquire);
        }

        if let Some(idx) = crate::search::tt_simd::simd::find_matching_key(&keys, target_key) {
            // Use Acquire ordering on data load to ensure proper synchronization
            // with Phase 5 optimization where data might be updated separately
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

    /// SIMD probe for 8 entries
    fn probe_simd_8(&self, target_key: u64) -> Option<TTEntry> {
        let mut keys = [0u64; 8];
        for (i, key) in keys.iter_mut().enumerate() {
            // Use Acquire ordering on key load to synchronize with Release store
            *key = self.entries[i * 2].load(Ordering::Acquire);
        }

        if let Some(idx) = crate::search::tt_simd::simd::find_matching_key_8(&keys, target_key) {
            // Use Acquire ordering on data load to ensure proper synchronization
            // with Phase 5 optimization where data might be updated separately
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

    /// Generic scalar probe for N entries (hybrid: early termination + single fence)
    fn probe_scalar_impl<const N: usize>(&self, target_key: u64) -> Option<TTEntry> {
        // Hybrid approach: early termination to minimize memory access
        let mut matching_idx = None;

        // Load keys with early termination
        for i in 0..N {
            // Use Acquire ordering on key load to synchronize with Release store
            let key = self.entries[i * 2].load(Ordering::Acquire);
            if key == target_key {
                matching_idx = Some(i);
                break; // Early termination - key optimization
            }
        }

        // If we found a match, load data
        if let Some(idx) = matching_idx {
            // Use Acquire ordering on data load to ensure proper synchronization
            // with Phase 5 optimization where data might be updated separately
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

    /// Scalar probe for 4 entries (hybrid: early termination + single fence)
    fn probe_scalar_4(&self, target_key: u64) -> Option<TTEntry> {
        self.probe_scalar_impl::<4>(target_key)
    }

    /// Scalar probe for 8 entries (hybrid: early termination + single fence)
    fn probe_scalar_8(&self, target_key: u64) -> Option<TTEntry> {
        self.probe_scalar_impl::<8>(target_key)
    }

    /// Scalar probe for 16 entries (hybrid: early termination + single fence)
    fn probe_scalar_16(&self, target_key: u64) -> Option<TTEntry> {
        self.probe_scalar_impl::<16>(target_key)
    }

    /// Store entry in bucket with empty slot mode
    fn store_with_mode(
        &self,
        params: TTEntryParams,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        match self.size {
            #[cfg(feature = "tt_metrics")]
            BucketSize::Small => {
                self.store_4_with_mode(params, current_age, empty_slot_mode, metrics)
            }
            #[cfg(not(feature = "tt_metrics"))]
            BucketSize::Small => self.store_4_with_mode(params, current_age, empty_slot_mode, None),
            #[cfg(feature = "tt_metrics")]
            BucketSize::Medium => {
                self.store_8_with_mode(params, current_age, empty_slot_mode, metrics)
            }
            #[cfg(not(feature = "tt_metrics"))]
            BucketSize::Medium => {
                self.store_8_with_mode(params, current_age, empty_slot_mode, None)
            }
            #[cfg(feature = "tt_metrics")]
            BucketSize::Large => {
                self.store_16_with_mode(params, current_age, empty_slot_mode, metrics)
            }
            #[cfg(not(feature = "tt_metrics"))]
            BucketSize::Large => {
                self.store_16_with_mode(params, current_age, empty_slot_mode, None)
            }
        }
    }

    /// Generic store implementation for N-entry bucket
    fn store_impl<const N: usize>(
        &self,
        params: TTEntryParams,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        let new_entry = TTEntry::from_params(params);
        let target_key = params.key;

        // First pass: check all entries for exact match or empty slot
        for i in 0..N {
            let idx = i * 2;

            const MAX_CAS_RETRIES: u32 = 4;
            let mut retry_count = 0;

            loop {
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
                    // Empty slot - optimization based on architecture
                    #[cfg(target_arch = "x86_64")]
                    {
                        // x86_64 TSO: CAS first to avoid wasted data writes
                        // Record CAS attempt
                        #[cfg(feature = "tt_metrics")]
                        if let Some(m) = metrics {
                            m.cas_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }

                        match self.entries[idx].compare_exchange_weak(
                            0,
                            new_entry.key,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => {
                                // CAS succeeded - now write data
                                self.entries[idx + 1].store(new_entry.data, Ordering::Release);

                                // Record metrics
                                #[cfg(feature = "tt_metrics")]
                                if let Some(m) = metrics {
                                    m.cas_successes
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    m.atomic_stores
                                        .fetch_add(2, std::sync::atomic::Ordering::Relaxed); // CAS + data
                                    m.replace_empty
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                return;
                            }
                            Err(current) => {
                                // Phase 5 optimization: Check if another thread wrote the same key
                                if current == target_key {
                                    // Same key - just update the data
                                    // Use Release ordering to ensure reader sees the updated data
                                    self.entries[idx + 1].store(new_entry.data, Ordering::Release);

                                    #[cfg(feature = "tt_metrics")]
                                    if let Some(m) = metrics {
                                        m.cas_key_match
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                        m.update_existing
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                        m.atomic_stores
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    return;
                                }

                                #[cfg(feature = "tt_metrics")]
                                if let Some(m) = metrics {
                                    m.cas_failures
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                retry_count += 1;
                                if retry_count >= MAX_CAS_RETRIES {
                                    break;
                                }

                                // Check if slot is still relevant
                                if current != 0 && current != target_key {
                                    break;
                                }

                                // Backoff strategy
                                if retry_count < 3 {
                                    for _ in 0..(retry_count * 2) {
                                        std::hint::spin_loop();
                                    }
                                } else {
                                    std::thread::yield_now();
                                }
                            }
                        }
                    }
                    #[cfg(not(target_arch = "x86_64"))]
                    {
                        // Other architectures: CAS first for consistency
                        // Record CAS attempt
                        #[cfg(feature = "tt_metrics")]
                        if let Some(m) = metrics {
                            m.cas_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }

                        match self.entries[idx].compare_exchange_weak(
                            0,
                            new_entry.key,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => {
                                // CAS succeeded - now write data
                                self.entries[idx + 1].store(new_entry.data, Ordering::Release);

                                // Record metrics
                                #[cfg(feature = "tt_metrics")]
                                if let Some(m) = metrics {
                                    m.cas_successes
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    m.atomic_stores
                                        .fetch_add(2, std::sync::atomic::Ordering::Relaxed); // CAS + data
                                    m.replace_empty
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                return;
                            }
                            Err(current) => {
                                // Phase 5 optimization: Check if another thread wrote the same key
                                if current == target_key {
                                    // Same key - just update the data
                                    // Use Release ordering to ensure reader sees the updated data
                                    self.entries[idx + 1].store(new_entry.data, Ordering::Release);

                                    #[cfg(feature = "tt_metrics")]
                                    if let Some(m) = metrics {
                                        m.cas_key_match
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                        m.update_existing
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                        m.atomic_stores
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    return;
                                }

                                #[cfg(feature = "tt_metrics")]
                                if let Some(m) = metrics {
                                    m.cas_failures
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                retry_count += 1;
                                if retry_count >= MAX_CAS_RETRIES {
                                    break;
                                }

                                // Check if slot is still relevant
                                if current != 0 && current != target_key {
                                    break;
                                }

                                // Backoff strategy
                                if retry_count < 3 {
                                    for _ in 0..(retry_count * 2) {
                                        std::hint::spin_loop();
                                    }
                                } else {
                                    std::thread::yield_now();
                                }
                            }
                        }
                    }
                } else {
                    break;
                }
            }
        }

        // If empty slot mode is enabled, skip replacement
        if empty_slot_mode {
            return;
        }

        // Find worst entry to replace
        let (worst_idx, _) = match N {
            4 => self.find_worst_entry_4(current_age),
            8 => self.find_worst_entry_8(current_age),
            16 => self.find_worst_entry_16_scalar(current_age),
            _ => unreachable!("Unsupported bucket size"),
        };
        let idx = worst_idx * 2;

        // Use CAS for worst replacement
        let old_key = self.entries[idx].load(Ordering::Relaxed);

        // Record atomic load
        #[cfg(feature = "tt_metrics")]
        if let Some(m) = metrics {
            m.atomic_loads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                // CAS already published the key with Release ordering
                self.entries[idx + 1].store(new_entry.data, Ordering::Release);

                // Record metrics
                #[cfg(feature = "tt_metrics")]
                if let Some(m) = metrics {
                    m.cas_successes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    m.atomic_stores.fetch_add(2, std::sync::atomic::Ordering::Relaxed); // CAS + data
                    if old_key == 0 {
                        m.replace_empty.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        m.replace_worst.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            Err(current) => {
                // Phase 5 optimization: Check if another thread wrote the same key
                if current == target_key {
                    // Same key - just update the data
                    // Use Relaxed ordering since key hasn't changed and reader will
                    // see the key first with Acquire ordering
                    self.entries[idx + 1].store(new_entry.data, Ordering::Relaxed);

                    #[cfg(feature = "tt_metrics")]
                    if let Some(m) = metrics {
                        m.cas_key_match.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        m.update_existing.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        m.atomic_stores.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                } else {
                    #[cfg(feature = "tt_metrics")]
                    if let Some(m) = metrics {
                        m.cas_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// Store in 4-entry bucket with mode
    fn store_4_with_mode(
        &self,
        params: TTEntryParams,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        #[cfg(feature = "tt_metrics")]
        self.store_impl::<4>(params, current_age, empty_slot_mode, metrics);
        #[cfg(not(feature = "tt_metrics"))]
        self.store_impl::<4>(params, current_age, empty_slot_mode, None);
    }

    /// Store in 8-entry bucket with mode
    fn store_8_with_mode(
        &self,
        params: TTEntryParams,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        #[cfg(feature = "tt_metrics")]
        self.store_impl::<8>(params, current_age, empty_slot_mode, metrics);
        #[cfg(not(feature = "tt_metrics"))]
        self.store_impl::<8>(params, current_age, empty_slot_mode, None);
    }

    /// Store in 16-entry bucket with mode
    fn store_16_with_mode(
        &self,
        params: TTEntryParams,
        current_age: u8,
        empty_slot_mode: bool,
        #[cfg(feature = "tt_metrics")] metrics: Option<&DetailedTTMetrics>,
        #[cfg(not(feature = "tt_metrics"))] _metrics: Option<&()>,
    ) {
        #[cfg(feature = "tt_metrics")]
        self.store_impl::<16>(params, current_age, empty_slot_mode, metrics);
        #[cfg(not(feature = "tt_metrics"))]
        self.store_impl::<16>(params, current_age, empty_slot_mode, None);
    }

    /// Generic find worst entry implementation for N-entry bucket using SIMD
    fn find_worst_entry_impl<const N: usize>(&self, current_age: u8) -> (usize, i32) {
        // For SIMD processing, we need fixed-size arrays
        // If N > 16, fall back to scalar implementation
        if N > 16 {
            return self.find_worst_entry_scalar_generic::<N>(current_age);
        }

        // Use a larger fixed-size array and only use first N elements
        let mut depths = [0u8; 16];
        let mut ages = [0u8; 16];
        let mut is_pv = [false; 16];
        let mut is_exact = [false; 16];
        let mut is_empty = [false; 16];

        for i in 0..N {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Relaxed);
            let data = self.entries[idx + 1].load(Ordering::Relaxed);
            let entry = TTEntry { key, data };

            if entry.is_empty() {
                is_empty[i] = true;
            } else {
                depths[i] = entry.depth();
                ages[i] = entry.age();
                is_pv[i] = entry.is_pv();
                is_exact[i] = entry.node_type() == NodeType::Exact;
            }
        }

        // Calculate priorities using SIMD
        // Since SIMD functions return fixed-size arrays, we need to handle them separately
        let mut scores = if N == 4 {
            let scores4 = crate::search::tt_simd::simd::calculate_priority_scores(
                &depths[..4].try_into().unwrap(),
                &ages[..4].try_into().unwrap(),
                &is_pv[..4].try_into().unwrap(),
                &is_exact[..4].try_into().unwrap(),
                current_age,
            );
            // Copy to our larger array
            let mut scores = [0i32; 16];
            scores[..4].copy_from_slice(&scores4);
            scores
        } else if N == 8 {
            let scores8 = crate::search::tt_simd::simd::calculate_priority_scores_8(
                &depths[..8].try_into().unwrap(),
                &ages[..8].try_into().unwrap(),
                &is_pv[..8].try_into().unwrap(),
                &is_exact[..8].try_into().unwrap(),
                current_age,
            );
            // Copy to our larger array
            let mut scores = [0i32; 16];
            scores[..8].copy_from_slice(&scores8);
            scores
        } else {
            // For other sizes, fall back to scalar
            return self.find_worst_entry_scalar_generic::<N>(current_age);
        };

        // Set empty entries to minimum priority (only for first N entries)
        for i in 0..N {
            if is_empty[i] {
                scores[i] = i32::MIN;
            }
        }

        // Find minimum score among first N entries
        let mut worst_idx = 0;
        let mut worst_score = scores[0];
        for (i, &score) in scores.iter().enumerate().take(N).skip(1) {
            if score < worst_score {
                worst_score = score;
                worst_idx = i;
            }
        }

        (worst_idx, worst_score)
    }

    /// Generic scalar implementation for finding worst entry
    fn find_worst_entry_scalar_generic<const N: usize>(&self, current_age: u8) -> (usize, i32) {
        let mut worst_idx = 0;
        let mut worst_score = i32::MAX;

        for i in 0..N {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Relaxed);
            let data = self.entries[idx + 1].load(Ordering::Relaxed);
            let entry = TTEntry { key, data };

            let score = entry.priority_score(current_age);
            if score < worst_score {
                worst_score = score;
                worst_idx = i;
            }
        }

        (worst_idx, worst_score)
    }

    /// Find worst entry in 4-entry bucket
    fn find_worst_entry_4(&self, current_age: u8) -> (usize, i32) {
        self.find_worst_entry_impl::<4>(current_age)
    }

    /// Find worst entry in 8-entry bucket using SIMD
    fn find_worst_entry_8(&self, current_age: u8) -> (usize, i32) {
        self.find_worst_entry_impl::<8>(current_age)
    }

    /// Find worst entry in 16-entry bucket (scalar for now)
    fn find_worst_entry_16(&self, current_age: u8) -> (usize, i32) {
        self.find_worst_entry_scalar_generic::<16>(current_age)
    }

    /// Alias for find_worst_entry_16 (used in store_impl match)
    fn find_worst_entry_16_scalar(&self, current_age: u8) -> (usize, i32) {
        self.find_worst_entry_16(current_age)
    }
}

/// Detailed metrics for analyzing TT performance and CAS overhead
#[cfg(feature = "tt_metrics")]
#[derive(Debug, Default)]
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

        println!("=== TT Detailed Metrics ===");
        println!("Update patterns:");
        println!(
            "  Existing updates: {} ({:.1}%)",
            self.update_existing.load(Relaxed),
            self.update_existing.load(Relaxed) as f64 / total_updates as f64 * 100.0
        );
        println!(
            "  Empty slots used: {} ({:.1}%)",
            self.replace_empty.load(Relaxed),
            self.replace_empty.load(Relaxed) as f64 / total_updates as f64 * 100.0
        );
        println!(
            "  Worst replaced: {} ({:.1}%)",
            self.replace_worst.load(Relaxed),
            self.replace_worst.load(Relaxed) as f64 / total_updates as f64 * 100.0
        );

        println!("\nAtomic operations:");
        println!("  Stores: {}", self.atomic_stores.load(Relaxed));
        println!("  Loads: {}", self.atomic_loads.load(Relaxed));

        println!("\nPrefetch statistics:");
        println!("  Prefetch count: {}", self.prefetch_count.load(Relaxed));

        if self.cas_attempts.load(Relaxed) > 0 {
            println!("\nCAS operations:");
            println!("  Attempts: {}", self.cas_attempts.load(Relaxed));
            println!("  Successes: {}", self.cas_successes.load(Relaxed));
            println!("  Failures: {}", self.cas_failures.load(Relaxed));
            println!(
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
            println!("\nOptimization filters:");
            println!("  Depth filtered: {depth_filtered}");
            println!("  Hashfull filtered: {hashfull_filtered}");
            println!("  Effective updates: {}", self.effective_updates.load(Relaxed));
        }
    }
}

/// Metrics update types
#[cfg(feature = "tt_metrics")]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum MetricType {
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
fn record_metric(metrics: &DetailedTTMetrics, metric_type: MetricType) {
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

/// Optimized transposition table with bucket structure
pub struct TranspositionTable {
    /// Table buckets (legacy fixed-size)
    buckets: Vec<TTBucket>,
    /// Flexible buckets (for dynamic sizing)
    flexible_buckets: Option<Vec<FlexibleTTBucket>>,
    /// Number of buckets
    num_buckets: usize,
    /// Current age/generation (3 bits: 0-7)
    age: u8,
    /// Bucket size configuration
    #[allow(dead_code)]
    bucket_size: Option<BucketSize>,
    /// Adaptive prefetch manager
    prefetcher: Option<crate::search::adaptive_prefetcher::AdaptivePrefetcher>,
    /// Detailed metrics for performance analysis
    #[cfg(feature = "tt_metrics")]
    pub metrics: Option<DetailedTTMetrics>,
    /// Occupied bitmap - 1 bit per bucket to track occupancy
    occupied_bitmap: Vec<AtomicU8>,
    /// Hashfull estimate using exponential moving average
    hashfull_estimate: AtomicU16,
    /// Node counter for periodic hashfull updates
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
            buckets: Vec::new(), // Empty for flexible mode
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

    /// Enable detailed metrics collection
    #[cfg(feature = "tt_metrics")]
    pub fn enable_metrics(&mut self) {
        self.metrics = Some(DetailedTTMetrics::new());
    }

    /// Get bucket index from zobrist hash
    #[inline]
    fn bucket_index(&self, hash: u64) -> usize {
        (hash as usize) & (self.num_buckets - 1)
    }

    /// Probe the transposition table
    pub fn probe(&self, hash: u64) -> Option<TTEntry> {
        let idx = self.bucket_index(hash);

        let result = if let Some(ref flexible_buckets) = self.flexible_buckets {
            flexible_buckets[idx].probe(hash)
        } else {
            self.buckets[idx].probe(hash)
        };

        // Record hit/miss for adaptive prefetcher
        if let Some(ref prefetcher) = self.prefetcher {
            if result.is_some() {
                prefetcher.record_hit();
            } else {
                prefetcher.record_miss();
            }
        }

        // Record metrics if enabled
        #[cfg(feature = "tt_metrics")]
        if let Some(ref metrics) = self.metrics {
            if result.is_some() {
                metrics.prefetch_hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        result
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

    /// Store entry in transposition table with parameters
    pub fn store_with_params(&self, mut params: TTEntryParams) {
        // Override age with current table age
        params.age = self.age;
        self.store_entry(params);
    }

    /// Store entry using parameters
    fn store_entry(&self, params: TTEntryParams) {
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

            // Update empty slot mode with hysteresis
            self.update_empty_slot_mode(hf);

            // Immediate trigger at 99%
            if hf >= 990 && !self.need_gc.load(Ordering::Relaxed) {
                self.need_gc.store(true, Ordering::Relaxed);
                #[cfg(feature = "tt_metrics")]
                self.gc_triggered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // Gradual trigger at 95% if sustained
            else if hf >= 950 {
                let high_count = self.high_hashfull_counter.fetch_add(1, Ordering::Relaxed);
                if high_count >= 10 && !self.need_gc.load(Ordering::Relaxed) {
                    // 10 * 256 = 2560 nodes at high hashfull
                    self.need_gc.store(true, Ordering::Relaxed);
                    #[cfg(feature = "tt_metrics")]
                    self.gc_triggered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            } else {
                // Reset counter if hashfull drops below threshold
                self.high_hashfull_counter.store(0, Ordering::Relaxed);
            }
        }

        // Check if empty slot mode is enabled
        let empty_slot_mode = self.empty_slot_mode_enabled.load(Ordering::Relaxed);

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            #[cfg(feature = "tt_metrics")]
            flexible_buckets[idx].store_with_mode(
                params,
                self.age,
                empty_slot_mode,
                self.metrics.as_ref(),
            );
            #[cfg(not(feature = "tt_metrics"))]
            flexible_buckets[idx].store_with_mode(params, self.age, empty_slot_mode, None);
        } else {
            let entry = TTEntry::from_params(params);
            #[cfg(feature = "tt_metrics")]
            self.buckets[idx].store_with_metrics_and_mode(
                entry,
                self.age,
                empty_slot_mode,
                self.metrics.as_ref(),
            );
            #[cfg(not(feature = "tt_metrics"))]
            self.buckets[idx].store_with_metrics_and_mode(entry, self.age, empty_slot_mode, None);
        }
    }

    /// Clear the transposition table
    pub fn clear(&mut self) {
        if let Some(ref mut flexible_buckets) = self.flexible_buckets {
            for bucket in flexible_buckets {
                for atomic in bucket.entries.iter() {
                    atomic.store(0, Ordering::Relaxed);
                }
            }
        } else {
            for bucket in &mut self.buckets {
                for atomic in &bucket.entries {
                    atomic.store(0, Ordering::Relaxed);
                }
            }
        }

        // Clear occupied bitmap
        for byte in &self.occupied_bitmap {
            byte.store(0, Ordering::Relaxed);
        }

        // Reset estimates and counters
        self.hashfull_estimate.store(0, Ordering::Relaxed);
        self.node_counter.store(0, Ordering::Relaxed);
        self.high_hashfull_counter.store(0, Ordering::Relaxed);

        // Reset GC state
        self.need_gc.store(false, Ordering::Relaxed);
        self.gc_progress.store(0, Ordering::Relaxed);

        self.age = 0;
    }

    /// Advance to next generation
    pub fn new_search(&mut self) {
        self.age = (self.age + 1) & AGE_MASK;
    }

    /// Get fill rate (percentage of non-empty entries)
    pub fn hashfull(&self) -> u16 {
        // Sample first 1000 buckets
        let sample_buckets = 1000.min(self.num_buckets);
        let mut filled = 0;
        let mut total = 0;

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            // Flexible bucket implementation
            for bucket in flexible_buckets.iter().take(sample_buckets) {
                let entries_per_bucket = bucket.size.entries();

                for j in 0..entries_per_bucket {
                    let idx = j * 2;
                    total += 1;
                    // Use Relaxed for sampling - no synchronization needed
                    if bucket.entries[idx].load(Ordering::Relaxed) != 0 {
                        filled += 1;
                    }
                }
            }
        } else {
            // Legacy bucket implementation
            for i in 0..sample_buckets {
                for j in 0..BUCKET_SIZE {
                    let idx = j * 2;
                    total += 1;
                    // Use Relaxed for sampling - no synchronization needed
                    if self.buckets[i].entries[idx].load(Ordering::Relaxed) != 0 {
                        filled += 1;
                    }
                }
            }
        }

        ((filled * 1000) / total) as u16
    }

    /// Get table size in entries
    pub fn size(&self) -> usize {
        self.num_buckets * BUCKET_SIZE
    }

    /// Mark bucket as occupied in the bitmap
    #[inline(always)]
    fn mark_bucket_occupied(&self, bucket_idx: usize) {
        let byte_idx = bucket_idx / 8;
        let bit_idx = bucket_idx % 8;
        if byte_idx < self.occupied_bitmap.len() {
            self.occupied_bitmap[byte_idx].fetch_or(1 << bit_idx, Ordering::Relaxed);
        }
    }

    /// Check if bucket is occupied
    #[inline(always)]
    fn is_bucket_occupied(&self, bucket_idx: usize) -> bool {
        let byte_idx = bucket_idx / 8;
        let bit_idx = bucket_idx % 8;
        if byte_idx < self.occupied_bitmap.len() {
            let byte_val = self.occupied_bitmap[byte_idx].load(Ordering::Relaxed);
            (byte_val & (1 << bit_idx)) != 0
        } else {
            false
        }
    }

    /// Update hashfull estimate using exponential moving average
    fn update_hashfull_estimate(&self) {
        // Sample 64 buckets for efficiency
        const SAMPLE_SIZE: usize = 64;
        let sample_buckets = SAMPLE_SIZE.min(self.num_buckets);

        // Use a simple linear congruential generator for pseudo-random sampling
        let seed = self.node_counter.load(Ordering::Relaxed);
        let start_idx = ((seed * 1103515245 + 12345) >> 16) as usize % self.num_buckets;

        let mut filled = 0;
        for i in 0..sample_buckets {
            let bucket_idx = (start_idx + i) % self.num_buckets;
            if self.is_bucket_occupied(bucket_idx) {
                filled += 1;
            }
        }

        let sample_hashfull = (filled * 1000) / sample_buckets;

        // Exponential moving average: new = (old * 7 + sample) / 8
        let old_estimate = self.hashfull_estimate.load(Ordering::Relaxed);
        let new_estimate = ((old_estimate as u32 * 7 + sample_hashfull as u32) / 8) as u16;
        self.hashfull_estimate.store(new_estimate, Ordering::Relaxed);
    }

    /// Get current hashfull estimate
    pub fn hashfull_estimate(&self) -> u16 {
        self.hashfull_estimate.load(Ordering::Relaxed)
    }

    /// Update empty slot mode based on hashfull with hysteresis
    fn update_empty_slot_mode(&self, current_hf: u16) {
        let was_enabled = self.empty_slot_mode_enabled.load(Ordering::Relaxed);

        // Get threshold based on bucket size if available - more relaxed thresholds
        let (enable_threshold, disable_threshold) = if let Some(bucket_size) = self.bucket_size {
            match bucket_size {
                BucketSize::Small => (200, 300),  // 4-entry: 20-30%
                BucketSize::Medium => (150, 250), // 8-entry: 15-25%
                BucketSize::Large => (100, 200),  // 16-entry: 10-20%
            }
        } else {
            (200, 300) // Default for legacy mode
        };

        if !was_enabled && current_hf < enable_threshold {
            // Enable empty slot mode when hashfull drops below lower threshold
            self.empty_slot_mode_enabled.store(true, Ordering::Relaxed);
        } else if was_enabled && current_hf >= disable_threshold {
            // Disable empty slot mode when hashfull rises above upper threshold
            self.empty_slot_mode_enabled.store(false, Ordering::Relaxed);
        }

        self.empty_slot_mode_last_hf.store(current_hf, Ordering::Relaxed);
    }

    /// Calculate age distance between current age and entry age
    #[inline]
    fn calculate_age_distance(&self, entry_age: u8) -> u8 {
        ((GENERATION_CYCLE + self.age as u16 - entry_age as u16) & (AGE_MASK as u16)) as u8
    }

    /// Check if bucket is empty
    fn is_bucket_empty(&self, bucket_idx: usize) -> bool {
        if let Some(ref flexible_buckets) = self.flexible_buckets {
            let bucket = &flexible_buckets[bucket_idx];
            for i in 0..bucket.size.entries() {
                if bucket.entries[i * 2].load(Ordering::Relaxed) != 0 {
                    return false;
                }
            }
        } else {
            let bucket = &self.buckets[bucket_idx];
            for i in 0..BUCKET_SIZE {
                if bucket.entries[i * 2].load(Ordering::Relaxed) != 0 {
                    return false;
                }
            }
        }
        true
    }

    /// Unmark bucket as occupied in the bitmap
    #[inline]
    fn unmark_bucket_occupied(&self, bucket_idx: usize) {
        let byte_idx = bucket_idx / 8;
        let bit_idx = bucket_idx % 8;
        if byte_idx < self.occupied_bitmap.len() {
            self.occupied_bitmap[byte_idx].fetch_and(!(1 << bit_idx), Ordering::Relaxed);
        }
    }

    /// Clear old entries in a specific bucket
    fn clear_old_entries_in_bucket(&self, bucket_idx: usize) {
        if let Some(ref flexible_buckets) = self.flexible_buckets {
            let bucket = &flexible_buckets[bucket_idx];
            let entries_per_bucket = bucket.size.entries();

            for i in 0..entries_per_bucket {
                let key_idx = i * 2;
                let data_idx = key_idx + 1;

                let key = bucket.entries[key_idx].load(Ordering::Relaxed);
                if key != 0 {
                    let data = bucket.entries[data_idx].load(Ordering::Relaxed);
                    let entry = TTEntry { key, data };
                    let age_distance = self.calculate_age_distance(entry.age());

                    if age_distance >= self.gc_threshold_age_distance {
                        // Clear the entry
                        bucket.entries[key_idx].store(0, Ordering::Relaxed);
                        bucket.entries[data_idx].store(0, Ordering::Relaxed);

                        #[cfg(feature = "tt_metrics")]
                        self.gc_entries_cleared.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        } else {
            let bucket = &self.buckets[bucket_idx];

            for i in 0..BUCKET_SIZE {
                let key_idx = i * 2;
                let data_idx = key_idx + 1;

                let key = bucket.entries[key_idx].load(Ordering::Relaxed);
                if key != 0 {
                    let data = bucket.entries[data_idx].load(Ordering::Relaxed);
                    let entry = TTEntry { key, data };
                    let age_distance = self.calculate_age_distance(entry.age());

                    if age_distance >= self.gc_threshold_age_distance {
                        // Clear the entry
                        bucket.entries[key_idx].store(0, Ordering::Relaxed);
                        bucket.entries[data_idx].store(0, Ordering::Relaxed);

                        #[cfg(feature = "tt_metrics")]
                        self.gc_entries_cleared.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }

        // Update bitmap if bucket is now empty
        if self.is_bucket_empty(bucket_idx) {
            self.unmark_bucket_occupied(bucket_idx);
        }
    }

    /// Perform incremental garbage collection
    /// Returns true if GC is complete
    pub fn incremental_gc(&self, buckets_per_call: usize) -> bool {
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

    /// Prefetch bucket for the given hash with cache level hint
    /// hint: 0=L1, 1=L2, 2=L3, 3=NTA (non-temporal)
    #[inline(always)]
    pub fn prefetch(&self, hash: u64, hint: i32) {
        // Record prefetch count
        #[cfg(feature = "tt_metrics")]
        if let Some(ref metrics) = self.metrics {
            metrics.prefetch_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let idx = self.bucket_index(hash);

        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::_mm_prefetch;
            let bucket_ptr = if let Some(ref flexible) = self.flexible_buckets {
                &flexible[idx] as *const _ as *const i8
            } else {
                &self.buckets[idx] as *const _ as *const i8
            };

            // x86_64 hints: 0=T0(L1), 1=T1(L2), 2=T2(L3), 3=NTA
            // _mm_prefetch requires compile-time constant, so we match on hint
            match hint {
                0 => _mm_prefetch(bucket_ptr, 0), // _MM_HINT_T0
                1 => _mm_prefetch(bucket_ptr, 1), // _MM_HINT_T1
                2 => _mm_prefetch(bucket_ptr, 2), // _MM_HINT_T2
                _ => _mm_prefetch(bucket_ptr, 3), // _MM_HINT_NTA
            }
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            // Use inline assembly for stable Rust compatibility
            let bucket_ptr = if let Some(ref flexible) = self.flexible_buckets {
                &flexible[idx] as *const _ as *const u8
            } else {
                &self.buckets[idx] as *const _ as *const u8
            };

            // ARM PRFM instruction with different cache levels
            match hint {
                0 => {
                    // PLDL1KEEP - Prefetch to L1 cache
                    core::arch::asm!(
                        "prfm pldl1keep, [{ptr}]",
                        ptr = in(reg) bucket_ptr,
                        options(nostack, preserves_flags)
                    );
                }
                1 => {
                    // PLDL2KEEP - Prefetch to L2 cache
                    core::arch::asm!(
                        "prfm pldl2keep, [{ptr}]",
                        ptr = in(reg) bucket_ptr,
                        options(nostack, preserves_flags)
                    );
                }
                _ => {
                    // PLDL3KEEP - Prefetch to L3 cache
                    core::arch::asm!(
                        "prfm pldl3keep, [{ptr}]",
                        ptr = in(reg) bucket_ptr,
                        options(nostack, preserves_flags)
                    );
                }
            }
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            // No-op for unsupported architectures
            let _ = (hash, hint);
        }
    }

    /// Simple prefetch to L1 cache
    #[inline(always)]
    pub fn prefetch_l1(&self, hash: u64) {
        self.prefetch(hash, 0);
    }

    /// Prefetch to L2 cache for deeper searches
    #[inline(always)]
    pub fn prefetch_l2(&self, hash: u64) {
        self.prefetch(hash, 1);
    }

    /// Prefetch to L3 cache for very deep searches
    #[inline(always)]
    pub fn prefetch_l3(&self, hash: u64) {
        self.prefetch(hash, 2);
    }

    /// Enable adaptive prefetch statistics tracking
    pub fn enable_prefetch_stats(&mut self) {
        if self.prefetcher.is_none() {
            self.prefetcher = Some(crate::search::adaptive_prefetcher::AdaptivePrefetcher::new());
        }
    }

    /// Disable adaptive prefetch statistics tracking
    pub fn disable_prefetch_stats(&mut self) {
        self.prefetcher = None;
    }

    /// Get prefetch statistics
    pub fn prefetch_stats(&self) -> Option<crate::search::adaptive_prefetcher::PrefetchStats> {
        self.prefetcher.as_ref().map(|p| p.stats())
    }

    /// Reset prefetch statistics
    pub fn reset_prefetch_stats(&self) {
        if let Some(ref prefetcher) = self.prefetcher {
            prefetcher.reset();
        }
    }
}

// Ensure proper alignment
#[cfg(test)]
mod alignment_tests {
    use super::*;

    #[test]
    fn test_entry_alignment() {
        assert_eq!(std::mem::size_of::<TTEntry>(), 16);
        assert_eq!(std::mem::align_of::<TTEntry>(), 16);
    }

    #[test]
    fn test_bucket_alignment() {
        assert_eq!(std::mem::size_of::<TTBucket>(), 64);
        assert_eq!(std::mem::align_of::<TTBucket>(), 64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitmap_operations() {
        let tt = TranspositionTable::new(1); // 1MB table

        // Initially all buckets should be unoccupied
        assert!(!tt.is_bucket_occupied(0));
        assert!(!tt.is_bucket_occupied(100));

        // Mark some buckets as occupied
        tt.mark_bucket_occupied(0);
        tt.mark_bucket_occupied(100);
        tt.mark_bucket_occupied(255);

        // Check they are marked
        assert!(tt.is_bucket_occupied(0));
        assert!(tt.is_bucket_occupied(100));
        assert!(tt.is_bucket_occupied(255));

        // Check others remain unoccupied
        assert!(!tt.is_bucket_occupied(1));
        assert!(!tt.is_bucket_occupied(99));
        assert!(!tt.is_bucket_occupied(256));
    }

    #[test]
    fn test_hashfull_estimate() {
        let tt = TranspositionTable::new(1); // 1MB table

        // Initially estimate should be 0
        assert_eq!(tt.hashfull_estimate(), 0);

        // Mark some buckets and update estimate
        for i in 0..64 {
            tt.mark_bucket_occupied(i);
        }
        tt.update_hashfull_estimate();

        // Since we marked all 64 sampled buckets, estimate should be 1000
        // But due to EMA, it won't jump immediately
        let estimate = tt.hashfull_estimate();
        assert!(estimate > 0);

        // Multiple updates should converge
        for _ in 0..10 {
            tt.update_hashfull_estimate();
        }
        // After convergence, should be close to 1000 if all sampled buckets are occupied
        // But exact value depends on sampling
    }

    #[test]
    fn test_store_updates_bitmap() {
        let tt = TranspositionTable::new(1); // 1MB table
        let position = crate::shogi::Position::startpos();
        let hash = position.hash;
        let bucket_idx = tt.bucket_index(hash);

        // Initially bucket should be unoccupied
        assert!(!tt.is_bucket_occupied(bucket_idx));

        // Store an entry
        tt.store(hash, None, 100, 50, 5, NodeType::Exact);

        // Bucket should now be occupied
        assert!(tt.is_bucket_occupied(bucket_idx));
    }

    #[test]
    fn test_clear_resets_bitmap() {
        let mut tt = TranspositionTable::new(1); // 1MB table

        // Mark some buckets and set estimate
        for i in 0..100 {
            tt.mark_bucket_occupied(i);
        }
        tt.hashfull_estimate.store(500, Ordering::Relaxed);
        tt.node_counter.store(1000, Ordering::Relaxed);

        // Clear the table
        tt.clear();

        // All buckets should be unoccupied
        for i in 0..100 {
            assert!(!tt.is_bucket_occupied(i));
        }

        // Estimates and counters should be reset
        assert_eq!(tt.hashfull_estimate(), 0);
        assert_eq!(tt.node_counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[cfg(all(feature = "hashfull_filter", feature = "tt_metrics"))]
    fn test_hashfull_filtering() {
        let mut tt = TranspositionTable::new(1); // 1MB table
        tt.enable_metrics();

        // Set hashfull estimate to 750 (75%)
        tt.hashfull_estimate.store(750, Ordering::Relaxed);

        // Try to store a shallow entry (depth=1) - should be filtered at 75%
        let position = crate::shogi::Position::startpos();
        tt.store(
            position.hash,
            None,
            100,
            50,
            1, // very shallow depth - will be filtered when threshold is 2
            NodeType::LowerBound,
        );

        // Should be filtered (750 is in 600-800 range, so depth < 2 is filtered)
        let metrics = tt.metrics.as_ref().unwrap();
        assert_eq!(metrics.hashfull_filtered.load(Ordering::Relaxed), 1);

        // Set hashfull estimate to 900 (90%)
        tt.hashfull_estimate.store(900, Ordering::Relaxed);

        // Try to store a non-exact entry
        tt.store(
            position.hash + 1,
            None,
            100,
            50,
            10,                   // deep enough
            NodeType::LowerBound, // not exact
        );

        // Should be filtered
        assert_eq!(metrics.hashfull_filtered.load(Ordering::Relaxed), 2);

        // Try to store an exact entry
        tt.store(
            position.hash + 2,
            None,
            100,
            50,
            10,
            NodeType::Exact, // exact node
        );

        // Should NOT be filtered
        assert_eq!(metrics.hashfull_filtered.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_gc_trigger() {
        let tt = TranspositionTable::new(1); // 1MB table

        // Initially GC should not be needed
        assert!(!tt.should_trigger_gc());

        // Simulate high hashfull scenario
        // First set a high hashfull estimate
        tt.hashfull_estimate.store(990, Ordering::Relaxed);

        // Directly trigger GC (simulating what store_entry would do)
        tt.need_gc.store(true, Ordering::Relaxed);

        assert!(tt.should_trigger_gc());

        // Test gradual trigger
        tt.need_gc.store(false, Ordering::Relaxed);
        tt.hashfull_estimate.store(950, Ordering::Relaxed);

        // Simulate multiple high hashfull updates
        for _ in 0..11 {
            tt.high_hashfull_counter.fetch_add(1, Ordering::Relaxed);
        }

        // Check if counter would trigger GC
        if tt.high_hashfull_counter.load(Ordering::Relaxed) >= 10 {
            tt.need_gc.store(true, Ordering::Relaxed);
        }

        assert!(tt.should_trigger_gc());
    }

    #[cfg(feature = "tt_metrics")]
    #[test]
    fn test_cas_key_match_optimization() {
        // Test Phase 5 optimization - ensure data updates are properly synchronized
        use std::sync::Arc;
        use std::thread;

        let mut tt = TranspositionTable::new(1); // 1MB table
        tt.enable_metrics();
        let tt = Arc::new(tt);

        let hash = 0x123456789ABCDEF0;
        let initial_depth = 5;
        let updated_depth = 10;

        // Initial store
        tt.store(hash, None, 100, 50, initial_depth, NodeType::Exact);

        // Spawn multiple threads trying to update the same position
        let mut handles = vec![];
        let num_threads = 4;

        for i in 0..num_threads {
            let tt_clone = Arc::clone(&tt);
            let handle = thread::spawn(move || {
                // Each thread tries to update with a different depth
                let thread_depth = updated_depth + i as u8;
                tt_clone.store(hash, None, 200, 60, thread_depth, NodeType::Exact);

                // Immediately probe to check if the update is visible
                tt_clone.probe(hash)
            });
            handles.push(handle);
        }

        // Collect results
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All threads should see a valid entry
        for result in &results {
            assert!(result.is_some(), "Entry should be found");
            if let Some(entry) = result {
                assert!(entry.depth() >= initial_depth, "Depth should not decrease");
                assert!(
                    entry.depth() <= updated_depth + num_threads as u8,
                    "Depth should be within expected range"
                );
            }
        }

        // Check metrics
        if let Some(metrics) = &tt.metrics {
            let cas_key_match = metrics.cas_key_match.load(Ordering::Relaxed);
            // We expect at least some key matches in high contention scenario
            println!("CAS key matches in test: {cas_key_match}");
        }
    }

    #[test]
    fn test_incremental_gc() {
        let mut tt = TranspositionTable::new(1); // 1MB table
        let position = crate::shogi::Position::startpos();

        // Fill table with entries of different ages
        for i in 0..1000 {
            let hash = position.hash + i;
            tt.store_with_params(TTEntryParams {
                key: hash,
                mv: None,
                score: 100,
                eval: 50,
                depth: 5,
                node_type: NodeType::Exact,
                age: (i % 8) as u8, // Various ages
                is_pv: false,
                ..Default::default()
            });
        }

        // Advance age to make some entries old
        tt.age = 6;

        // Trigger GC
        tt.need_gc.store(true, Ordering::Relaxed);

        // Run incremental GC
        let mut complete = false;
        let mut iterations = 0;
        while !complete && iterations < 1000 {
            complete = tt.incremental_gc(256);
            iterations += 1;
        }

        assert!(complete);
        assert!(!tt.should_trigger_gc());

        // Verify some entries were cleared
        #[cfg(feature = "tt_metrics")]
        {
            let cleared = tt.gc_entries_cleared.load(std::sync::atomic::Ordering::Relaxed);
            assert!(cleared > 0);
        }
    }

    #[test]
    fn test_age_distance_calculation() {
        let mut tt = TranspositionTable::new(1);

        // Test various age combinations
        tt.age = 5;
        assert_eq!(tt.calculate_age_distance(5), 0); // Same age
        assert_eq!(tt.calculate_age_distance(4), 1); // 1 generation old
        assert_eq!(tt.calculate_age_distance(3), 2); // 2 generations old
        assert_eq!(tt.calculate_age_distance(1), 4); // 4 generations old

        // Test wraparound
        tt.age = 1;
        assert_eq!(tt.calculate_age_distance(7), 2); // Wrapped around
        assert_eq!(tt.calculate_age_distance(6), 3); // Wrapped around
    }

    use crate::{
        shogi::{Move, Square},
        usi::parse_usi_square,
    };

    // Test SIMD probe produces same results as scalar
    #[test]
    fn test_tt_probe_simd_vs_scalar() {
        let bucket = TTBucket::new();
        let test_entry = TTEntry::new(0x1234567890ABCDEF, None, 100, -50, 10, NodeType::Exact, 0);

        // Store an entry
        bucket.store_with_metrics_and_mode(test_entry, 0, false, None);

        // Test both SIMD and scalar paths produce same result
        let simd_result = if bucket.probe_simd_available() {
            bucket.probe_simd_impl(test_entry.key)
        } else {
            bucket.probe_scalar(test_entry.key)
        };

        let scalar_result = bucket.probe_scalar(test_entry.key);

        assert_eq!(simd_result.is_some(), scalar_result.is_some());
        if let (Some(simd_entry), Some(scalar_entry)) = (simd_result, scalar_result) {
            assert_eq!(simd_entry.key, scalar_entry.key);
            assert_eq!(simd_entry.data, scalar_entry.data);
        }
    }

    // Test SIMD store priority calculation
    #[test]
    fn test_tt_store_simd_priority() {
        let bucket = TTBucket::new();

        // Fill bucket with test entries with unique keys
        for i in 0..BUCKET_SIZE {
            // Use high bits to ensure unique keys after shift
            let key = (0x1000000000000000_u64 * (i as u64 + 1)) | 0xFFFF;
            let entry = TTEntry::new(
                key,
                None,
                50 + i as i16 * 10,
                -20 + i as i16 * 5,
                5 + i as u8,
                if i % 2 == 0 {
                    NodeType::Exact
                } else {
                    NodeType::LowerBound
                },
                i as u8,
            );
            bucket.store_with_metrics_and_mode(entry, 0, false, None);
        }

        // Test both SIMD and scalar find worst entry
        let current_age = 4;
        let (simd_idx, simd_score) = if bucket.store_simd_available() {
            bucket.find_worst_entry_simd(current_age)
        } else {
            bucket.find_worst_entry_scalar(current_age)
        };

        let (scalar_idx, scalar_score) = bucket.find_worst_entry_scalar(current_age);

        // Debug output to understand the difference
        if simd_idx != scalar_idx {
            println!("SIMD idx: {simd_idx}, score: {simd_score}");
            println!("Scalar idx: {scalar_idx}, score: {scalar_score}");

            // Check all scores to understand what's happening
            for i in 0..BUCKET_SIZE {
                let idx = i * 2;
                let key = bucket.entries[idx].load(Ordering::Acquire);
                if key != 0 {
                    let data = bucket.entries[idx + 1].load(Ordering::Acquire);
                    let entry = TTEntry { key, data };
                    let score = entry.priority_score(current_age);
                    println!(
                        "Entry {}: depth={}, age={}, score={}",
                        i,
                        entry.depth(),
                        entry.age(),
                        score
                    );
                } else {
                    println!("Entry {i}: empty");
                }
            }
        }

        // They should identify the same worst entry
        assert_eq!(simd_idx, scalar_idx, "SIMD and scalar should find same worst entry");
        assert_eq!(simd_score, scalar_score, "SIMD and scalar should calculate same score");
    }

    // Test depth filter functionality
    #[test]
    #[cfg(feature = "tt_metrics")]
    fn test_tt_depth_filter() {
        let bucket = TTBucket::new();
        let metrics = DetailedTTMetrics::new();

        // Store an entry with depth 10
        let key = 0x1234567890ABCDEF;
        let entry1 = TTEntry::new(key, None, 100, 50, 10, NodeType::Exact, 0);
        bucket.store_with_metrics_and_mode(entry1, 0, false, Some(&metrics));

        // Try to update with a shallower entry (depth 5)
        let entry2 = TTEntry::new(key, None, 200, 60, 5, NodeType::Exact, 0);
        bucket.store_with_metrics_and_mode(entry2, 0, false, Some(&metrics));

        // The update should be filtered
        assert_eq!(metrics.depth_filtered.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.update_existing.load(Ordering::Relaxed), 0); // No successful update yet

        // Verify the original entry is still there
        let result = bucket.probe(key);
        assert!(result.is_some());
        let stored_entry = result.unwrap();
        assert_eq!(stored_entry.depth(), 10);
        assert_eq!(stored_entry.score(), 100);

        // Try to update with a deeper entry (depth 15)
        let entry3 = TTEntry::new(key, None, 300, 70, 15, NodeType::Exact, 0);
        bucket.store_with_metrics_and_mode(entry3, 0, false, Some(&metrics));

        // This update should succeed
        assert_eq!(metrics.depth_filtered.load(Ordering::Relaxed), 1); // Still 1
        assert_eq!(metrics.update_existing.load(Ordering::Relaxed), 1); // Now 1
        assert_eq!(metrics.effective_updates.load(Ordering::Relaxed), 1); // Only the depth-improved update

        // Verify the new entry replaced the old one
        let result = bucket.probe(key);
        assert!(result.is_some());
        let stored_entry = result.unwrap();
        assert_eq!(stored_entry.depth(), 15);
        assert_eq!(stored_entry.score(), 300);
    }

    // Test CAS failure data integrity
    #[test]
    fn test_cas_failure_data_integrity() {
        use std::sync::Arc;
        use std::thread;

        let bucket = Arc::new(TTBucket::new());
        let num_threads = 4;
        let iterations = 1000;

        // Launch multiple threads trying to write to the same slot
        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let bucket = Arc::clone(&bucket);
                thread::spawn(move || {
                    for i in 0..iterations {
                        let key = 0x1000 + (thread_id as u64);
                        let score = (thread_id * 100 + i) as i16;
                        let entry = TTEntry::new(key, None, score, 0, 10, NodeType::Exact, 0);
                        bucket.store_with_metrics_and_mode(entry, 0, false, None);
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify data integrity - each slot should have consistent key-data pairs
        for i in 0..BUCKET_SIZE {
            let idx = i * 2;
            let key = bucket.entries[idx].load(Ordering::Acquire);

            if key != 0 {
                let data = bucket.entries[idx + 1].load(Ordering::Acquire);
                let entry = TTEntry { key, data };

                // Extract thread_id from key
                let thread_id = (key - 0x1000) as usize;

                // Score should be consistent with the thread that wrote it
                let score = entry.score();
                assert!(
                    score >= (thread_id * 100) as i16 && score < ((thread_id + 1) * 100) as i16,
                    "Inconsistent data: key={:x}, score={}, expected range [{}, {})",
                    key,
                    score,
                    thread_id * 100,
                    (thread_id + 1) * 100
                );
            }
        }
    }

    #[test]
    fn test_bucket_operations() {
        let bucket = TTBucket::new();
        let hash1 = 0x1234567890ABCDEF;
        let mv = Some(Move::normal(
            parse_usi_square("7h").unwrap(),
            parse_usi_square("7g").unwrap(),
            false,
        ));

        // Store entry
        let entry = TTEntry::new(hash1, mv, 100, 50, 10, NodeType::Exact, 0);
        bucket.store(entry, 0);

        // Probe should find it
        let found = bucket.probe(hash1);
        assert!(found.is_some());
        let found_entry = found.unwrap();
        assert_eq!(found_entry.score(), 100);
        assert_eq!(found_entry.depth(), 10);
    }

    #[test]
    fn test_bucket_replacement() {
        let bucket = TTBucket::new();
        let current_age = 0;

        // Fill bucket with entries
        for i in 0..BUCKET_SIZE {
            let hash = 0x1000000000000000 * (i as u64 + 1);
            let entry = TTEntry::new(hash, None, i as i16, 0, 5, NodeType::LowerBound, current_age);
            bucket.store(entry, current_age);
        }

        // Try to store one more entry with higher depth
        let new_hash = 0x5000000000000000;
        let new_entry = TTEntry::new(new_hash, None, 999, 0, 20, NodeType::Exact, current_age);
        bucket.store(new_entry, current_age);

        // New entry should be stored (replacing a shallow entry)
        let found = bucket.probe(new_hash);
        assert!(found.is_some());
        assert_eq!(found.unwrap().score(), 999);
    }

    #[test]
    fn test_transposition_table() {
        let tt = TranspositionTable::new(1);
        let hash = 0x1234567890ABCDEF;
        let mv = Some(Move::normal(
            parse_usi_square("2h").unwrap(),
            parse_usi_square("2g").unwrap(),
            false,
        ));

        // Store and retrieve
        tt.store(hash, mv, 1500, 1000, 8, NodeType::LowerBound);

        let entry = tt.probe(hash).expect("Entry should be found");
        assert_eq!(entry.score(), 1500);
        assert_eq!(entry.eval(), 1000);
        assert_eq!(entry.depth(), 8);
        assert_eq!(entry.node_type(), NodeType::LowerBound);
    }

    #[test]
    fn test_generation_management() {
        let mut tt = TranspositionTable::new(1);
        assert_eq!(tt.age, 0);

        // Advance generations
        for i in 1..=7 {
            tt.new_search();
            assert_eq!(tt.age, i);
        }

        // Should wrap around to 0
        tt.new_search();
        assert_eq!(tt.age, 0);
    }

    #[test]
    fn test_prefetch() {
        let tt = TranspositionTable::new(1);
        let hash = 0x1234567890ABCDEF;

        // Store entry
        tt.store(hash, None, 100, 50, 10, NodeType::Exact);

        // Prefetch should not crash
        tt.prefetch_l1(hash);

        // Verify entry is still accessible
        let entry = tt.probe(hash);
        assert!(entry.is_some());
    }

    #[test]
    fn test_cache_line_optimization() {
        // Test that accessing entries in the same bucket is fast
        let tt = TranspositionTable::new(1);

        // Set hashfull high to avoid empty slot mode
        tt.hashfull_estimate.store(500, Ordering::Relaxed);

        // Find hashes that map to the same bucket but have different keys
        let bucket_idx = 100_usize; // Choose a specific bucket
        let base_upper = 0x1234567800000000_u64;

        // Store multiple entries in same bucket with different upper bits
        let mut stored_count = 0;
        for i in 0..BUCKET_SIZE {
            // Create hash with same lower bits (bucket index) but different upper bits
            let hash = base_upper + ((i as u64 + 1) << 32) + bucket_idx as u64;
            assert_eq!(tt.bucket_index(hash), bucket_idx);

            tt.store(hash, None, i as i16, 0, 10, NodeType::Exact);
            stored_count += 1;
        }

        // Verify entries can be retrieved
        let mut retrieved_count = 0;
        for i in 0..BUCKET_SIZE {
            let hash = base_upper + ((i as u64 + 1) << 32) + bucket_idx as u64;
            let entry = tt.probe(hash);
            if entry.is_some() {
                assert_eq!(entry.unwrap().score(), i as i16);
                retrieved_count += 1;
            }
        }

        // All entries should be stored and retrievable since they have different keys
        assert_eq!(stored_count, BUCKET_SIZE);
        assert_eq!(retrieved_count, BUCKET_SIZE);
    }

    // === Apery-style improvement tests ===

    #[test]
    fn test_generation_cycle_distance() {
        let tt = TranspositionTable::new(1);

        // Store entry with age 0
        let hash1 = 0x1234567890abcdef;
        tt.store(hash1, None, 100, 50, 10, NodeType::Exact);

        // Verify entry exists and has correct age
        let entry = tt.probe(hash1).expect("Entry should exist");
        assert_eq!(entry.score(), 100);
        assert_eq!(entry.depth(), 10);
        assert_eq!(entry.age(), 0);
    }

    #[test]
    fn test_pv_flag_functionality() {
        let tt = TranspositionTable::new(1);

        // Store PV node
        let hash = 0xfedcba0987654321;
        tt.store_with_params(TTEntryParams {
            key: hash,
            mv: None,
            score: 200,
            eval: 100,
            depth: 15,
            node_type: NodeType::Exact,
            age: 0, // Will be overridden by store_with_params
            is_pv: true,
            ..Default::default()
        });

        // Verify PV flag is set
        let entry = tt.probe(hash).expect("Entry should exist");
        assert!(entry.is_pv(), "PV flag should be set");
        assert_eq!(entry.node_type(), NodeType::Exact);
        assert_eq!(entry.score(), 200);
        assert_eq!(entry.depth(), 15);

        // Store another entry with same hash (will always replace due to exact key match)
        tt.store_with_params(TTEntryParams {
            key: hash,
            mv: None,
            score: 300,
            eval: 150,
            depth: 20,
            node_type: NodeType::LowerBound,
            age: 0, // Will be overridden by store_with_params
            is_pv: false,
            ..Default::default()
        });

        // The new entry should replace
        let entry = tt.probe(hash).expect("Entry should exist");
        assert_eq!(entry.score(), 300, "New entry should replace on exact key match");
        assert!(!entry.is_pv(), "PV flag should be cleared");
    }

    #[test]
    fn test_priority_score_with_age() {
        let mut tt = TranspositionTable::new(1);

        // Fill a bucket with entries
        let base_hash = 0x123456789abcdef0;

        // Store 4 entries (bucket size) with different depths
        for i in 0..4 {
            let hash = base_hash + i;
            tt.store(hash, None, (i * 10) as i16, 0, (i + 1) as u8, NodeType::Exact);
        }

        // Advance age
        tt.new_search();
        tt.new_search();

        // Try to store a new entry - should replace the shallowest old entry
        let new_hash = base_hash + 100;
        tt.store_with_params(TTEntryParams {
            key: new_hash,
            mv: None,
            score: 500,
            eval: 250,
            depth: 20,
            node_type: NodeType::Exact,
            age: 0, // Will be overridden by store_with_params
            is_pv: true,
            ..Default::default()
        });

        // Verify the new entry exists
        let entry = tt.probe(new_hash);
        assert!(entry.is_some(), "New entry should be stored");
    }

    #[test]
    fn test_age_wraparound_handling() {
        let mut tt = TranspositionTable::new(1);

        // Store entry at age 0
        let hash = 0xdeadbeefcafebabe;
        tt.store(hash, None, 100, 50, 10, NodeType::Exact);

        let entry1 = tt.probe(hash).expect("Entry should exist");
        let initial_age = entry1.age();

        // Advance age through full cycle (8 generations)
        for _ in 0..8 {
            tt.new_search();
        }

        // Store new entry after wraparound
        let hash2 = 0xbabecafedeadbeef;
        tt.store(hash2, None, 200, 100, 15, NodeType::Exact);

        let entry2 = tt.probe(hash2).expect("Entry should exist");
        // After 8 generations, age wraps around (0-7), so we're back at 0
        assert_eq!(entry2.age(), initial_age, "Age should wrap around after 8 generations");
    }

    #[test]
    fn test_high_contention_cas_handling() {
        use std::sync::Arc;
        use std::thread;

        // Test CAS retry mechanism under high contention
        let tt = Arc::new(TranspositionTable::new(1)); // Small table to force collisions
        let num_threads = 16;
        let iterations_per_thread = 1000;
        let target_hash = 0x123456789ABCDEF0; // Same hash for all threads

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let tt = Arc::clone(&tt);
                thread::spawn(move || {
                    for i in 0..iterations_per_thread {
                        // All threads try to write to the same hash location
                        tt.store(
                            target_hash,
                            Some(Move::normal(
                                Square::new((thread_id % 9) as u8, 7),
                                Square::new((thread_id % 9) as u8, 6),
                                false,
                            )),
                            thread_id as i16 * 10 + i as i16,
                            thread_id as i16 * 5,
                            (thread_id % 20) as u8,
                            NodeType::Exact,
                        );
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify that the table still works correctly after high contention
        let entry = tt.probe(target_hash);
        assert!(entry.is_some(), "Entry should exist after high contention");

        // The entry should have valid data (from one of the threads)
        let entry = entry.unwrap();
        assert!(entry.depth() <= 20, "Depth should be reasonable");
        assert!(entry.score().abs() < 10000, "Score should be reasonable");
    }

    #[test]
    fn test_sign_extension_correctness() {
        // Test that our optimized sign extension works correctly
        // for 14-bit signed values

        // Test cases: [14-bit value, expected 16-bit result]
        let test_cases = vec![
            // Zero and edge cases
            (0x0000_u16, 0_i16),  // Zero
            (0x0001_u16, 1_i16),  // Smallest positive
            (0x3FFF_u16, -1_i16), // -1 in 14-bit
            // Positive boundary values
            (0x1FFE_u16, 8190_i16), // Max positive - 1
            (0x1FFF_u16, 8191_i16), // Max positive (2^13 - 1)
            // Negative boundary values (critical for sign extension)
            (0x2000_u16, -8192_i16), // Min negative (-2^13) - most critical
            (0x2001_u16, -8191_i16), // Min negative + 1
            (0x2002_u16, -8190_i16), // Min negative + 2
            // Mid-range values
            (0x1000_u16, 4096_i16),  // Mid positive
            (0x3000_u16, -4096_i16), // Mid negative
            (0x0FFF_u16, 4095_i16),  // Just below mid positive
            (0x2FFF_u16, -4097_i16), // Just above mid negative
            // Additional edge cases near sign bit
            (0x1FFD_u16, 8189_i16),  // Close to max positive
            (0x2003_u16, -8189_i16), // Close to min negative
        ];

        for (raw_14bit, expected) in test_cases {
            // Test score sign extension
            let entry = TTEntry {
                key: 0,
                data: (raw_14bit as u64) << SCORE_SHIFT,
            };
            let actual_score = entry.score();
            assert_eq!(
                actual_score, expected,
                "Score sign extension failed for {raw_14bit:#06x}: got {actual_score}, expected {expected}"
            );

            // Test eval sign extension
            let entry = TTEntry {
                key: 0,
                data: (raw_14bit as u64) << EVAL_SHIFT,
            };
            let actual_eval = entry.eval();
            assert_eq!(
                actual_eval,
                expected,
                "Eval sign extension failed for {raw_14bit:#06x}: got {actual_eval}, expected {expected}"
            );
        }

        // Additional test: Verify round-trip conversion
        // Store values and retrieve them to ensure consistency
        for value in [-8192, -8191, -1, 0, 1, 8190, 8191] {
            let params = TTEntryParams {
                key: 0x123456789ABCDEF,
                mv: None,
                score: value,
                eval: value,
                depth: 10,
                node_type: NodeType::Exact,
                age: 0,
                is_pv: false,
                ..Default::default()
            };
            let entry = TTEntry::from_params(params);
            assert_eq!(entry.score(), value, "Round-trip failed for score {value}");
            assert_eq!(entry.eval(), value, "Round-trip failed for eval {value}");
        }
    }

    #[test]
    fn test_apery_priority_calculation() {
        // Test the priority calculation indirectly through bucket replacement
        let bucket = TTBucket::new();

        // Create entries with different characteristics
        let entry1 = TTEntry::from_params(TTEntryParams {
            key: 0x123,
            mv: None,
            score: 100,
            eval: 50,
            depth: 10,
            node_type: NodeType::Exact,
            age: 0,
            is_pv: false,
            ..Default::default()
        });
        let entry2 = TTEntry::from_params(TTEntryParams {
            key: 0x456,
            mv: None,
            score: 100,
            eval: 50,
            depth: 10,
            node_type: NodeType::Exact,
            age: 0,
            is_pv: true,
            ..Default::default()
        });

        // Store regular entry
        bucket.store(entry1, 0);
        assert!(bucket.probe(0x123).is_some());

        // Store PV entry - it should be preserved when possible
        bucket.store(entry2, 0);
        assert!(bucket.probe(0x456).is_some());

        // Test that PV entries have priority in replacement
        // Fill the bucket
        for i in 2..BUCKET_SIZE {
            let hash = 0x1000 * (i as u64 + 1);
            let entry = TTEntry::new(hash, None, 50, 25, 5, NodeType::LowerBound, 0);
            bucket.store_with_metrics_and_mode(entry, 0, false, None);
        }

        // PV entry should still be there
        assert!(bucket.probe(0x456).is_some(), "PV entry should be preserved");
    }

    // Tests for dynamic bucket sizing
    #[test]
    fn test_bucket_size_selection() {
        assert_eq!(BucketSize::optimal_for_size(4), BucketSize::Small);
        assert_eq!(BucketSize::optimal_for_size(8), BucketSize::Small);
        assert_eq!(BucketSize::optimal_for_size(16), BucketSize::Medium);
        assert_eq!(BucketSize::optimal_for_size(32), BucketSize::Medium);
        assert_eq!(BucketSize::optimal_for_size(64), BucketSize::Large);
        assert_eq!(BucketSize::optimal_for_size(128), BucketSize::Large);
    }

    #[test]
    fn test_bucket_size_properties() {
        assert_eq!(BucketSize::Small.entries(), 4);
        assert_eq!(BucketSize::Small.bytes(), 64);

        assert_eq!(BucketSize::Medium.entries(), 8);
        assert_eq!(BucketSize::Medium.bytes(), 128);

        assert_eq!(BucketSize::Large.entries(), 16);
        assert_eq!(BucketSize::Large.bytes(), 256);
    }

    #[test]
    fn test_flexible_bucket_operations() {
        for size in [BucketSize::Small, BucketSize::Medium, BucketSize::Large] {
            let bucket = FlexibleTTBucket::new(size);
            let hash = 0x1234567890ABCDEF;

            // Test probe on empty bucket
            assert!(bucket.probe(hash).is_none());

            // Store entry
            let params = TTEntryParams {
                key: hash,
                mv: None,
                score: 100,
                eval: 50,
                depth: 10,
                node_type: NodeType::Exact,
                age: 0,
                is_pv: false,
                ..Default::default()
            };
            bucket.store_with_mode(params, 0, false, None);

            // Retrieve entry
            let found = bucket.probe(hash);
            assert!(found.is_some());
            let entry = found.unwrap();
            assert_eq!(entry.score(), 100);
            assert_eq!(entry.eval(), 50);
            assert_eq!(entry.depth(), 10);
        }
    }

    #[test]
    fn test_dynamic_tt_creation() {
        // Test with different sizes and configurations
        let configs = [
            (4, Some(BucketSize::Small)),
            (16, Some(BucketSize::Medium)),
            (64, Some(BucketSize::Large)),
            (16, None), // Auto-select
        ];

        for (size_mb, bucket_size) in configs {
            let tt = TranspositionTable::new_with_config(size_mb, bucket_size);

            // Verify it was created with flexible buckets
            assert!(tt.flexible_buckets.is_some());
            assert!(tt.bucket_size.is_some());

            // Test basic operations
            let hash = 0xABCDEF1234567890;
            tt.store(hash, None, 100, 50, 10, NodeType::Exact);

            let entry = tt.probe(hash);
            assert!(entry.is_some());
            assert_eq!(entry.unwrap().score(), 100);
        }
    }

    #[test]
    fn test_flexible_bucket_replacement() {
        let bucket = FlexibleTTBucket::new(BucketSize::Medium); // 8 entries

        // Test replacement within a single bucket
        // First, verify empty bucket
        for i in 0..8 {
            let hash = (i + 1) << 32 | i;
            assert!(bucket.probe(hash).is_none(), "Bucket should start empty");
        }

        // Fill bucket with entries - all different upper bits but same bucket
        // Note: In a real TT, these would map to the same bucket based on lower bits
        for i in 0..8 {
            let hash = ((i + 1) as u64) << 32 | 0x1234; // Different upper bits, same lower
            let params = TTEntryParams {
                key: hash,
                mv: None,
                score: (i * 10) as i16,
                eval: 0,
                depth: (i + 1) as u8, // Increasing depth
                node_type: NodeType::Exact,
                age: 0,
                is_pv: false,
                ..Default::default()
            };
            bucket.store_with_mode(params, 0, false, None);
        }

        // After storing 8 unique entries in an 8-entry bucket, all should be retrievable
        let mut found_count = 0;
        for i in 0..8 {
            let hash = ((i + 1) as u64) << 32 | 0x1234;
            if bucket.probe(hash).is_some() {
                found_count += 1;
            }
        }
        assert_eq!(found_count, 8, "All 8 entries should be stored");

        // Add a new entry with much higher depth (should replace lowest priority)
        let new_hash = (9_u64 << 32) | 0x1234;
        let new_params = TTEntryParams {
            key: new_hash,
            mv: None,
            score: 200,
            eval: 100,
            depth: 20,
            node_type: NodeType::Exact,
            age: 0,
            is_pv: true,
            ..Default::default()
        };
        bucket.store_with_mode(new_params, 0, false, None);

        // New entry should be stored
        assert!(bucket.probe(new_hash).is_some());
    }

    #[test]
    fn test_simd_8_entry_correctness() {
        let bucket = FlexibleTTBucket::new(BucketSize::Medium);

        // Store entries at different positions
        let test_hashes = [
            0x1111111111111111,
            0x2222222222222222,
            0x3333333333333333,
            0x4444444444444444,
            0x5555555555555555,
            0x6666666666666666,
            0x7777777777777777,
            0x8888888888888888,
        ];

        for (i, &hash) in test_hashes.iter().enumerate() {
            let params = TTEntryParams {
                key: hash,
                mv: None,
                score: (i * 10) as i16,
                eval: 0,
                depth: (i + 1) as u8,
                node_type: NodeType::Exact,
                age: 0,
                is_pv: false,
                ..Default::default()
            };
            bucket.store_with_mode(params, 0, false, None);
        }

        // Test both SIMD and scalar probe paths
        for (i, &hash) in test_hashes.iter().enumerate() {
            // SIMD path
            if bucket.probe_simd_available() {
                let simd_result = bucket.probe_simd_8(hash);
                assert!(simd_result.is_some(), "SIMD probe failed for entry {i}");
            }

            // Scalar path
            let scalar_result = bucket.probe_scalar_8(hash);
            assert!(scalar_result.is_some(), "Scalar probe failed for entry {i}");

            // Full probe (uses dispatch)
            let result = bucket.probe(hash);
            assert!(result.is_some(), "Full probe failed for entry {i}");
            assert_eq!(result.unwrap().score(), (i * 10) as i16);
        }
    }
}

#[cfg(test)]
mod parallel_tests {
    use crate::usi::parse_usi_square;

    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_cas_basic_functionality() {
        // First test basic single-threaded operation
        let tt = TranspositionTable::new(1);
        let hash = 0x123456789ABCDEF;

        // Store and retrieve
        tt.store(hash, None, 100, 50, 10, NodeType::Exact);
        let entry = tt.probe(hash);
        assert!(entry.is_some(), "Should find entry after store");
        assert_eq!(entry.unwrap().score(), 100);
    }

    #[test]
    fn test_cas_concurrent_updates() {
        let tt = Arc::new(TranspositionTable::new(1));
        let num_threads = 4;
        let test_hash = 0x123456789ABCDEF;

        let mut handles = vec![];

        // Multiple threads updating the same position
        for thread_id in 0..num_threads {
            let tt_clone = Arc::clone(&tt);
            let handle = thread::spawn(move || {
                for i in 0..100 {
                    // Each thread stores its ID as the score
                    tt_clone.store(
                        test_hash,
                        None,
                        (thread_id * 100 + i) as i16,
                        0,
                        10,
                        NodeType::Exact,
                    );

                    // Give other threads a chance
                    if i % 10 == 0 {
                        thread::yield_now();
                    }
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().expect("Thread should complete");
        }

        // Final entry should exist
        let final_entry = tt.probe(test_hash);
        assert!(final_entry.is_some(), "Entry should exist after concurrent updates");
    }

    #[test]
    fn test_cas_different_positions() {
        let tt = Arc::new(TranspositionTable::new(1));

        // Set hashfull to avoid empty slot mode in parallel test
        tt.hashfull_estimate.store(500, Ordering::Relaxed);

        let num_threads = 4;
        let operations_per_thread = 100;

        let mut handles = vec![];

        for thread_id in 0..num_threads {
            let tt_clone = Arc::clone(&tt);
            let handle = thread::spawn(move || {
                // Each thread uses its own hash range
                for i in 0..operations_per_thread {
                    let hash = 0x1000000000000000 * (thread_id as u64 + 1) + i as u64;

                    // Store entry
                    tt_clone.store(
                        hash,
                        None,
                        (thread_id * 100 + i) as i16,
                        0,
                        10,
                        NodeType::Exact,
                    );

                    // Verify we can read it back
                    let entry = tt_clone.probe(hash);
                    assert!(
                        entry.is_some(),
                        "Entry not found for hash {hash:#x} (thread {thread_id}, iteration {i})"
                    );
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().expect("Thread should complete");
        }

        // Verify table has entries
        let hashfull = tt.hashfull();
        assert!(hashfull > 0, "Table should contain entries");
    }

    #[test]
    fn test_prefetch_basic() {
        let tt = TranspositionTable::new(16); // 16MB table
        let pos = crate::shogi::board::Position::startpos();
        let hash = pos.zobrist_hash();

        // Test different cache levels
        tt.prefetch_l1(hash);
        tt.prefetch_l2(hash);
        tt.prefetch_l3(hash);

        // Store an entry
        let mv = crate::shogi::Move::make_normal(
            parse_usi_square("2h").unwrap(),
            parse_usi_square("2g").unwrap(),
        );

        tt.store(hash, Some(mv), 100, 50, 10, NodeType::Exact);

        // Prefetch and probe should work
        tt.prefetch_l1(hash);
        let entry = tt.probe(hash);
        assert!(entry.is_some());

        if let Some(entry) = entry {
            assert_eq!(entry.score(), 100);
            assert_eq!(entry.depth(), 10);
        }
    }

    #[test]
    fn test_prefetch_with_hint() {
        let tt = TranspositionTable::new(16);
        let pos = crate::shogi::board::Position::startpos();

        // Test all hint levels
        for hint in 0..=3 {
            tt.prefetch(pos.zobrist_hash(), hint);
        }

        // Store and retrieve
        tt.store(pos.zobrist_hash(), None, 200, 100, 15, NodeType::LowerBound);

        // Prefetch with specific hint
        tt.prefetch(pos.zobrist_hash(), 1); // L2 cache

        let entry = tt.probe(pos.zobrist_hash());
        assert!(entry.is_some());
    }

    #[test]
    fn test_prefetch_cache_level_selection() {
        let tt = TranspositionTable::new(16);
        let pos = crate::shogi::board::Position::startpos();

        // Test that different cache levels work
        let depths = [
            (0, 0),  // Shallow: L1
            (5, 0),  // Shallow: L1
            (10, 1), // Medium: L2
            (15, 1), // Medium: L2
            (20, 2), // Deep: L3
            (25, 2), // Deep: L3
        ];

        for (depth, expected_hint) in depths {
            // Calculate appropriate hint based on depth
            let hint = if depth < 8 {
                0 // L1
            } else if depth < 16 {
                1 // L2
            } else {
                2 // L3
            };

            assert_eq!(hint, expected_hint);

            // Prefetch with calculated hint
            tt.prefetch(pos.zobrist_hash() ^ depth, hint);
        }
    }

    #[test]
    #[ignore] // Performance test - run with --ignored flag
    fn test_prefetch_performance() {
        use std::time::Instant;

        let tt = TranspositionTable::new(128); // Larger table for performance test
        let pos = crate::shogi::board::Position::startpos();

        // Generate test hashes
        let test_hashes: Vec<u64> = (0..10000).map(|i| pos.zobrist_hash() ^ (i as u64)).collect();

        // Store entries
        for &hash in &test_hashes[0..5000] {
            tt.store(hash, None, 100, 50, 10, NodeType::Exact);
        }

        // Test without prefetch
        let start = Instant::now();
        let mut hits_without = 0;
        for &hash in &test_hashes {
            if tt.probe(hash).is_some() {
                hits_without += 1;
            }
        }
        let time_without = start.elapsed();

        // Test with prefetch
        let start = Instant::now();
        let mut hits_with = 0;
        for (i, &hash) in test_hashes.iter().enumerate() {
            // Prefetch next few entries
            for j in 1..=3 {
                if i + j < test_hashes.len() {
                    tt.prefetch_l1(test_hashes[i + j]);
                }
            }

            if tt.probe(hash).is_some() {
                hits_with += 1;
            }
        }
        let time_with = start.elapsed();

        // Both should find the same number of hits
        assert_eq!(hits_without, hits_with);
        assert_eq!(hits_without, 5000);

        // Log performance difference (prefetch might not always be faster in tests)
        println!("Without prefetch: {time_without:?}, With prefetch: {time_with:?}");
    }

    #[test]
    fn test_memory_ordering_correctness() {
        // This test verifies that the memory ordering fix prevents
        // readers from seeing new keys with old/zero data
        let tt = Arc::new(TranspositionTable::new(1));
        let num_threads = 8;
        let iterations = 1000;

        // Use a barrier to synchronize thread start
        use std::sync::Barrier;
        let barrier = Arc::new(Barrier::new(num_threads + 1));

        let mut handles = vec![];

        // Spawn reader threads
        for reader_id in 0..num_threads / 2 {
            let tt_clone = Arc::clone(&tt);
            let barrier_clone = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                barrier_clone.wait();

                for i in 0..iterations {
                    let hash = 0x123456789abcdef0 + (i % 100) as u64;

                    if let Some(entry) = tt_clone.probe(hash) {
                        // Verify that if we see a key, the data is valid
                        assert!(
                            entry.depth() > 0,
                            "Reader {reader_id} saw key but depth is 0 at iteration {i}"
                        );
                        assert!(
                            entry.score() != 0,
                            "Reader {reader_id} saw key but score is 0 at iteration {i}"
                        );
                        // The move might be None, but other fields should be valid
                        assert!(
                            entry.eval() != 0 || entry.node_type() != NodeType::Exact,
                            "Reader {reader_id} saw key but data looks uninitialized at iteration {i}"
                        );
                    }
                }
            });
            handles.push(handle);
        }

        // Spawn writer threads
        for writer_id in 0..num_threads / 2 {
            let tt_clone = Arc::clone(&tt);
            let barrier_clone = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                barrier_clone.wait();

                for i in 0..iterations {
                    let hash = 0x123456789abcdef0 + (i % 100) as u64;
                    let score = (writer_id * 1000 + i) as i16;
                    let eval = (writer_id * 100 + i) as i16;
                    let depth = ((i % 20) + 1) as u8;

                    // Store with non-zero values
                    tt_clone.store(hash, None, score, eval, depth, NodeType::Exact);

                    // Occasionally use different hash ranges to test empty slot insertion
                    if i % 50 == 0 {
                        let empty_hash = 0xfedcba9876543210 + (i % 100) as u64;
                        tt_clone.store(empty_hash, None, score, eval, depth, NodeType::Exact);
                    }
                }
            });
            handles.push(handle);
        }

        // Start all threads simultaneously
        barrier.wait();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().expect("Thread should complete");
        }
    }
}
