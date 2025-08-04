//! Optimized transposition table with bucket structure
//!
//! This implementation uses a bucket structure to optimize cache performance:
//! - 4 entries per bucket (64 bytes = 1 cache line)
//! - Improved replacement strategy within buckets
//! - Better memory locality

use crate::{shogi::Move, util};
use util::sync_compat::{AtomicU64, Ordering};

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
const MAX_GENERATION_VALUE: u16 = (1 << 8) - 1; // Maximum value before adding AGE_BITS range
pub(crate) const GENERATION_CYCLE: u16 = MAX_GENERATION_VALUE + (1 << AGE_BITS); // 255 + 8 = 263
#[allow(dead_code)]
const GENERATION_CYCLE_MASK: u16 = (1 << (8 + AGE_BITS)) - 1; // For efficient modulo operation

// Key uses upper 32 bits of hash (lower 32 bits used for indexing)
const KEY_SHIFT: u8 = 32;

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
        // Use upper 32 bits of key
        let key = (params.key >> KEY_SHIFT) << KEY_SHIFT;

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
        self.key == ((key >> KEY_SHIFT) << KEY_SHIFT)
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
        let target_key = (key >> KEY_SHIFT) << KEY_SHIFT;

        // Try SIMD-optimized path first
        if self.probe_simd_available() {
            return self.probe_simd_impl(target_key);
        }

        // Fallback to scalar implementation
        self.probe_scalar(target_key)
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
        // Use Relaxed ordering for initial loads to avoid unnecessary barriers
        let mut keys = [0u64; BUCKET_SIZE];
        for (i, key) in keys.iter_mut().enumerate() {
            *key = self.entries[i * 2].load(Ordering::Relaxed);
        }

        // Use SIMD to find matching key
        if let Some(idx) = crate::search::tt_simd::simd::find_matching_key(&keys, target_key) {
            // Add acquire fence before reading data to ensure consistency
            std::sync::atomic::fence(Ordering::Acquire);
            let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
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
        let mut matched_key = 0u64;

        // Load keys with early termination
        for i in 0..BUCKET_SIZE {
            let key = self.entries[i * 2].load(Ordering::Relaxed);
            if key == target_key {
                matching_idx = Some(i);
                matched_key = key;
                break; // Early termination - key optimization
            }
        }

        // If we found a match, apply single memory fence and load data
        if let Some(idx) = matching_idx {
            // Single acquire fence for synchronization
            std::sync::atomic::fence(Ordering::Acquire);

            let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
            let entry = TTEntry {
                key: matched_key,
                data,
            };

            if entry.depth() > 0 {
                return Some(entry);
            }
        }

        None
    }

    /// Store entry in bucket using improved replacement strategy with SIMD
    fn store(&self, new_entry: TTEntry, current_age: u8) {
        let target_key = new_entry.key;

        // First pass: look for exact match or empty slot
        for i in 0..BUCKET_SIZE {
            let idx = i * 2;

            // Use CAS for atomic update to avoid race conditions
            // Reduced retries and exponential backoff for better performance under contention
            const MAX_CAS_RETRIES: u32 = 4; // Reduced from 8 - fail fast is better
            let mut retry_count = 0;

            loop {
                // Use Relaxed for speculative read in CAS loop
                let old_key = self.entries[idx].load(Ordering::Relaxed);

                if old_key == 0 || old_key == target_key {
                    // Try to atomically update the key
                    match self.entries[idx].compare_exchange_weak(
                        old_key,
                        new_entry.key,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            // Key successfully updated, now store the data
                            self.entries[idx + 1].store(new_entry.data, Ordering::Release);
                            return;
                        }
                        Err(_) => {
                            retry_count += 1;
                            if retry_count >= MAX_CAS_RETRIES {
                                // Too many retries, give up on this slot
                                break;
                            }

                            // Another thread modified the entry, retry if still applicable
                            let current_key = self.entries[idx].load(Ordering::Relaxed);
                            if current_key != 0 && current_key != target_key {
                                // This slot is now occupied by a different position, try next slot
                                break;
                            }
                            // Otherwise, retry the CAS operation
                            // Exponential backoff to reduce contention
                            for _ in 0..(1 << retry_count.min(3)) {
                                std::hint::spin_loop();
                            }
                        }
                    }
                } else {
                    // Slot is occupied by different position, try next
                    break;
                }
            }
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

            // Attempt atomic update of the key
            if self.entries[idx]
                .compare_exchange(old_key, new_entry.key, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                // Key successfully updated, now store the data
                self.entries[idx + 1].store(new_entry.data, Ordering::Release);
            }
            // If CAS failed, another thread updated this entry - we accept this race
            // as it's not critical (both threads are storing valid entries)
        }
    }

    /// Check if SIMD store optimization is available
    #[inline]
    fn store_simd_available(&self) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            std::is_x86_feature_detected!("avx2") || std::is_x86_feature_detected!("sse2")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
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
        // Use Relaxed ordering since we're just reading for priority calculation
        for i in 0..BUCKET_SIZE {
            let idx = i * 2;
            let key = self.entries[idx].load(Ordering::Relaxed);
            if key == 0 {
                // Mark empty slots
                is_empty[i] = true;
                depths[i] = 0;
                ages[i] = 0;
                is_pv[i] = false;
                is_exact[i] = false;
            } else {
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

    /// Probe bucket for matching entry
    fn probe(&self, key: u64) -> Option<TTEntry> {
        let target_key = (key >> KEY_SHIFT) << KEY_SHIFT;

        match self.size {
            BucketSize::Small => self.probe_4(target_key),
            BucketSize::Medium => self.probe_8(target_key),
            BucketSize::Large => self.probe_16(target_key),
        }
    }

    /// Probe 4-entry bucket (current implementation)
    fn probe_4(&self, target_key: u64) -> Option<TTEntry> {
        // Try SIMD-optimized path first
        if self.probe_simd_available() {
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
        #[cfg(target_arch = "x86_64")]
        {
            std::is_x86_feature_detected!("avx2") || std::is_x86_feature_detected!("sse2")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    /// SIMD probe for 4 entries
    fn probe_simd_4(&self, target_key: u64) -> Option<TTEntry> {
        let mut keys = [0u64; 4];
        for (i, key) in keys.iter_mut().enumerate() {
            *key = self.entries[i * 2].load(Ordering::Relaxed);
        }

        if let Some(idx) = crate::search::tt_simd::simd::find_matching_key(&keys, target_key) {
            // Add acquire fence before reading data
            std::sync::atomic::fence(Ordering::Acquire);
            let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
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
            *key = self.entries[i * 2].load(Ordering::Relaxed);
        }

        if let Some(idx) = crate::search::tt_simd::simd::find_matching_key_8(&keys, target_key) {
            // Add acquire fence before reading data
            std::sync::atomic::fence(Ordering::Acquire);
            let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
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

    /// Scalar probe for 4 entries (hybrid: early termination + single fence)
    fn probe_scalar_4(&self, target_key: u64) -> Option<TTEntry> {
        // Hybrid approach: early termination to minimize memory access
        let mut matching_idx = None;
        let mut matched_key = 0u64;

        // Load keys with early termination
        for i in 0..4 {
            let key = self.entries[i * 2].load(Ordering::Relaxed);
            if (key >> KEY_SHIFT) << KEY_SHIFT == target_key {
                matching_idx = Some(i);
                matched_key = key;
                break; // Early termination - key optimization
            }
        }

        // If we found a match, apply single memory fence and load data
        if let Some(idx) = matching_idx {
            // Single acquire fence for synchronization
            std::sync::atomic::fence(Ordering::Acquire);

            let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
            let entry = TTEntry {
                key: matched_key,
                data,
            };

            if entry.depth() > 0 {
                return Some(entry);
            }
        }
        None
    }

    /// Scalar probe for 8 entries (hybrid: early termination + single fence)
    fn probe_scalar_8(&self, target_key: u64) -> Option<TTEntry> {
        // Hybrid approach: early termination to minimize memory access
        let mut matching_idx = None;
        let mut matched_key = 0u64;

        // Load keys with early termination
        for i in 0..8 {
            let key = self.entries[i * 2].load(Ordering::Relaxed);
            if (key >> KEY_SHIFT) << KEY_SHIFT == target_key {
                matching_idx = Some(i);
                matched_key = key;
                break; // Early termination - key optimization
            }
        }

        // If we found a match, apply single memory fence and load data
        if let Some(idx) = matching_idx {
            // Single acquire fence for synchronization
            std::sync::atomic::fence(Ordering::Acquire);

            let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
            let entry = TTEntry {
                key: matched_key,
                data,
            };

            if entry.depth() > 0 {
                return Some(entry);
            }
        }
        None
    }

    /// Scalar probe for 16 entries (hybrid: early termination + single fence)
    fn probe_scalar_16(&self, target_key: u64) -> Option<TTEntry> {
        // Hybrid approach: early termination to minimize memory access
        let mut matching_idx = None;
        let mut matched_key = 0u64;

        // Load keys with early termination
        for i in 0..16 {
            let key = self.entries[i * 2].load(Ordering::Relaxed);
            if (key >> KEY_SHIFT) << KEY_SHIFT == target_key {
                matching_idx = Some(i);
                matched_key = key;
                break; // Early termination - key optimization
            }
        }

        // If we found a match, apply single memory fence and load data
        if let Some(idx) = matching_idx {
            // Single acquire fence for synchronization
            std::sync::atomic::fence(Ordering::Acquire);

            let data = self.entries[idx * 2 + 1].load(Ordering::Relaxed);
            let entry = TTEntry {
                key: matched_key,
                data,
            };

            if entry.depth() > 0 {
                return Some(entry);
            }
        }
        None
    }

    /// Store entry in bucket
    fn store(&self, params: TTEntryParams, current_age: u8) {
        match self.size {
            BucketSize::Small => self.store_4(params, current_age),
            BucketSize::Medium => self.store_8(params, current_age),
            BucketSize::Large => self.store_16(params, current_age),
        }
    }

    /// Store in 4-entry bucket
    fn store_4(&self, params: TTEntryParams, current_age: u8) {
        let new_entry = TTEntry::from_params(params);
        let target_key = (params.key >> KEY_SHIFT) << KEY_SHIFT;

        // Check for existing entry
        for i in 0..4 {
            let idx = i * 2;
            let old_key = self.entries[idx].load(Ordering::Relaxed);

            if (old_key >> KEY_SHIFT) << KEY_SHIFT == target_key {
                // Update existing entry
                self.entries[idx].store(new_entry.key, Ordering::Release);
                self.entries[idx + 1].store(new_entry.data, Ordering::Release);
                return;
            }
        }

        // Find worst entry to replace
        let (worst_idx, _) = self.find_worst_entry_4(current_age);
        let idx = worst_idx * 2;

        // Store new entry
        self.entries[idx].store(new_entry.key, Ordering::Release);
        self.entries[idx + 1].store(new_entry.data, Ordering::Release);
    }

    /// Store in 8-entry bucket
    fn store_8(&self, params: TTEntryParams, current_age: u8) {
        let new_entry = TTEntry::from_params(params);
        let target_key = (params.key >> KEY_SHIFT) << KEY_SHIFT;

        // Check for existing entry
        for i in 0..8 {
            let idx = i * 2;
            let old_key = self.entries[idx].load(Ordering::Relaxed);

            if (old_key >> KEY_SHIFT) << KEY_SHIFT == target_key {
                // Update existing entry
                self.entries[idx].store(new_entry.key, Ordering::Release);
                self.entries[idx + 1].store(new_entry.data, Ordering::Release);
                return;
            }
        }

        // Find worst entry to replace
        let (worst_idx, _) = self.find_worst_entry_8(current_age);
        let idx = worst_idx * 2;

        // Store new entry
        self.entries[idx].store(new_entry.key, Ordering::Release);
        self.entries[idx + 1].store(new_entry.data, Ordering::Release);
    }

    /// Store in 16-entry bucket
    fn store_16(&self, params: TTEntryParams, current_age: u8) {
        let new_entry = TTEntry::from_params(params);
        let target_key = (params.key >> KEY_SHIFT) << KEY_SHIFT;

        // Check for existing entry
        for i in 0..16 {
            let idx = i * 2;
            let old_key = self.entries[idx].load(Ordering::Relaxed);

            if (old_key >> KEY_SHIFT) << KEY_SHIFT == target_key {
                // Update existing entry
                self.entries[idx].store(new_entry.key, Ordering::Release);
                self.entries[idx + 1].store(new_entry.data, Ordering::Release);
                return;
            }
        }

        // Find worst entry to replace (scalar for now)
        let (worst_idx, _) = self.find_worst_entry_16(current_age);
        let idx = worst_idx * 2;

        // Store new entry
        self.entries[idx].store(new_entry.key, Ordering::Release);
        self.entries[idx + 1].store(new_entry.data, Ordering::Release);
    }

    /// Find worst entry in 4-entry bucket using SIMD
    fn find_worst_entry_4(&self, current_age: u8) -> (usize, i32) {
        // Prepare data for SIMD processing
        let mut depths = [0u8; 4];
        let mut ages = [0u8; 4];
        let mut is_pv = [false; 4];
        let mut is_exact = [false; 4];
        let mut is_empty = [false; 4];

        for i in 0..4 {
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
        let mut scores = crate::search::tt_simd::simd::calculate_priority_scores(
            &depths,
            &ages,
            &is_pv,
            &is_exact,
            current_age,
        );

        // Set empty entries to minimum priority
        for (i, empty) in is_empty.iter().enumerate() {
            if *empty {
                scores[i] = i32::MIN;
            }
        }

        // Find minimum score
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

    /// Find worst entry in 8-entry bucket using SIMD
    fn find_worst_entry_8(&self, current_age: u8) -> (usize, i32) {
        // Prepare data for SIMD processing
        let mut depths = [0u8; 8];
        let mut ages = [0u8; 8];
        let mut is_pv = [false; 8];
        let mut is_exact = [false; 8];
        let mut is_empty = [false; 8];

        for i in 0..8 {
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
        let mut scores = crate::search::tt_simd::simd::calculate_priority_scores_8(
            &depths,
            &ages,
            &is_pv,
            &is_exact,
            current_age,
        );

        // Set empty entries to minimum priority
        for (i, empty) in is_empty.iter().enumerate() {
            if *empty {
                scores[i] = i32::MIN;
            }
        }

        // Find minimum score
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

    /// Find worst entry in 16-entry bucket (scalar for now)
    fn find_worst_entry_16(&self, current_age: u8) -> (usize, i32) {
        let mut worst_idx = 0;
        let mut worst_score = i32::MAX;

        for i in 0..16 {
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

        TranspositionTable {
            buckets,
            flexible_buckets: None,
            num_buckets,
            age: 0,
            bucket_size: None,
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

        TranspositionTable {
            buckets: Vec::new(), // Empty for flexible mode
            flexible_buckets: Some(flexible_buckets),
            num_buckets,
            age: 0,
            bucket_size: Some(bucket_size),
        }
    }

    /// Get bucket index from zobrist hash
    #[inline]
    fn bucket_index(&self, hash: u64) -> usize {
        (hash as usize) & (self.num_buckets - 1)
    }

    /// Probe the transposition table
    pub fn probe(&self, hash: u64) -> Option<TTEntry> {
        let idx = self.bucket_index(hash);

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            flexible_buckets[idx].probe(hash)
        } else {
            self.buckets[idx].probe(hash)
        }
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

        let idx = self.bucket_index(params.key);

        if let Some(ref flexible_buckets) = self.flexible_buckets {
            flexible_buckets[idx].store(params, self.age);
        } else {
            let entry = TTEntry::from_params(params);
            self.buckets[idx].store(entry, self.age);
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

        ((filled * 1000) / total) as u16
    }

    /// Get table size in entries
    pub fn size(&self) -> usize {
        self.num_buckets * BUCKET_SIZE
    }

    /// Prefetch bucket for the given hash
    #[inline]
    pub fn prefetch(&self, hash: u64) {
        let idx = self.bucket_index(hash);
        let bucket_ptr = &self.buckets[idx] as *const TTBucket;

        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::_mm_prefetch;
            _mm_prefetch(bucket_ptr as *const i8, 3); // _MM_HINT_T0
        }

        // ARM64 prefetch - currently requires nightly Rust
        // We conditionally compile based on whether we're using nightly
        #[cfg(target_arch = "aarch64")]
        {
            // On nightly Rust (like in CI), we can use prefetch
            // This uses a trick: we try to detect nightly at compile time
            #[cfg(feature = "nightly")]
            unsafe {
                use std::arch::aarch64::_prefetch;
                _prefetch(bucket_ptr as *const i8, 0, 3); // Read, L1 cache
            }

            // On stable Rust, prefetch is not available
            #[cfg(not(feature = "nightly"))]
            {
                // No-op on stable builds to avoid compilation errors
                let _ = bucket_ptr; // Avoid unused variable warning
            }
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
    use crate::shogi::{Move, Square};

    // Test SIMD probe produces same results as scalar
    #[test]
    fn test_tt_probe_simd_vs_scalar() {
        let bucket = TTBucket::new();
        let test_entry = TTEntry::new(0x1234567890ABCDEF, None, 100, -50, 10, NodeType::Exact, 0);

        // Store an entry
        bucket.store(test_entry, 0);

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
            bucket.store(entry, 0);
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

    #[test]
    fn test_bucket_operations() {
        let bucket = TTBucket::new();
        let hash1 = 0x1234567890ABCDEF;
        let mv = Some(Move::normal(Square::new(2, 7), Square::new(2, 6), false));

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
        let mv = Some(Move::normal(Square::new(7, 7), Square::new(7, 6), false));

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
        tt.prefetch(hash);

        // Verify entry is still accessible
        let entry = tt.probe(hash);
        assert!(entry.is_some());
    }

    #[test]
    fn test_cache_line_optimization() {
        // Test that accessing entries in the same bucket is fast
        let tt = TranspositionTable::new(1);

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
            bucket.store(entry, 0);
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
            bucket.store(params, 0);

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
            bucket.store(params, 0);
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
        bucket.store(new_params, 0);

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
            bucket.store(params, 0);
        }

        // Test both SIMD and scalar probe paths
        for (i, &hash) in test_hashes.iter().enumerate() {
            // SIMD path
            if bucket.probe_simd_available() {
                let simd_result = bucket.probe_simd_8(hash >> KEY_SHIFT << KEY_SHIFT);
                assert!(simd_result.is_some(), "SIMD probe failed for entry {i}");
            }

            // Scalar path
            let scalar_result = bucket.probe_scalar_8(hash >> KEY_SHIFT << KEY_SHIFT);
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
}
