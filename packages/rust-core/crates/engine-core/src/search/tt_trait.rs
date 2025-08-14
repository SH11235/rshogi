//! Trait for transposition tables to allow using both regular and sharded implementations

use super::tt::{
    entry::{NodeType, TTEntry, TTEntryParams},
    TranspositionTable,
};
use crate::shogi::Move;

/// Common interface for transposition tables
pub trait TranspositionTableTrait: Send + Sync {
    /// Probe the transposition table
    fn probe(&self, hash: u64) -> Option<TTEntry>;

    /// Store an entry in the transposition table
    fn store(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    );

    /// Store entry and check if it was new
    fn store_and_check_new(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) -> bool;

    /// Store with parameters
    fn store_with_params(&self, params: TTEntryParams);

    /// Set exact cut flag for ABDADA
    fn set_exact_cut(&self, hash: u64) -> bool;

    /// Clear exact cut flag
    fn clear_exact_cut(&self, hash: u64) -> bool;

    /// Prefetch a hash for future access
    fn prefetch(&self, hash: u64, hint: i32);

    /// Get hashfull estimate
    fn hashfull(&self) -> u16;
}

/// Implement the trait for TranspositionTable
impl TranspositionTableTrait for TranspositionTable {
    fn probe(&self, hash: u64) -> Option<TTEntry> {
        self.probe(hash)
    }

    fn store(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) {
        self.store(hash, mv, score, eval, depth, node_type)
    }

    fn store_and_check_new(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) -> bool {
        self.store_and_check_new(hash, mv, score, eval, depth, node_type)
    }

    fn store_with_params(&self, params: TTEntryParams) {
        self.store_with_params(params)
    }

    fn set_exact_cut(&self, hash: u64) -> bool {
        self.set_exact_cut(hash)
    }

    fn clear_exact_cut(&self, hash: u64) -> bool {
        self.clear_exact_cut(hash)
    }

    fn prefetch(&self, hash: u64, hint: i32) {
        self.prefetch(hash, hint)
    }

    fn hashfull(&self) -> u16 {
        self.hashfull()
    }
}

/// Implement the trait for ShardedTranspositionTable
impl TranspositionTableTrait for super::ShardedTranspositionTable {
    fn probe(&self, hash: u64) -> Option<TTEntry> {
        self.probe(hash)
    }

    fn store(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) {
        self.store(hash, mv, score, eval, depth, node_type)
    }

    fn store_and_check_new(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) -> bool {
        self.store_and_check_new(hash, mv, score, eval, depth, node_type)
    }

    fn store_with_params(&self, params: TTEntryParams) {
        self.store_with_params(params)
    }

    fn set_exact_cut(&self, hash: u64) -> bool {
        self.set_exact_cut(hash)
    }

    fn clear_exact_cut(&self, hash: u64) -> bool {
        self.clear_exact_cut(hash)
    }

    fn prefetch(&self, hash: u64, hint: i32) {
        self.prefetch(hash, hint)
    }

    fn hashfull(&self) -> u16 {
        self.hashfull()
    }
}
