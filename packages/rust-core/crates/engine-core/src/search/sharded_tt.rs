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
        let num_shards = NUM_SHARDS;
        let shard_size_mb = total_size_mb.max(num_shards) / num_shards;
        
        // Create independent TT shards
        let shards: Vec<TranspositionTable> = (0..num_shards)
            .map(|_| TranspositionTable::new(shard_size_mb))
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
        let sum: u32 = self.shards
            .iter()
            .map(|shard| shard.hashfull() as u32)
            .sum();
        (sum / self.num_shards as u32) as u16
    }
    
    /// Get total size in MB
    pub fn size_mb(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.size() / (1024 * 1024))
            .sum()
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
}

/// Thread-safe reference to sharded TT
pub type SharedShardedTT = Arc<ShardedTranspositionTable>;

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_sharded_tt_basic() {
        let mut tt = ShardedTranspositionTable::new(16);
        
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
        let mut tt = ShardedTranspositionTable::new(16);
        
        // Test that different hashes go to different shards
        let hash1 = 0x0000000000000001;
        let hash2 = 0x0000000000000002;
        
        assert_ne!(tt.shard_index(hash1), tt.shard_index(hash2));
    }
    
    #[test]
    fn test_exact_cut() {
        let mut tt = ShardedTranspositionTable::new(16);
        
        let hash = 0xFEDCBA9876543210;
        tt.store(hash, None, 200, 100, 8, NodeType::Exact);
        
        // Should have exact cut since we stored an Exact node
        assert!(tt.has_exact_cut(hash));
        
        // Non-existent hash should not have exact cut
        assert!(!tt.has_exact_cut(0x1111111111111111));
    }
}