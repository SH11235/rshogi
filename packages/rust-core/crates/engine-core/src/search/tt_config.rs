//! Transposition table configuration module
//!
//! Provides a unified interface for choosing between TT implementations

use super::{tt, tt_v2};

/// Transposition table version selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TTVersion {
    /// Original TT implementation
    V1,
    /// Optimized TT with bucket structure
    #[default]
    V2,
}

/// Unified transposition table interface
///
/// This enum provides a common interface for both TT implementations,
/// allowing runtime selection while maintaining zero-cost abstraction
/// for the actual operations.
pub enum TranspositionTableUnified {
    V1(tt::TranspositionTable),
    V2(tt_v2::TranspositionTableV2),
}

impl TranspositionTableUnified {
    /// Create a new transposition table with the specified version and size
    pub fn new(version: TTVersion, size_mb: usize) -> Self {
        match version {
            TTVersion::V1 => Self::V1(tt::TranspositionTable::new(size_mb)),
            TTVersion::V2 => Self::V2(tt_v2::TranspositionTableV2::new(size_mb)),
        }
    }

    /// Create with default version (V2)
    pub fn new_default(size_mb: usize) -> Self {
        Self::new(TTVersion::default(), size_mb)
    }

    /// Probe the transposition table
    pub fn probe(&self, hash: u64) -> Option<tt::TTEntry> {
        match self {
            Self::V1(tt) => tt.probe(hash),
            Self::V2(tt) => {
                // Convert from V2 entry to V1 entry for compatibility
                tt.probe(hash).map(|e| {
                    // Reconstruct a V1 TTEntry using the V2 data
                    tt::TTEntry::new(
                        hash, // Use the hash as key
                        e.get_move(),
                        e.score(),
                        e.eval(),
                        e.depth(),
                        match e.node_type() {
                            tt_v2::NodeType::Exact => tt::NodeType::Exact,
                            tt_v2::NodeType::LowerBound => tt::NodeType::LowerBound,
                            tt_v2::NodeType::UpperBound => tt::NodeType::UpperBound,
                        },
                        e.age(),
                    )
                })
            }
        }
    }

    /// Store an entry in the transposition table
    pub fn store(
        &self,
        hash: u64,
        best_move: Option<crate::shogi::Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: tt::NodeType,
    ) {
        match self {
            Self::V1(tt) => tt.store(hash, best_move, score, eval, depth, node_type),
            Self::V2(tt) => {
                let node_type_v2 = match node_type {
                    tt::NodeType::Exact => tt_v2::NodeType::Exact,
                    tt::NodeType::LowerBound => tt_v2::NodeType::LowerBound,
                    tt::NodeType::UpperBound => tt_v2::NodeType::UpperBound,
                };
                tt.store(hash, best_move, score, eval, depth, node_type_v2);
            }
        }
    }

    /// Clear the transposition table
    pub fn clear(&mut self) {
        match self {
            Self::V1(tt) => tt.clear(),
            Self::V2(tt) => tt.clear(),
        }
    }

    /// Start a new search (increment age)
    pub fn new_search(&mut self) {
        match self {
            Self::V1(tt) => tt.new_search(),
            Self::V2(tt) => tt.new_search(),
        }
    }

    /// Get hash fullness in per mille
    pub fn hashfull(&self) -> usize {
        match self {
            Self::V1(tt) => tt.hashfull().into(),
            Self::V2(tt) => tt.hashfull().into(),
        }
    }

    /// Prefetch an entry
    pub fn prefetch(&self, hash: u64) {
        match self {
            Self::V1(tt) => tt.prefetch(hash),
            Self::V2(tt) => tt.prefetch(hash),
        }
    }

    /// Get the current version
    pub fn version(&self) -> TTVersion {
        match self {
            Self::V1(_) => TTVersion::V1,
            Self::V2(_) => TTVersion::V2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_interface() {
        // Test V1
        let tt_v1 = TranspositionTableUnified::new(TTVersion::V1, 1);
        tt_v1.store(0x123456789ABCDEF, None, 100, 50, 10, tt::NodeType::Exact);
        let entry = tt_v1.probe(0x123456789ABCDEF);
        assert!(entry.is_some());

        // Test V2
        let tt_v2 = TranspositionTableUnified::new(TTVersion::V2, 1);
        tt_v2.store(0x123456789ABCDEF, None, 100, 50, 10, tt::NodeType::Exact);
        let entry = tt_v2.probe(0x123456789ABCDEF);
        assert!(entry.is_some());
    }

    #[test]
    fn test_version_switching() {
        let tt = TranspositionTableUnified::new_default(1);
        assert_eq!(tt.version(), TTVersion::V2);

        let tt = TranspositionTableUnified::new(TTVersion::V1, 1);
        assert_eq!(tt.version(), TTVersion::V1);
    }
}
