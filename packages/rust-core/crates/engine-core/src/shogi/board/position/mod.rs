//! Position module
//!
//! This module manages the complete game position including board state,
//! hands, move history, and provides methods for move execution and validation.
//!
//! ## Module Structure
//! - `core` - Position struct and basic methods
//! - `moves` - Move execution and undo functionality
//! - `validation` - Move validation and position queries

// Private modules
mod core;
mod moves;
mod validation;

// Tests module
#[cfg(test)]
mod tests;

// Re-export Position and UndoInfo
pub use self::core::{Position, UndoInfo};
