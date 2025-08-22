//! Shogi board representation and game logic
//!
//! This module re-exports the refactored components from their respective submodules
//! to maintain backwards compatibility.

// Submodules
pub mod attacks;
mod bitboard;
mod board_repr;
pub mod see;
mod types;

// Performance test module
#[cfg(test)]
mod performance_tests;

// Re-export all public items from submodules
pub use self::bitboard::*;
pub use self::board_repr::*;
pub use self::types::*;
