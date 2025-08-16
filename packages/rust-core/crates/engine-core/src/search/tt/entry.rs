//! Transposition table entry types and bit layout

use crate::shogi::Move;

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

pub(crate) const MOVE_SHIFT: u8 = 48;
pub(crate) const MOVE_BITS: u8 = 16;
pub(crate) const MOVE_MASK: u64 = (1 << MOVE_BITS) - 1;

// Optimized score field: 14 bits (was 16)
pub(crate) const SCORE_SHIFT: u8 = 34;
pub(crate) const SCORE_BITS: u8 = 14;
pub(crate) const SCORE_MASK: u64 = (1 << SCORE_BITS) - 1;
pub(crate) const SCORE_MAX: i16 = (1 << (SCORE_BITS - 1)) - 1; // 8191
pub(crate) const SCORE_MIN: i16 = -(1 << (SCORE_BITS - 1)); // -8192

// Extended flags (new)
#[allow(dead_code)]
pub(crate) const EXTENDED_FLAGS_SHIFT: u8 = 32;
#[allow(dead_code)]
pub(crate) const EXTENDED_FLAGS_BITS: u8 = 2;
pub(crate) const SINGULAR_EXTENSION_FLAG: u64 = 1 << 33;
pub(crate) const NULL_MOVE_FLAG: u64 = 1 << 32;

pub(crate) const DEPTH_SHIFT: u8 = 25;
pub(crate) const DEPTH_BITS: u8 = 7;
pub(crate) const DEPTH_MASK: u8 = (1 << DEPTH_BITS) - 1;
pub(crate) const NODE_TYPE_SHIFT: u8 = 23;
pub(crate) const NODE_TYPE_BITS: u8 = 2;
pub(crate) const NODE_TYPE_MASK: u8 = (1 << NODE_TYPE_BITS) - 1;
pub(crate) const AGE_SHIFT: u8 = 20;
pub(crate) const AGE_BITS: u8 = 3;
pub const AGE_MASK: u8 = (1 << AGE_BITS) - 1;
pub(crate) const PV_FLAG_SHIFT: u8 = 19;
pub(crate) const PV_FLAG_MASK: u64 = 1;

// Search flags (expanded)
#[allow(dead_code)]
pub(crate) const SEARCH_FLAGS_SHIFT: u8 = 16;
#[allow(dead_code)]
pub(crate) const SEARCH_FLAGS_BITS: u8 = 3;
pub(crate) const TT_MOVE_TRIED_FLAG: u64 = 1 << 18;
pub(crate) const MATE_THREAT_FLAG: u64 = 1 << 17;

// Optimized eval field: 14 bits (was 16)
pub(crate) const EVAL_SHIFT: u8 = 2;
pub(crate) const EVAL_BITS: u8 = 14;
pub(crate) const EVAL_MASK: u64 = (1 << EVAL_BITS) - 1;
pub(crate) const EVAL_MAX: i16 = (1 << (EVAL_BITS - 1)) - 1; // 8191
pub(crate) const EVAL_MIN: i16 = -(1 << (EVAL_BITS - 1)); // -8192

// Reserved for future
#[allow(dead_code)]
pub(crate) const RESERVED_BITS: u8 = 2;
#[allow(dead_code)]
pub(crate) const RESERVED_MASK: u64 = (1 << RESERVED_BITS) - 1;

// ABDADA flag for duplicate detection (Phase 3 optimization)
pub(crate) const ABDADA_CUT_FLAG: u64 = 1 << 0; // Use bit 0 from reserved bits

// Apery-style generation cycle constants
// This ensures proper wraparound behavior for age distance calculations
// The cycle is designed to be larger than the maximum possible age value (2^AGE_BITS)
// to prevent ambiguity in age distance calculations
// Use 256 as base for better alignment with age calculations
pub const GENERATION_CYCLE: u16 = 256; // Multiple of 256 for cleaner age distance calculations
#[allow(dead_code)]
pub(crate) const GENERATION_CYCLE_MASK: u16 = GENERATION_CYCLE - 1; // For efficient modulo operation

// Ensure GENERATION_CYCLE is larger than AGE_MASK to prevent ambiguity
#[cfg(debug_assertions)]
const _: () = assert!(GENERATION_CYCLE > AGE_MASK as u16);

/// Type of node in the search tree
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeType {
    /// Exact score (PV node)
    Exact = 0,
    /// Lower bound (fail-high/cut node)
    LowerBound = 1,
    /// Upper bound (fail-low/all node)
    UpperBound = 2,
}

/// Parameters for creating a TT entry
#[derive(Clone, Copy)]
pub struct TTEntryParams {
    pub key: u64,
    pub mv: Option<Move>,
    pub score: i16,
    pub eval: i16,
    pub depth: u8,
    pub node_type: NodeType,
    pub age: u8,
    pub is_pv: bool,
    // Extended flags (optional)
    pub singular_extension: bool,
    pub null_move: bool,
    pub tt_move_tried: bool,
    pub mate_threat: bool,
}

impl Default for TTEntryParams {
    fn default() -> Self {
        Self {
            key: 0,
            mv: None,
            score: 0,
            eval: 0,
            depth: 0,
            node_type: NodeType::Exact,
            age: 0,
            is_pv: false,
            singular_extension: false,
            null_move: false,
            tt_move_tried: false,
            mate_threat: false,
        }
    }
}

/// Transposition table entry (16 bytes)
#[derive(Clone, Copy, Default)]
#[repr(C, align(16))]
pub struct TTEntry {
    pub(crate) key: u64,
    pub(crate) data: u64,
}

