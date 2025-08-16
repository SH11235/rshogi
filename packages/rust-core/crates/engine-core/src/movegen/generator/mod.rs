//! Move generator module facade

// Internal modules
mod attacks;
mod checks;
mod core;
mod drops;
mod pieces;
mod sliding;
mod utils;

// Re-export the main types
pub use core::MoveGenImpl;

// Module is organized as follows:
// - core.rs: Main MoveGenImpl struct and generate_all() method
// - pieces.rs: Piece-specific move generation (king, gold, silver, knight, pawn)
// - sliding.rs: Sliding piece moves (rook, bishop, lance)
// - drops.rs: Drop move generation and validation
// - checks.rs: Check and pin calculation
// - attacks.rs: Attack detection functions
// - utils.rs: Helper functions and utilities
