//! Transposition table entry types and bit layout

use super::constants::*;
use crate::search::NodeType;
use crate::shogi::Move;

// Use crate::search::NodeType as the canonical definition

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

    /// Calculate priority score for replacement decision
    pub(crate) fn priority_score(&self, current_age: u8) -> i32 {
        // Calculate cyclic age distance (Apery-style)
        let age_distance = ((GENERATION_CYCLE + current_age as u16 - self.age() as u16)
            & (AGE_MASK as u16)) as i32;

        // Base priority: depth minus age distance
        let mut priority = self.depth() as i32 - age_distance;

        // Bonus for PV nodes
        if self.is_pv() {
            priority += 32;
        }

        // Bonus for exact entries
        if self.node_type() == NodeType::Exact {
            priority += 16;
        }

        priority
    }
}