impl TTEntry {
    /// Create new TT entry (backward compatibility)
    pub fn new(
        key: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
        age: u8,
    ) -> Self {
        let params = TTEntryParams {
            key,
            mv,
            score,
            eval,
            depth,
            node_type,
            age,
            is_pv: false,
            ..Default::default()
        };
        Self::from_params(params)
    }

    /// Create new TT entry from parameters
    pub fn from_params(params: TTEntryParams) -> Self {
        // Store full 64-bit key for accurate collision detection
        let key = params.key;

        // Pack move into 16 bits
        let move_data = match params.mv {
            Some(m) => m.to_u16(),
            None => 0,
        };

        // Clamp score and eval to 14-bit range
        let score = params.score.clamp(SCORE_MIN, SCORE_MAX);
        let eval = params.eval.clamp(EVAL_MIN, EVAL_MAX);

        // Encode score and eval as 14-bit values (with sign bit)
        let score_encoded = ((score as u16) & SCORE_MASK as u16) as u64;
        let eval_encoded = ((eval as u16) & EVAL_MASK as u16) as u64;

        // Pack all data into 64 bits with optimized layout:
        let mut data = ((move_data as u64) << MOVE_SHIFT)
            | (score_encoded << SCORE_SHIFT)
            | (((params.depth & DEPTH_MASK) as u64) << DEPTH_SHIFT)
            | ((params.node_type as u64) << NODE_TYPE_SHIFT)
            | (((params.age & AGE_MASK) as u64) << AGE_SHIFT)
            | ((params.is_pv as u64) << PV_FLAG_SHIFT)
            | (eval_encoded << EVAL_SHIFT);

        // Set extended flags
        if params.singular_extension {
            data |= SINGULAR_EXTENSION_FLAG;
        }
        if params.null_move {
            data |= NULL_MOVE_FLAG;
        }
        if params.tt_move_tried {
            data |= TT_MOVE_TRIED_FLAG;
        }
        if params.mate_threat {
            data |= MATE_THREAT_FLAG;
        }

        TTEntry { key, data }
    }

    /// Check if entry matches the given key
    #[inline]
    pub fn matches(&self, key: u64) -> bool {
        self.key == key
    }

    /// Check if entry is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.key == 0 && self.data == 0
    }

    /// Extract move from entry
    pub fn get_move(&self) -> Option<Move> {
        let move_data = ((self.data >> MOVE_SHIFT) & MOVE_MASK) as u16;
        if move_data == 0 {
            return None;
        }
        Some(Move::from_u16(move_data))
    }

    /// Get score from entry (14-bit signed value)
    #[inline]
    pub fn score(&self) -> i16 {
        let raw = ((self.data >> SCORE_SHIFT) & SCORE_MASK) as u16;
        // Efficient sign-extension from 14-bit to 16-bit
        // Left shift to align sign bit, then arithmetic right shift to extend
        ((raw as i16) << (16 - SCORE_BITS)) >> (16 - SCORE_BITS)
    }

    /// Get static evaluation from entry (14-bit signed value)
    #[inline]
    pub fn eval(&self) -> i16 {
        let raw = ((self.data >> EVAL_SHIFT) & EVAL_MASK) as u16;
        // Efficient sign-extension from 14-bit to 16-bit
        // Left shift to align sign bit, then arithmetic right shift to extend
        ((raw as i16) << (16 - EVAL_BITS)) >> (16 - EVAL_BITS)
    }

    /// Get search depth
    #[inline]
    pub fn depth(&self) -> u8 {
        ((self.data >> DEPTH_SHIFT) & DEPTH_MASK as u64) as u8
    }

    /// Get node type
    #[inline]
    pub fn node_type(&self) -> NodeType {
        let raw = (self.data >> NODE_TYPE_SHIFT) & NODE_TYPE_MASK as u64;
        match raw {
            0 => NodeType::Exact,
            1 => NodeType::LowerBound,
            2 => NodeType::UpperBound,
            _ => {
                // Debug assertion to catch corrupted data in development
                debug_assert!(false, "Corrupted node type in TT entry: raw value = {raw}");
                NodeType::Exact // Default to Exact for corrupted data
            }
        }
    }

    /// Get age
    #[inline]
    pub fn age(&self) -> u8 {
        let age = ((self.data >> AGE_SHIFT) & AGE_MASK as u64) as u8;
        // Debug assertion to validate age is within expected range
        debug_assert!(age <= AGE_MASK, "Age value out of range: {age} (max: {AGE_MASK})");
        age
    }

    /// Check if this is a PV node
    #[inline]
    pub fn is_pv(&self) -> bool {
        ((self.data >> PV_FLAG_SHIFT) & PV_FLAG_MASK) != 0
    }

    /// Check if Singular Extension was applied
    #[inline]
    pub fn has_singular_extension(&self) -> bool {
        (self.data & SINGULAR_EXTENSION_FLAG) != 0
    }

    /// Check if Null Move Pruning was applied
    #[inline]
    pub fn has_null_move(&self) -> bool {
        (self.data & NULL_MOVE_FLAG) != 0
    }

    /// Check if TT move was tried
    #[inline]
    pub fn tt_move_tried(&self) -> bool {
        (self.data & TT_MOVE_TRIED_FLAG) != 0
    }

    /// Check if position has mate threat
    #[inline]
    pub fn has_mate_threat(&self) -> bool {
        (self.data & MATE_THREAT_FLAG) != 0
    }

    /// Check if ABDADA exact cut flag is set
    #[inline]
    pub fn has_abdada_cut(&self) -> bool {
        (self.data & ABDADA_CUT_FLAG) != 0
    }

    /// Get the raw key value
    #[inline]
    pub fn key(&self) -> u64 {
        self.key
    }

    /// Convert from old representation (for migration)
    pub fn from_old_format(key: u64, data: u64) -> Self {
        TTEntry { key, data }
    }
}
