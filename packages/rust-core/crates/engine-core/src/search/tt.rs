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
const AGE_MASK: u8 = (1 << AGE_BITS) - 1;
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
const GENERATION_CYCLE: u16 = MAX_GENERATION_VALUE + (1 << AGE_BITS); // 255 + 8 = 263
#[allow(dead_code)]
const GENERATION_CYCLE_MASK: u16 = (1 << (8 + AGE_BITS)) - 1; // For efficient modulo operation

// Key uses upper 32 bits of hash (lower 32 bits used for indexing)
const KEY_SHIFT: u8 = 32;

/// Number of entries per bucket
const BUCKET_SIZE: usize = 4;

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

    /// Probe bucket for matching entry
    fn probe(&self, key: u64) -> Option<TTEntry> {
        let target_key = (key >> KEY_SHIFT) << KEY_SHIFT;

        for i in 0..BUCKET_SIZE {
            let idx = i * 2;
            let entry_key = self.entries[idx].load(Ordering::Acquire);

            if entry_key == target_key {
                let data = self.entries[idx + 1].load(Ordering::Acquire);
                let entry = TTEntry {
                    key: entry_key,
                    data,
                };

                if entry.depth() > 0 {
                    return Some(entry);
                }
            }
        }

        None
    }

    /// Store entry in bucket using improved replacement strategy
    fn store(&self, new_entry: TTEntry, current_age: u8) {
        let target_key = new_entry.key;

        // First pass: look for exact match or empty slot
        for i in 0..BUCKET_SIZE {
            let idx = i * 2;

            // Use CAS for atomic update to avoid race conditions
            // Limit retries to prevent potential infinite loops under extreme contention
            const MAX_CAS_RETRIES: u32 = 8;
            let mut retry_count = 0;

            loop {
                let old_key = self.entries[idx].load(Ordering::Acquire);

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
                            let current_key = self.entries[idx].load(Ordering::Acquire);
                            if current_key != 0 && current_key != target_key {
                                // This slot is now occupied by a different position, try next slot
                                break;
                            }
                            // Otherwise, retry the CAS operation
                            // Add small backoff to reduce contention
                            std::hint::spin_loop();
                        }
                    }
                } else {
                    // Slot is occupied by different position, try next
                    break;
                }
            }
        }

        // Second pass: find least valuable entry to replace
        let mut worst_idx = 0;
        let mut worst_score = i32::MAX;

        for i in 0..BUCKET_SIZE {
            let idx = i * 2;
            let old_key = self.entries[idx].load(Ordering::Acquire);
            let old_data = self.entries[idx + 1].load(Ordering::Acquire);
            let old_entry = TTEntry {
                key: old_key,
                data: old_data,
            };

            let score = old_entry.priority_score(current_age);
            if score < worst_score {
                worst_score = score;
                worst_idx = i;
            }
        }

        // Check if new entry is more valuable than the worst existing entry
        if new_entry.priority_score(current_age) > worst_score {
            let idx = worst_idx * 2;

            // Use CAS to ensure atomic replacement
            // Note: We don't retry here as we've already determined this is the best slot to replace
            let old_key = self.entries[idx].load(Ordering::Acquire);

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
}

/// Optimized transposition table with bucket structure
pub struct TranspositionTable {
    /// Table buckets
    buckets: Vec<TTBucket>,
    /// Number of buckets
    num_buckets: usize,
    /// Current age/generation (3 bits: 0-7)
    age: u8,
}

impl TranspositionTable {
    /// Create new transposition table with given size in MB
    pub fn new(size_mb: usize) -> Self {
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
            num_buckets,
            age: 0,
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
        self.buckets[idx].probe(hash)
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

        let entry = TTEntry::from_params(params);
        let idx = self.bucket_index(params.key);
        self.buckets[idx].store(entry, self.age);
    }

    /// Clear the transposition table
    pub fn clear(&mut self) {
        for bucket in &mut self.buckets {
            for atomic in &bucket.entries {
                atomic.store(0, Ordering::Relaxed);
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

        #[cfg(target_arch = "aarch64")]
        unsafe {
            use std::arch::aarch64::_prefetch;
            _prefetch(bucket_ptr as *const i8, 0, 3); // Read, L1 cache
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
    use crate::Square;

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
                        "Entry not found for hash {:#x} (thread {}, iteration {})",
                        hash,
                        thread_id,
                        i
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
