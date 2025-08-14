//! Static Exchange Evaluation (SEE)
//!
//! This module implements SEE for evaluating capture sequences and determining
//! whether a capture is likely to be profitable.
//!
//! ## Module Structure
//! - `pin_info` - Pin detection for SEE calculations
//! - `helpers` - Helper functions for attacker detection and x-ray updates
//! - `core` - Main SEE algorithm implementation
//! - `tests` - Comprehensive test suite

// Private modules
mod core;
mod helpers;
mod pin_info;

// Tests module
#[cfg(test)]
mod tests;

// Re-export pin info struct for internal use (if needed in the future)
// pub(super) use pin_info::SeePinInfo;
