//! Optimized transposition table with bucket structure
//!
//! This implementation uses a bucket structure to optimize cache performance:
//! - 4 entries per bucket (64 bytes = 1 cache line)
//! - Improved replacement strategy within buckets
//! - Better memory locality

use crate::{shogi::Move, util};
use util::sync_compat::{AtomicU64, Ordering};

// Bit layout constants for TTEntry data field
const MOVE_SHIFT: u8 = 48;
const MOVE_BITS: u8 = 16;
const MOVE_MASK: u64 = (1 << MOVE_BITS) - 1;
const SCORE_SHIFT: u8 = 32;
const SCORE_BITS: u8 = 16;
const SCORE_MASK: u64 = (1 << SCORE_BITS) - 1;
const DEPTH_SHIFT: u8 = 25;
const DEPTH_BITS: u8 = 7;
const DEPTH_MASK: u8 = (1 << DEPTH_BITS) - 1;
const NODE_TYPE_SHIFT: u8 = 23;
const NODE_TYPE_BITS: u8 = 2;
const NODE_TYPE_MASK: u8 = (1 << NODE_TYPE_BITS) - 1;
const AGE_SHIFT: u8 = 20;
const AGE_BITS: u8 = 3; // Reduced from 6 to 3 bits (0-7)
const AGE_MASK: u8 = (1 << AGE_BITS) - 1;
const EVAL_BITS: u8 = 16;
const EVAL_MASK: u64 = (1 << EVAL_BITS) - 1;

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

/// Transposition table entry (16 bytes)
#[derive(Clone, Copy, Default)]
#[repr(C, align(16))]
pub struct TTEntry {
    key: u64,
    data: u64,
}

impl TTEntry {
    /// Create new TT entry
    pub fn new(
        key: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
        age: u8,
    ) -> Self {
        // Use upper 32 bits of key
        let key = (key >> KEY_SHIFT) << KEY_SHIFT;

        // Pack move into 16 bits
        let move_data = match mv {
            Some(m) => m.to_u16(),
            None => 0,
        };

        // Pack all data into 64 bits:
        // [63-48]: move (16 bits)
        // [47-32]: score (16 bits)
        // [31-25]: depth (7 bits)
        // [24-23]: node type (2 bits)
        // [22-20]: age (3 bits)
        // [19-16]: reserved (4 bits)
        // [15-0]: static eval (16 bits)
        let data = ((move_data as u64) << MOVE_SHIFT)
            | ((score as u16 as u64) << SCORE_SHIFT)
            | (((depth & DEPTH_MASK) as u64) << DEPTH_SHIFT)
            | ((node_type as u64) << NODE_TYPE_SHIFT)
            | (((age & AGE_MASK) as u64) << AGE_SHIFT)
            | (eval as u16 as u64);

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
        Some(Move::from_u16(move_data))
    }

    /// Get score from entry
    #[inline]
    pub fn score(&self) -> i16 {
        ((self.data >> SCORE_SHIFT) & SCORE_MASK) as i16
    }

    /// Get static evaluation from entry
    #[inline]
    pub fn eval(&self) -> i16 {
        (self.data & EVAL_MASK) as i16
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
            _ => NodeType::Exact, // Default to Exact for corrupted data
        }
    }

    /// Get age
    #[inline]
    pub fn age(&self) -> u8 {
        ((self.data >> AGE_SHIFT) & AGE_MASK as u64) as u8
    }

    /// Calculate replacement priority score (higher = more valuable to keep)
    #[inline]
    fn priority_score(&self, current_age: u8) -> i32 {
        if self.is_empty() {
            return -1000; // Empty entries have lowest priority
        }

        let mut score = 0;

        // Age factor: current generation entries are preferred
        if self.age() == current_age {
            score += 100;
        }

        // Depth factor: deeper searches are more valuable
        score += self.depth() as i32 * 10;

        // Node type factor: exact nodes are most valuable
        if self.node_type() == NodeType::Exact {
            score += 50;
        }

        score
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
            let old_key = self.entries[idx].load(Ordering::Acquire);

            if old_key == 0 || old_key == target_key {
                // Empty slot or same position - store here
                self.entries[idx + 1].store(new_entry.data, Ordering::Release);
                self.entries[idx].store(new_entry.key, Ordering::Release);
                return;
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
            self.entries[idx + 1].store(new_entry.data, Ordering::Release);
            self.entries[idx].store(new_entry.key, Ordering::Release);
        }
    }
}

/// Optimized transposition table with bucket structure
pub struct TranspositionTableV2 {
    /// Table buckets
    buckets: Vec<TTBucket>,
    /// Number of buckets
    num_buckets: usize,
    /// Current age/generation (3 bits: 0-7)
    age: u8,
}

impl TranspositionTableV2 {
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

        TranspositionTableV2 {
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
        let entry = TTEntry::new(hash, mv, score, eval, depth, node_type, self.age);
        let idx = self.bucket_index(hash);
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
    fn test_transposition_table_v2() {
        let tt = TranspositionTableV2::new(1);
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
        let mut tt = TranspositionTableV2::new(1);
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
        let tt = TranspositionTableV2::new(1);
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
        let tt = TranspositionTableV2::new(1);

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
}
