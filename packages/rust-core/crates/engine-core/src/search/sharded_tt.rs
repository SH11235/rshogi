//! Sharded transposition table for improved cache locality and reduced contention
//!
//! This implementation divides the transposition table into multiple shards,
//! each operating independently to reduce cache line conflicts and improve
//! parallel performance.

use super::tt::{NodeType, TTEntry, TTEntryParams, TranspositionTable};
use crate::shogi::Move;
use std::sync::Arc;

/// Number of shards (should be power of 2 for efficient modulo)
const NUM_SHARDS: usize = 16;

/// Sharded transposition table with multiple independent TT instances
pub struct ShardedTranspositionTable {
    /// Individual TT shards
    shards: Vec<TranspositionTable>,
    /// Number of shards (cached for performance)
    num_shards: usize,
    /// Current age/generation
    age: u8,
}

impl ShardedTranspositionTable {
    /// Create a new sharded transposition table with the given total size in MB
    pub fn new(total_size_mb: usize) -> Self {
        // Dynamic shard count: use fewer shards for small sizes to ensure each shard gets at least 1MB
        // Find the largest power of 2 <= total_size_mb, but not more than NUM_SHARDS
        let num_shards = if total_size_mb == 0 {
            1 // Special case: 0MB gets 1 shard
        } else {
            // Find power of 2: 1, 2, 4, 8, 16
            let mut shards = 1;
            while shards * 2 <= total_size_mb && shards * 2 <= NUM_SHARDS {
                shards *= 2;
            }
            shards
        };

        // Distribute size across shards with remainder handling
        let base_size = total_size_mb / num_shards;
        let remainder = total_size_mb % num_shards;

        // Create independent TT shards with distributed sizes
        let shards: Vec<TranspositionTable> = (0..num_shards)
            .map(|i| {
                // First 'remainder' shards get base_size + 1 MB
                // Remaining shards get base_size MB
                let size_mb = base_size + if i < remainder { 1 } else { 0 };
                TranspositionTable::new(size_mb)
            })
            .collect();

        Self {
            shards,
            num_shards,
            age: 0,
        }
    }

    /// Get the shard index for a given hash
    #[inline(always)]
    fn shard_index(&self, hash: u64) -> usize {
        // Use lower bits for shard selection (better distribution)
        (hash as usize) & (self.num_shards - 1)
    }

    /// Probe the transposition table
    #[inline]
    pub fn probe(&self, hash: u64) -> Option<TTEntry> {
        let shard_idx = self.shard_index(hash);
        self.shards[shard_idx].probe(hash)
    }

    /// Store an entry in the transposition table
    #[inline]
    pub fn store(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) {
        let shard_idx = self.shard_index(hash);
        self.shards[shard_idx].store(hash, mv, score, eval, depth, node_type);
    }

    /// Store entry and check if it was new
    #[inline]
    pub fn store_and_check_new(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) -> bool {
        let shard_idx = self.shard_index(hash);
        self.shards[shard_idx].store_and_check_new(hash, mv, score, eval, depth, node_type)
    }

    /// Store with parameters
    #[inline]
    pub fn store_with_params(&self, params: TTEntryParams) {
        let shard_idx = self.shard_index(params.key);
        self.shards[shard_idx].store_with_params(params);
    }

    /// Set exact cut flag for ABDADA
    #[inline]
    pub fn set_exact_cut(&self, hash: u64) -> bool {
        let shard_idx = self.shard_index(hash);
        self.shards[shard_idx].set_exact_cut(hash)
    }

    /// Clear exact cut flag
    #[inline]
    pub fn clear_exact_cut(&self, hash: u64) -> bool {
        let shard_idx = self.shard_index(hash);
        self.shards[shard_idx].clear_exact_cut(hash)
    }

    /// Prefetch a hash for future access
    #[inline]
    pub fn prefetch(&self, hash: u64, hint: i32) {
        let shard_idx = self.shard_index(hash);
        self.shards[shard_idx].prefetch(hash, hint);
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        for shard in &mut self.shards {
            shard.clear();
        }
        self.age = 0;
    }

    /// Advance generation/age
    pub fn new_search(&mut self) {
        self.age = self.age.wrapping_add(1);
        for shard in &mut self.shards {
            shard.new_search();
        }
    }

