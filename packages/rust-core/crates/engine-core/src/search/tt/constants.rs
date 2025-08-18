//! Constants and bit layout definitions for the transposition table
//!
//! This module contains all the constants related to bit packing and
//! configuration for the transposition table implementation.

// Bit layout constants for TTEntry data field
// Optimized layout (64 bits total) - Version 2.1:
// [63-48]: move (16 bits)
// [47-34]: score (14 bits) - Optimized from 16 bits, supports ±8191
// [33-32]: extended flags (2 bits):
//          - Bit 33: Singular Extension flag
//          - Bit 32: Null Move Pruning flag
// [31-25]: depth (7 bits) - Supports depth up to 127
// [24-23]: node type (2 bits) - Exact/LowerBound/UpperBound
// [22-20]: age (3 bits) - Generation counter (0-7)
// [19]: PV flag (1 bit) - Principal Variation node marker
// [18-16]: search flags (3 bits):
//          - Bit 18: TT Move tried flag
//          - Bit 17: Mate threat flag
//          - Bit 16: Reserved for future use
// [15-2]: static eval (14 bits) - Optimized from 16 bits, supports ±8191
// [1-0]: Reserved (2 bits) - Bit 0: ABDADA_CUT_FLAG, Bit 1: Reserved for future use

pub const MOVE_SHIFT: u8 = 48;
pub const MOVE_BITS: u8 = 16;
pub const MOVE_MASK: u64 = (1 << MOVE_BITS) - 1;

// Optimized score field: 14 bits (was 16)
pub const SCORE_SHIFT: u8 = 34;
pub const SCORE_BITS: u8 = 14;
pub const SCORE_MASK: u64 = (1 << SCORE_BITS) - 1;
pub const SCORE_MAX: i16 = (1 << (SCORE_BITS - 1)) - 1; // 8191
pub const SCORE_MIN: i16 = -(1 << (SCORE_BITS - 1)); // -8192

pub const SINGULAR_EXTENSION_FLAG: u64 = 1 << 33;
pub const NULL_MOVE_FLAG: u64 = 1 << 32;

pub const DEPTH_SHIFT: u8 = 25;
pub const DEPTH_BITS: u8 = 7;
pub const DEPTH_MASK: u8 = (1 << DEPTH_BITS) - 1;
pub const NODE_TYPE_SHIFT: u8 = 23;
pub const NODE_TYPE_BITS: u8 = 2;
pub const NODE_TYPE_MASK: u8 = (1 << NODE_TYPE_BITS) - 1;
pub const AGE_SHIFT: u8 = 20;
pub const AGE_BITS: u8 = 3;
pub const AGE_MASK: u8 = (1 << AGE_BITS) - 1;
pub const PV_FLAG_SHIFT: u8 = 19;
pub const PV_FLAG_MASK: u64 = 1;

pub const TT_MOVE_TRIED_FLAG: u64 = 1 << 18;
pub const MATE_THREAT_FLAG: u64 = 1 << 17;

// Optimized eval field: 14 bits (was 16)
pub const EVAL_SHIFT: u8 = 2;
pub const EVAL_BITS: u8 = 14;
pub const EVAL_MASK: u64 = (1 << EVAL_BITS) - 1;
pub const EVAL_MAX: i16 = (1 << (EVAL_BITS - 1)) - 1; // 8191
pub const EVAL_MIN: i16 = -(1 << (EVAL_BITS - 1)); // -8192

// ABDADA flag for duplicate detection
pub const ABDADA_CUT_FLAG: u64 = 1 << 0; // Use bit 0 from reserved bits

// Apery-style generation cycle constants
// This ensures proper wraparound behavior for age distance calculations
// The cycle is designed to be larger than the maximum possible age value (2^AGE_BITS)
// to prevent ambiguity in age distance calculations
// Use 256 as base for better alignment with age calculations
pub const GENERATION_CYCLE: u16 = 256; // Multiple of 256 for cleaner age distance calculations

pub const GENERATION_CYCLE_MASK: u16 = GENERATION_CYCLE - 1; // For efficient modulo operation

// Ensure GENERATION_CYCLE is larger than AGE_MASK to prevent ambiguity
// in age distance calculations
const _: () = assert!(GENERATION_CYCLE > AGE_MASK as u16);

// Key now uses full 64 bits for accurate collision detection
// const KEY_SHIFT: u8 = 32; // No longer needed after 64-bit comparison update

/// Number of entries per bucket (default for backward compatibility)
pub const BUCKET_SIZE: usize = 4;

/// Extract depth from packed data (7 bits)
#[inline(always)]
pub fn extract_depth(data: u64) -> u8 {
    ((data >> DEPTH_SHIFT) & DEPTH_MASK as u64) as u8
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
