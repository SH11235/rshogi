/// Error types for move generation
pub mod error;

/// Move list implementation
pub mod movelist;

/// Static tables for move generation
pub mod tables;

/// Core move generator implementation
pub mod generator;

// Re-export main types
pub use error::MoveGenError;
pub use generator::MoveGenerator;
pub use movelist::MoveList;

/// Debug information for move generation
#[derive(Debug, Clone)]
pub struct MoveGenDebugInfo {
    /// Pieces giving check
    pub checkers: crate::shogi::Bitboard,
    /// Pinned pieces
    pub pinned: crate::shogi::Bitboard,
    /// Number of moves generated
    pub move_count: usize,
    /// Time taken for generation (nanoseconds)
    pub generation_time_ns: u64,
}

impl MoveGenDebugInfo {
    /// Create empty debug info
    pub fn empty() -> Self {
        Self {
            checkers: crate::shogi::Bitboard::EMPTY,
            pinned: crate::shogi::Bitboard::EMPTY,
            move_count: 0,
            generation_time_ns: 0,
        }
    }
}