    /// Get current age
    pub fn age(&self) -> u8 {
        self.age
    }

    /// Get hashfull estimate (average across all shards)
    pub fn hashfull(&self) -> u16 {
        let sum: u32 = self.shards.iter().map(|shard| shard.hashfull() as u32).sum();
        (sum / self.num_shards as u32) as u16
    }

    /// Get total size in MB
    pub fn size_mb(&self) -> usize {
        // Sum bytes first, then convert to MB to avoid rounding errors
        let total_bytes: usize = self.shards.iter().map(|shard| shard.size_bytes()).sum();
        // Round up to nearest MB
        total_bytes.div_ceil(1024 * 1024)
    }

    /// Check if exact cut flag is set
    pub fn has_exact_cut(&self, hash: u64) -> bool {
        let shard_idx = self.shard_index(hash);
        // Check if the entry exists and has the exact cut flag
        if let Some(entry) = self.shards[shard_idx].probe(hash) {
            // Check if this is an exact node
            entry.node_type() == NodeType::Exact
        } else {
            false
        }
    }

    /// Check if garbage collection should be triggered
    pub fn should_trigger_gc(&self) -> bool {
        // Since sharded TT manages memory independently per shard,
        // we don't need global GC. Return false.
        false
    }

    /// Perform incremental garbage collection
    pub fn incremental_gc(&self, _batch_size: usize) {
        // No-op for sharded TT as each shard manages its own memory
    }

    /// Prefetch to L1 cache
    pub fn prefetch_l1(&self, hash: u64) {
        let shard_idx = self.shard_index(hash);
        self.shards[shard_idx].prefetch_l1(hash);
    }
}

/// Thread-safe reference to sharded TT
pub type SharedShardedTT = Arc<ShardedTranspositionTable>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sharded_tt_basic() {
        let tt = ShardedTranspositionTable::new(16);

        // Test store and probe
        let hash = 0x123456789ABCDEF0;
        tt.store(hash, None, 100, 50, 5, NodeType::Exact);

        let entry = tt.probe(hash);
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.score(), 100);
        assert_eq!(entry.depth(), 5);
        assert_eq!(entry.node_type(), NodeType::Exact);
    }

    #[test]
    fn test_shard_distribution() {
        let tt = ShardedTranspositionTable::new(16);

        // Test that different hashes go to different shards
        let hash1 = 0x0000000000000001;
        let hash2 = 0x0000000000000002;

        assert_ne!(tt.shard_index(hash1), tt.shard_index(hash2));
    }

    #[test]
    fn test_exact_cut() {
        let tt = ShardedTranspositionTable::new(16);

        let hash = 0xFEDCBA9876543210;
        tt.store(hash, None, 200, 100, 8, NodeType::Exact);

        // Should have exact cut since we stored an Exact node
        assert!(tt.has_exact_cut(hash));

        // Non-existent hash should not have exact cut
        assert!(!tt.has_exact_cut(0x1111111111111111));
    }

    #[test]
    fn test_total_size_exact_match() {
        // Test that total size matches requested size exactly

        // USI_Hash = 1 should give 1MB total
        let tt1 = ShardedTranspositionTable::new(1);
        assert_eq!(tt1.size_mb(), 1, "1MB should give exactly 1MB total");

        // USI_Hash = 16 should give 16MB total
        let tt16 = ShardedTranspositionTable::new(16);
        assert_eq!(tt16.size_mb(), 16, "16MB should give exactly 16MB total");

        // USI_Hash = 17 should give 17MB total
        let tt17 = ShardedTranspositionTable::new(17);
        assert_eq!(tt17.size_mb(), 17, "17MB should give exactly 17MB total");

        // USI_Hash = 64 should give 64MB total
        let tt64 = ShardedTranspositionTable::new(64);
        assert_eq!(tt64.size_mb(), 64, "64MB should give exactly 64MB total");
    }

    #[test]
    fn test_small_sizes() {
        // Test very small sizes (< NUM_SHARDS)
        for size in 1..NUM_SHARDS {
            let tt = ShardedTranspositionTable::new(size);
            let actual_size = tt.size_mb();
            assert_eq!(actual_size, size, "Requested {size}MB but got {actual_size}MB");
        }
    }
}
