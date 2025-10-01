//! Constants and bit layout definitions for the transposition table
//!
//! This module contains all the constants related to bit packing and
//! configuration for the transposition table implementation.

// Bit layout constants for TTEntry data field (64 bits total)
// [63:48] move16
// [47:32] score16 (i16, fail-soft, mate is distance-normalized)
// [31:16] eval16  (i16, static evaluate(pos))
// [15:11] gen5    (search generation, 0..31, wrap)
// [10]    pv1     (PV flag)
// [9:8]   bound2  (0=Exact,1=Lower,2=Upper)
// [0]     abdada1 (ABDADA exact-cut flag)

pub const MOVE_SHIFT: u8 = 48;
pub const MOVE_BITS: u8 = 16;
pub const MOVE_MASK: u64 = (1 << MOVE_BITS) - 1;

pub const SCORE_SHIFT: u8 = 32;
pub const SCORE_BITS: u8 = 16;
pub const SCORE_MASK: u64 = (1 << SCORE_BITS) - 1;
pub const SCORE_MAX: i16 = i16::MAX; // 32767
pub const SCORE_MIN: i16 = i16::MIN; // -32768

pub const EVAL_SHIFT: u8 = 16;
pub const EVAL_BITS: u8 = 16;
pub const EVAL_MASK: u64 = (1 << EVAL_BITS) - 1;
pub const EVAL_MAX: i16 = i16::MAX;
pub const EVAL_MIN: i16 = i16::MIN;

// gen5 occupies [10:4]
pub const GEN_SHIFT: u8 = 4;
pub const GEN_BITS: u8 = 5;
pub const GEN_MASK: u8 = (1 << GEN_BITS) - 1; // 0..31

// pv1 at bit 3
pub const PV_FLAG_SHIFT: u8 = 3;
pub const PV_FLAG_MASK: u64 = 1;

// bound2 at [2:1]
pub const NODE_TYPE_SHIFT: u8 = 1;
pub const NODE_TYPE_BITS: u8 = 2;
pub const NODE_TYPE_MASK: u8 = (1 << NODE_TYPE_BITS) - 1;

pub const DEPTH_SHIFT: u8 = 9; // Derived below for helper
pub const DEPTH_BITS: u8 = 7; // depth is stored in [15:9], see extract_depth()
pub const DEPTH_MASK: u8 = (1 << DEPTH_BITS) - 1;

// ABDADA flag for duplicate detection (bit 0)
pub const ABDADA_CUT_FLAG: u64 = 1 << 0;

// Apery-style generation cycle constants
// This ensures proper wraparound behavior for age distance calculations
// The cycle is designed to be larger than the maximum possible age value (2^AGE_BITS)
// to prevent ambiguity in age distance calculations
// Use 256 as base for better alignment with age calculations
pub const GENERATION_CYCLE: u16 = 256; // Keep 256; gen field is 5 bits inside entry

pub const GENERATION_CYCLE_MASK: u16 = GENERATION_CYCLE - 1; // For efficient modulo operation

// Ensure GENERATION_CYCLE is larger than AGE_MASK to prevent ambiguity
// in age distance calculations
// Age mask for TranspositionTable age counter (now 5 bits)
pub const AGE_BITS: u8 = 5;
pub const AGE_MASK: u8 = (1 << AGE_BITS) - 1; // 0..31
const _: () = assert!(GENERATION_CYCLE > AGE_MASK as u16);

// Key now uses full 64 bits for accurate collision detection
// const KEY_SHIFT: u8 = 32; // No longer needed after 64-bit comparison update

/// Number of entries per bucket (default for backward compatibility)
pub const BUCKET_SIZE: usize = 4;

/// Extract depth from packed data (7 bits)
#[inline(always)]
pub fn extract_depth(data: u64) -> u8 {
    // depth is stored in [15:9]
    ((data >> 9) & (DEPTH_MASK as u64)) as u8
}

/// Get depth threshold based on hashfull - optimized branch version
#[inline(always)]
pub fn get_depth_threshold(hf: u16) -> u8 {
    // Early return for most common case
    if hf < 600 {
        return 0;
    }

    match hf {
        600..=800 => 2,
        801..=900 => 3,
        901..=950 => 4,
        _ => 5,
    }
}
