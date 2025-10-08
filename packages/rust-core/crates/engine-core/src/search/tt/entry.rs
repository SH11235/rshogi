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
    pub side_to_move: crate::Color,
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
            side_to_move: crate::Color::Black,
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

        // Clamp score and eval to 16-bit range (i16)
        let score = params.score.clamp(SCORE_MIN, SCORE_MAX);
        let eval = params.eval.clamp(EVAL_MIN, EVAL_MAX);
        let score_encoded = (score as u16) as u64; // two's complement bitcast
        let eval_encoded = (eval as u16) as u64; // two's complement bitcast

        // Pack all data into 64 bits with optimized layout:
        let data = ((move_data as u64) << MOVE_SHIFT)
            | (score_encoded << SCORE_SHIFT)
            | (eval_encoded << EVAL_SHIFT)
            | (((params.depth & DEPTH_MASK) as u64) << 9)
            | (((params.age & GEN_MASK) as u64) << GEN_SHIFT)
            | (((params.is_pv as u64) & 1) << PV_FLAG_SHIFT)
            | (((params.node_type as u64) & NODE_TYPE_MASK as u64) << NODE_TYPE_SHIFT);

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

    /// Get score from entry (16-bit signed value)
    #[inline]
    pub fn score(&self) -> i16 {
        let raw = ((self.data >> SCORE_SHIFT) & SCORE_MASK) as u16;
        raw as i16
    }

    /// Get static evaluation from entry (16-bit signed value)
    #[inline]
    pub fn eval(&self) -> i16 {
        let raw = ((self.data >> EVAL_SHIFT) & EVAL_MASK) as u16;
        raw as i16
    }

    /// Get search depth
    #[inline]
    pub fn depth(&self) -> u8 {
        ((self.data >> 9) & DEPTH_MASK as u64) as u8
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
        let age = ((self.data >> GEN_SHIFT) & GEN_MASK as u64) as u8;
        // Debug assertion to validate age is within expected range
        debug_assert!(age <= GEN_MASK, "Age value out of range: {age} (max: {GEN_MASK})");
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
        // Not implemented: reserved bit(s) are not allocated yet.
        // Kept as stub for future diagnostics. Always returns false.
        false
    }

    /// Check if Null Move Pruning was applied
    #[inline]
    pub fn has_null_move(&self) -> bool {
        // Not implemented: reserved bit(s) are not allocated yet.
        false
    }

    /// Check if TT move was tried
    #[inline]
    pub fn tt_move_tried(&self) -> bool {
        // Not implemented: reserved bit(s) are not allocated yet.
        false
    }

    /// Check if position has mate threat
    #[inline]
    pub fn has_mate_threat(&self) -> bool {
        // Not implemented: reserved bit(s) are not allocated yet.
        false
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

        // A/B2: わずかなペナルティで Non-PV の bound を優先度で抑制
        if !self.is_pv() && self.node_type() != NodeType::Exact {
            priority -= 2;
        }

        priority
    }
}
