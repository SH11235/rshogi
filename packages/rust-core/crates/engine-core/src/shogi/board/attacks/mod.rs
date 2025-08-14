//! Attack detection module
//!
//! This module provides functionality for detecting attacks, pins, and
//! determining which pieces can attack specific squares.
//!
//! ## Module Structure
//! - `non_sliding` - Attack detection for non-sliding pieces (pawn, knight, king, gold, silver)
//! - `sliding` - Attack detection for sliding pieces (rook, bishop, lance)
//! - `pins` - Pin detection functionality
//! - `core` - Main attack detection methods that combine all attack types

// Private modules
mod core;
mod non_sliding;
mod pins;
mod sliding;

// Tests module
#[cfg(test)]
mod tests;

// Re-export commonly used functions (if needed)
// Currently all functionality is exposed through impl Position
