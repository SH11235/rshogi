//! Position module
//!
//! This module manages the complete game position including board state,
//! hands, move history, and provides methods for move execution and validation.
//!
//! ## Module Structure
//! - `core` - Position struct and basic methods
//! - `moves` - Move execution and undo functionality
//! - `validation` - Move validation and position queries
//! - `legality` - Move legality checking
//! - `zobrist` - Zobrist hashing for position identification

// Private modules
mod core;
mod legality;
mod moves;
mod validation;
mod zobrist;

// Tests module
#[cfg(test)]
mod tests;

// Re-export Position and UndoInfo
pub use self::core::{Position, UndoInfo};

// Re-export zobrist types
pub use self::zobrist::{ZobristHashing, ZobristTable};
