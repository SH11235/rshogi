//! Data structures for opening book conversion
//!
//! This module defines the core data structures used in the YaneuraOu SFEN
//! to binary conversion process.

/// Raw SFEN entry as parsed from the input file
#[derive(Debug, Clone)]
pub struct RawSfenEntry {
    /// Board position in SFEN notation
    pub position: String,
    /// Current turn ('b' for black, 'w' for white)
    pub turn: char,
    /// Pieces in hand notation
    pub hand: String,
    /// Number of moves played from initial position
    pub move_count: u32,
    /// List of moves with evaluations for this position
    pub moves: Vec<RawMove>,
}

/// Raw move data as parsed from the input file
#[derive(Debug, Clone)]
pub struct RawMove {
    /// Move notation (e.g., "7g7f", "P*5f", "3d3c+")
    pub move_notation: String,
    /// Move type (typically "none" in YaneuraOu format)
    pub move_type: String,
    /// Position evaluation after this move (centipawns)
    pub evaluation: i32,
    /// Search depth used for this evaluation
    pub depth: u32,
    /// Number of nodes searched
    pub nodes: u64,
}

/// Compact binary representation of a position
#[derive(Debug, Clone)]
#[repr(C)]
pub struct CompactPosition {
    /// Hash of the position for fast lookup
    pub position_hash: u64, // 8 bytes
    /// Best move encoded as 16-bit integer
    pub best_move: u16, // 2 bytes
    /// Evaluation of the best move
    pub evaluation: i16, // 2 bytes
    /// Search depth of the best move
    pub depth: u8, // 1 byte
    /// Number of alternative moves stored
    pub move_count: u8, // 1 byte
    /// Usage frequency indicator
    pub popularity: u8, // 1 byte
    /// Reserved for alignment
    pub reserved: u8, // 1 byte
                      // Total: 16 bytes
}

/// Compact binary representation of an alternative move
#[derive(Debug, Clone)]
#[repr(C)]
pub struct CompactMove {
    /// Move encoded as 16-bit integer
    pub move_encoded: u16, // 2 bytes
    /// Evaluation of this move
    pub evaluation: i16, // 2 bytes
    /// Search depth for this move
    pub depth: u8, // 1 byte
    /// Reserved for alignment
    pub reserved: u8, // 1 byte
                      // Total: 6 bytes per move
}

/// Index entry for fast position lookup
#[derive(Debug, Clone)]
#[repr(C)]
pub struct PositionIndex {
    /// Position hash (matches CompactPosition.position_hash)
    pub hash: u64, // 8 bytes
    /// Byte offset in the binary file
    pub offset: u32, // 4 bytes
    /// Length of the position data in bytes
    pub length: u16, // 2 bytes
    /// Reserved for alignment
    pub reserved: u16, // 2 bytes
                       // Total: 16 bytes per index entry
}
