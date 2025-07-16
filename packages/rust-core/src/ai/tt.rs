//! Transposition table for caching search results
//!
//! Lock-free implementation suitable for parallel search

use super::moves::Move;
use super::sync_compat::{AtomicU64, Ordering};

// Bit layout constants for TTEntry data field
const MOVE_SHIFT: u8 = 48;
const MOVE_BITS: u8 = 16;
const MOVE_MASK: u64 = (1 << MOVE_BITS) - 1; // 0xFFFF (16 bits for move)
const SCORE_SHIFT: u8 = 32;
const SCORE_BITS: u8 = 16;
const SCORE_MASK: u64 = (1 << SCORE_BITS) - 1; // 0xFFFF (16 bits for score)
const DEPTH_SHIFT: u8 = 25;
const DEPTH_BITS: u8 = 7;
const DEPTH_MASK: u8 = (1 << DEPTH_BITS) - 1; // 0x7F (7 bits for depth)
const NODE_TYPE_SHIFT: u8 = 23;
const NODE_TYPE_BITS: u8 = 2;
const NODE_TYPE_MASK: u8 = (1 << NODE_TYPE_BITS) - 1; // 0x3 (2 bits for node type)
const ASPIRATION_FAIL_SHIFT: u8 = 22;
const AGE_SHIFT: u8 = 16;
const AGE_BITS: u8 = 6;
const AGE_MASK: u8 = (1 << AGE_BITS) - 1; // 0x3F (6 bits for age)
#[allow(dead_code)]
const EVAL_SHIFT: u8 = 0;
const EVAL_BITS: u8 = 16;
const EVAL_MASK: u64 = (1 << EVAL_BITS) - 1; // 0xFFFF (16 bits for static eval)

// Key mask (use high 48 bits, lower 16 bits used for indexing)
const KEY_MASK: u64 = 0xFFFF_FFFF_FFFF_0000;

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

/// Transposition table entry
///
/// Packed into 16 bytes for cache efficiency:
/// - 8 bytes: key (high 48 bits of zobrist hash)
/// - 2 bytes: move
/// - 2 bytes: score
/// - 1 byte: depth
/// - 1 byte: node type and age
/// - 2 bytes: static eval
#[derive(Clone, Copy)]
pub struct TTEntry {
    key: u64,
    data: u64,
}

impl TTEntry {
    /// Create new TT entry
    pub fn new(
        key: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
        age: u8,
    ) -> Self {
        Self::new_with_aspiration(key, mv, score, eval, depth, node_type, age, false)
    }

    /// Create new TT entry with aspiration fail flag
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_aspiration(
        key: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
        age: u8,
        aspiration_fail: bool,
    ) -> Self {
        // Use high 48 bits of key (lower 16 bits used for indexing)
        let key = key & KEY_MASK;

        // Pack move into 16 bits
        let move_data = match mv {
            Some(m) => m.to_u16(),
            None => 0,
        };

        // Pack all data into 64 bits:
        // [63-48]: move (16 bits)
        // [47-32]: score (16 bits)
        // [31-25]: depth (7 bits)
        // [24-23]: node type (2 bits)
        // [22]: aspiration_fail flag (1 bit)
        // [21-16]: age (6 bits)
        // [15-0]: static eval (16 bits)
        let data = ((move_data as u64) << MOVE_SHIFT)
            | ((score as u16 as u64) << SCORE_SHIFT)  // Store as unsigned 16-bit value (preserves 2's complement)
            | (((depth & DEPTH_MASK) as u64) << DEPTH_SHIFT)  // 7 bits for depth (max 127)
            | ((node_type as u64) << NODE_TYPE_SHIFT)
            | ((aspiration_fail as u64) << ASPIRATION_FAIL_SHIFT)
            | (((age & AGE_MASK) as u64) << AGE_SHIFT)  // 6 bits for age (max 63)
            | (eval as u16 as u64); // Store as unsigned 16-bit value (preserves 2's complement)

        TTEntry { key, data }
    }

    /// Check if entry matches the given key
    #[inline]
    pub fn matches(&self, key: u64) -> bool {
        (self.key & KEY_MASK) == (key & KEY_MASK)
    }

    /// Extract move from entry
    pub fn get_move(&self) -> Option<Move> {
        let move_data = ((self.data >> MOVE_SHIFT) & MOVE_MASK) as u16;

        if move_data == 0 {
            return None;
        }

        Some(Move::from_u16(move_data))
    }

    /// Get score from entry
    #[inline]
    pub fn score(&self) -> i16 {
        // Score is always valid since it's stored as u16 and cast to i16
        ((self.data >> SCORE_SHIFT) & SCORE_MASK) as i16
    }

    /// Get static evaluation from entry
    #[inline]
    pub fn eval(&self) -> i16 {
        // Eval is always valid since it's stored as u16 and cast to i16
        (self.data & EVAL_MASK) as i16
    }

    /// Get search depth
    #[inline]
    pub fn depth(&self) -> u8 {
        ((self.data >> DEPTH_SHIFT) & DEPTH_MASK as u64) as u8 // 7 bits for depth
    }

    /// Get node type
    #[inline]
    pub fn node_type(&self) -> NodeType {
        let raw = (self.data >> NODE_TYPE_SHIFT) & NODE_TYPE_MASK as u64;
        debug_assert!(raw <= 2, "Corrupted node_type bits: {raw}");
        match raw {
            0 => NodeType::Exact,
            1 => NodeType::LowerBound,
            2 => NodeType::UpperBound,
            _ => NodeType::Exact, // Default to Exact for corrupted data (no pruning)
        }
    }

    /// Get age
    #[inline]
    pub fn age(&self) -> u8 {
        ((self.data >> AGE_SHIFT) & AGE_MASK as u64) as u8 // 6 bits for age
    }

    /// Get aspiration fail flag
    #[inline]
    pub fn aspiration_fail(&self) -> bool {
        ((self.data >> ASPIRATION_FAIL_SHIFT) & 0x1) != 0
    }
}

/// Lock-free transposition table using atomic operations
pub struct TranspositionTable {
    /// Table entries (using AtomicU64 for lock-free access)
    /// Each entry is 2 AtomicU64s (16 bytes total)
    table: Vec<AtomicU64>,
    /// Size of the table in entries
    size: usize,
    /// Current age/generation
    age: u8,
}

impl TranspositionTable {
    /// Create new transposition table with given size in MB
    pub fn new(size_mb: usize) -> Self {
        // Each entry is 16 bytes (2 * u64)
        let entry_size = 16;
        let size = if size_mb == 0 {
            // Minimum size: 64KB = 4096 entries
            4096
        } else {
            (size_mb * 1024 * 1024) / entry_size
        };

        // Round to power of 2 for fast indexing
        let size = size.next_power_of_two();

        // Allocate table (2 AtomicU64 per entry)
        let mut table = Vec::with_capacity(size * 2);
        for _ in 0..size * 2 {
            table.push(AtomicU64::new(0));
        }

        TranspositionTable {
            table,
            size,
            age: 0,
        }
    }

    /// Get table index from zobrist hash
    #[inline]
    fn index(&self, hash: u64) -> usize {
        (hash as usize) & (self.size - 1)
    }

    /// Probe the transposition table
    pub fn probe(&self, hash: u64) -> Option<TTEntry> {
        let idx = self.index(hash);
        let base_idx = idx * 2;

        // Read key with Acquire ordering to ensure data consistency
        let key = self.table[base_idx].load(Ordering::Acquire);

        // Check key match before reading data
        if (key & KEY_MASK) != (hash & KEY_MASK) {
            return None;
        }

        // Read data (Relaxed is sufficient after key match with Acquire)
        let data = self.table[base_idx + 1].load(Ordering::Relaxed);

        let entry = TTEntry { key, data };

        if entry.depth() > 0 {
            Some(entry)
        } else {
            None
        }
    }

    /// Store entry in transposition table
    pub fn store(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
    ) {
        self.store_with_aspiration(hash, mv, score, eval, depth, node_type, false)
    }

    /// Determine if new entry should replace old entry
    /// Returns true if new entry is stronger and should replace the old one
    fn is_stronger(&self, new: &TTEntry, old: &TTEntry, hash: u64) -> bool {
        // 1. Empty slot or different position (most common case)
        if !old.matches(hash) || old.depth() == 0 {
            return true;
        }

        // 2. Current generation always replaces old generation
        let new_is_current = new.age() == self.age;
        let old_is_current = old.age() == self.age;
        if new_is_current && !old_is_current {
            return true;
        }

        // 3. Node type priority: Exact > Lower/Upper
        let new_type = new.node_type();
        let old_type = old.node_type();
        if new_type == NodeType::Exact && old_type != NodeType::Exact {
            return true;
        }
        if new_type != NodeType::Exact && old_type == NodeType::Exact {
            return false;
        }

        // 4. Aspiration: success > fail
        let new_asp_fail = new.aspiration_fail();
        let old_asp_fail = old.aspiration_fail();
        if !new_asp_fail && old_asp_fail {
            return true;
        }
        if new_asp_fail && !old_asp_fail {
            return false;
        }

        // 5. Depth comparison
        let new_depth = new.depth();
        let old_depth = old.depth();
        if new_depth > old_depth {
            return true;
        }
        if new_depth < old_depth {
            return false;
        }

        // 6. Same depth, same conditions → don't replace
        // (Same generation case, or new is old generation)
        false
    }

    /// Store entry with aspiration fail flag
    #[allow(clippy::too_many_arguments)]
    pub fn store_with_aspiration(
        &self,
        hash: u64,
        mv: Option<Move>,
        score: i16,
        eval: i16,
        depth: u8,
        node_type: NodeType,
        aspiration_fail: bool,
    ) {
        let idx = self.index(hash);
        let base_idx = idx * 2;

        // Create new entry
        let entry = TTEntry::new_with_aspiration(
            hash,
            mv,
            score,
            eval,
            depth,
            node_type,
            self.age,
            aspiration_fail,
        );

        // Read existing entry
        let old_key = self.table[base_idx].load(Ordering::Relaxed);
        let old_data = self.table[base_idx + 1].load(Ordering::Relaxed);
        let old_entry = TTEntry {
            key: old_key,
            data: old_data,
        };

        // Use is_stronger to determine if replacement should occur
        if self.is_stronger(&entry, &old_entry, hash) {
            // Store data first, then key with Release ordering to ensure consistency
            // This prevents torn reads where key matches but data is stale
            self.table[base_idx + 1].store(entry.data, Ordering::Release);
            self.table[base_idx].store(entry.key, Ordering::Release);
        }
    }

    /// Clear the transposition table
    pub fn clear(&mut self) {
        for entry in &self.table {
            entry.store(0, Ordering::Relaxed);
        }
        self.age = 0;
    }

    /// Advance to next generation (for age-based replacement)
    pub fn new_search(&mut self) {
        self.age = self.age.wrapping_add(1) & AGE_MASK; // 6-bit age (max 63)
    }

    /// Get fill rate (percentage of non-empty entries)
    pub fn hashfull(&self) -> u16 {
        // Sample first 1000 entries
        let sample_size = 1000.min(self.size);
        let mut filled = 0;

        for i in 0..sample_size {
            let key = self.table[i * 2].load(Ordering::Relaxed);
            if key != 0 {
                filled += 1;
            }
        }

        ((filled * 1000) / sample_size) as u16
    }

    /// Get table size in entries
    pub fn size(&self) -> usize {
        self.size
    }

    /// Prefetch entry for the given hash (CPU optimization)
    #[inline]
    pub fn prefetch(&self, hash: u64) {
        let idx = self.index(hash);
        let base_idx = idx * 2;

        // Prefetch both cache lines
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::_mm_prefetch;
            let ptr = &self.table[base_idx] as *const AtomicU64 as *const i8;
            _mm_prefetch(ptr, 3); // _MM_HINT_T0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::board::{PieceType, Square};

    #[test]
    fn test_tt_entry_packing() {
        let mv = Some(Move::normal(Square::new(2, 7), Square::new(2, 6), false));

        let entry = TTEntry::new(0x1234567890ABCDEF, mv, -1234, 567, 10, NodeType::Exact, 42);

        // Check key is stored correctly (high 48 bits)
        assert!(entry.matches(0x1234567890ABCDEF));
        assert!(entry.matches(0x1234567890AB0000)); // Lower 16 bits ignored

        // Check data extraction
        assert_eq!(entry.score(), -1234);
        assert_eq!(entry.eval(), 567);
        assert_eq!(entry.depth(), 10);
        assert_eq!(entry.node_type(), NodeType::Exact);
        assert_eq!(entry.age(), 42 & 0x3F); // 6 bits only

        // Check move extraction
        let extracted_move = entry.get_move().unwrap();
        assert_eq!(extracted_move.from(), Some(Square::new(2, 7)));
        assert_eq!(extracted_move.to(), Square::new(2, 6));
        assert!(!extracted_move.is_promote());
    }

    #[test]
    fn test_tt_drop_move() {
        let mv = Some(Move::drop(PieceType::Pawn, Square::new(5, 5)));

        let entry = TTEntry::new(0, mv, 0, 0, 0, NodeType::Exact, 0);

        let extracted = entry.get_move().unwrap();
        assert!(extracted.is_drop());
        assert_eq!(extracted.drop_piece_type(), PieceType::Pawn);
        assert_eq!(extracted.to(), Square::new(5, 5));
    }

    #[test]
    fn test_transposition_table() {
        let tt = TranspositionTable::new(1); // 1MB table

        // Store and retrieve entry
        let hash = 0x1234567890ABCDEF;
        let mv = Some(Move::normal(Square::new(7, 7), Square::new(7, 6), false));

        tt.store(hash, mv, 1500, 1000, 8, NodeType::LowerBound);

        let entry = tt.probe(hash).expect("Entry should be found");
        assert_eq!(entry.score(), 1500);
        assert_eq!(entry.eval(), 1000);
        assert_eq!(entry.depth(), 8);
        assert_eq!(entry.node_type(), NodeType::LowerBound);
    }

    #[test]
    fn test_tt_replacement() {
        let tt = TranspositionTable::new(1);
        let hash = 0x1234567890ABCDEF;

        // Store initial entry
        tt.store(hash, None, 100, 50, 5, NodeType::Exact);

        // Store deeper entry (should replace)
        tt.store(hash, None, 200, 150, 10, NodeType::Exact);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.depth(), 10);
        assert_eq!(entry.score(), 200);

        // Store shallower entry (should not replace)
        tt.store(hash, None, 300, 250, 3, NodeType::Exact);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.depth(), 10); // Still the deeper entry
        assert_eq!(entry.score(), 200);
    }

    #[test]
    fn test_age_wrap_around() {
        let mut tt = TranspositionTable::new(1);

        // Test that age wraps around at 64 (6 bits: 0-63)
        tt.age = 63;
        tt.new_search();
        assert_eq!(tt.age, 0);

        // Test normal increment
        tt.age = 30;
        tt.new_search();
        assert_eq!(tt.age, 31);

        // Test age extraction from entry
        let hash = 0x1234567890ABCDEF;
        tt.age = 63;
        tt.store(hash, None, 100, 50, 5, NodeType::Exact);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.age(), 63);

        // Test that age is properly masked in TTEntry::new_with_aspiration
        let entry_with_overflow = TTEntry::new_with_aspiration(
            hash,
            None,
            100,
            50,
            5,
            NodeType::Exact,
            255, // This should be masked to 63 (255 & 0x3F = 63)
            false,
        );
        assert_eq!(entry_with_overflow.age(), 63);
    }

    #[test]
    fn test_depth_limit() {
        // Test that depth is limited to 7 bits (max 127)
        let entry = TTEntry::new(0x1234567890ABCDEF, None, 0, 0, 255, NodeType::Exact, 0);
        assert_eq!(entry.depth(), 127); // Should be clamped to 127

        let entry2 = TTEntry::new(0x1234567890ABCDEF, None, 0, 0, 127, NodeType::Exact, 0);
        assert_eq!(entry2.depth(), 127);

        let entry3 = TTEntry::new(0x1234567890ABCDEF, None, 0, 0, 100, NodeType::Exact, 0);
        assert_eq!(entry3.depth(), 100);
    }

    #[test]
    fn test_replacement_policy_node_type_priority() {
        let tt = TranspositionTable::new(1);
        let hash = 0x1234567890ABCDEF;

        // Store LowerBound entry
        tt.store_with_aspiration(hash, None, 100, 50, 10, NodeType::LowerBound, false);

        // Store Exact entry with same depth - should replace
        tt.store_with_aspiration(hash, None, 200, 150, 10, NodeType::Exact, false);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.score(), 200); // Exact entry
        assert_eq!(entry.node_type(), NodeType::Exact);

        // Store another LowerBound with deeper search - should NOT replace Exact
        tt.store_with_aspiration(hash, None, 300, 250, 15, NodeType::LowerBound, false);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.score(), 200); // Still Exact entry
        assert_eq!(entry.node_type(), NodeType::Exact);
    }

    #[test]
    fn test_replacement_policy_aspiration_priority() {
        let tt = TranspositionTable::new(1);
        let hash = 0x1234567890ABCDEF;

        // Store aspiration fail entry
        tt.store_with_aspiration(hash, None, 100, 50, 10, NodeType::Exact, true);

        // Store aspiration success entry with same depth - should replace
        tt.store_with_aspiration(hash, None, 200, 150, 10, NodeType::Exact, false);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.score(), 200); // Success entry
        assert!(!entry.aspiration_fail());

        // Store another fail entry with deeper search - should NOT replace success
        tt.store_with_aspiration(hash, None, 300, 250, 15, NodeType::Exact, true);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.score(), 200); // Still success entry
        assert!(!entry.aspiration_fail());
    }

    #[test]
    fn test_replacement_policy_generation_tiebreak() {
        let mut tt = TranspositionTable::new(1);
        let hash = 0x1234567890ABCDEF;

        // Store entry in generation 0
        tt.store_with_aspiration(hash, None, 100, 50, 10, NodeType::Exact, false);

        // Advance generation
        tt.new_search();

        // Store entry with same depth, same node type, same aspiration - should replace due to newer generation
        tt.store_with_aspiration(hash, None, 200, 150, 10, NodeType::Exact, false);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.score(), 200); // New generation entry
        assert_eq!(entry.age(), 1);
    }

    #[test]
    fn test_replacement_policy_same_generation_same_depth() {
        let tt = TranspositionTable::new(1);
        let hash = 0x1234567890ABCDEF;

        // Store initial entry
        tt.store_with_aspiration(hash, None, 100, 50, 10, NodeType::Exact, false);

        // Store another entry with same depth, same generation - should NOT replace
        tt.store_with_aspiration(hash, None, 200, 150, 10, NodeType::Exact, false);

        let entry = tt.probe(hash).unwrap();
        assert_eq!(entry.score(), 100); // Old entry retained
    }

    #[test]
    fn test_aspiration_entry() {
        let tt = TranspositionTable::new(1);
        let hash = 0x1234567890ABCDEF;

        // Store entry with aspiration fail flag
        tt.store_with_aspiration(hash, None, 100, 50, 10, NodeType::Exact, true);

        let entry = tt.probe(hash).unwrap();
        assert!(entry.aspiration_fail());
        assert_eq!(entry.score(), 100);

        // Store entry without aspiration fail flag
        tt.store_with_aspiration(hash, None, 200, 150, 10, NodeType::Exact, false);

        let entry = tt.probe(hash).unwrap();
        assert!(!entry.aspiration_fail());
        assert_eq!(entry.score(), 200);
    }

    #[test]
    fn test_negative_scores() {
        let tt = TranspositionTable::new(1);

        // Test various negative scores with different hash values to avoid conflicts
        let test_cases: Vec<(u64, i16)> = vec![
            (0x1111111111111111, -32768),
            (0x2222222222222222, -30000),
            (0x3333333333333333, -1000),
            (0x4444444444444444, -1),
            (0x5555555555555555, 0),
            (0x6666666666666666, 1),
            (0x7777777777777777, 1000),
            (0x8888888888888888, 30000),
            (0x9999999999999999, 32767),
        ];

        for (hash, score) in test_cases {
            tt.store(hash, None, score, score / 2, 10, NodeType::Exact);

            let entry = tt.probe(hash).unwrap();
            assert_eq!(entry.score(), score, "Failed to store/retrieve score: {score}");
            assert_eq!(entry.eval(), score / 2, "Failed to store/retrieve eval: {}", score / 2);
        }
    }

    #[test]
    fn test_edge_cases() {
        // Test maximum depth (127)
        {
            let tt = TranspositionTable::new(1);
            let hash = 0xCCCCCCCCCCCCCCCC;
            tt.store(hash, None, 100, 50, 127, NodeType::Exact);
            let entry = tt.probe(hash).unwrap();
            assert_eq!(entry.depth(), 127);
        }

        // Test depth overflow (masked to 7 bits)
        {
            let tt = TranspositionTable::new(1);
            let hash = 0xDDDDDDDDDDDDDDDD;
            tt.store(hash, None, 100, 50, 200, NodeType::Exact);
            let entry = tt.probe(hash).unwrap();
            assert_eq!(entry.depth(), 200 & DEPTH_MASK); // 200 & DEPTH_MASK = 72
        }

        // Test minimum values
        {
            let tt = TranspositionTable::new(1);
            let hash = 0xEEEEEEEEEEEEEEEE;
            tt.store(hash, None, -32768, -32768, 1, NodeType::Exact); // depth must be > 0
            let entry = tt.probe(hash).unwrap();
            assert_eq!(entry.score(), -32768);
            assert_eq!(entry.eval(), -32768);
            assert_eq!(entry.depth(), 1);
        }

        // Test maximum values
        {
            let tt = TranspositionTable::new(1);
            let hash = 0xFFFFFFFFFFFFFFFF;
            tt.store(hash, None, 32767, 32767, 127, NodeType::Exact);
            let entry = tt.probe(hash).unwrap();
            assert_eq!(entry.score(), 32767);
            assert_eq!(entry.eval(), 32767);
            assert_eq!(entry.depth(), 127);
        }
    }

    #[test]
    #[cfg(not(debug_assertions))] // Skip in debug mode due to debug_assert!
    fn test_corrupted_node_type() {
        // Create an entry with all bits set in node type field
        let entry = TTEntry {
            key: 0x1234567890AB0000,
            data: 0xFFFFFFFFFFFFFFFF, // All bits set
        };

        // Should return Exact as default for corrupted data (safest for alpha-beta)
        assert_eq!(entry.node_type(), NodeType::Exact);
    }

    #[test]
    fn test_table_size_calculation() {
        // Test minimum size (0 MB)
        let tt = TranspositionTable::new(0);
        assert_eq!(tt.size(), 4096); // Should be exactly 4096 entries (64KB)

        // Test 1 MB
        let tt = TranspositionTable::new(1);
        assert_eq!(tt.size(), 65536); // 1MB / 16 bytes = 65536 entries

        // Test non-power-of-two size
        let tt = TranspositionTable::new(3);
        assert_eq!(tt.size(), 262144); // Next power of 2 from 196608
    }

    #[test]
    fn test_tt_entry_packing_with_aspiration() {
        let mv = Some(Move::normal(Square::new(2, 7), Square::new(2, 6), false));

        // Comprehensive boundary value test cases
        let test_cases = vec![
            // (move, score, eval, depth, node_type, age, aspiration_fail)
            (mv, -32768i16, -32768i16, 0u8, NodeType::Exact, 0u8, false),
            (mv, 32767, 32767, 127, NodeType::LowerBound, 63, true),
            (None, 0, 0, 127, NodeType::UpperBound, 15, false),
            (
                Some(Move::drop(PieceType::Pawn, Square::new(5, 5))),
                -1000,
                500,
                50,
                NodeType::Exact,
                31,
                true,
            ),
            (
                Some(Move::normal(Square::new(7, 7), Square::new(7, 6), true)),
                1500,
                -200,
                25,
                NodeType::LowerBound,
                0,
                false,
            ),
        ];

        for (mv, score, eval, depth, node_type, age, asp_fail) in test_cases {
            let entry = TTEntry::new_with_aspiration(
                0x1234567890ABCDEF,
                mv,
                score,
                eval,
                depth,
                node_type,
                age,
                asp_fail,
            );

            assert_eq!(entry.score(), score, "Score mismatch for test case");
            assert_eq!(entry.eval(), eval, "Eval mismatch for test case");
            assert_eq!(entry.depth(), depth & DEPTH_MASK, "Depth mismatch for test case");
            assert_eq!(entry.node_type(), node_type, "NodeType mismatch for test case");
            assert_eq!(entry.age(), age & AGE_MASK, "Age mismatch for test case");
            assert_eq!(entry.aspiration_fail(), asp_fail, "Aspiration fail mismatch for test case");

            if let Some(m) = mv {
                let retrieved = entry.get_move().expect("Move should be present");
                assert_eq!(retrieved.from(), m.from(), "Move from square mismatch");
                assert_eq!(retrieved.to(), m.to(), "Move to square mismatch");
                assert_eq!(retrieved.is_promote(), m.is_promote(), "Move promotion mismatch");
                assert_eq!(retrieved.is_drop(), m.is_drop(), "Move drop type mismatch");
                if m.is_drop() {
                    assert_eq!(
                        retrieved.drop_piece_type(),
                        m.drop_piece_type(),
                        "Drop piece type mismatch"
                    );
                }
            } else {
                assert_eq!(entry.get_move(), None, "Move should be None");
            }
        }
    }

    #[test]
    fn test_bit_field_isolation() {
        // Test that each field is properly isolated and doesn't affect others
        // Use maximum values for each field to ensure no overflow into adjacent fields

        // Test with all fields at maximum values
        let entry_max = TTEntry::new_with_aspiration(
            0xFFFF_FFFF_FFFF_FFFF,
            Some(Move::from_u16(0xFFFF)), // Max move value
            -1,                           // 0xFFFF when stored as u16
            -1,                           // 0xFFFF when stored as u16
            127,                          // Max depth (7 bits)
            NodeType::UpperBound,         // Value 2
            63,                           // Max age (6 bits)
            true,                         // Aspiration fail set
        );

        // Verify each field independently
        assert!(entry_max.matches(0xFFFF_FFFF_FFFF_0000));
        assert_eq!(entry_max.score(), -1);
        assert_eq!(entry_max.eval(), -1);
        assert_eq!(entry_max.depth(), 127);
        assert_eq!(entry_max.node_type(), NodeType::UpperBound);
        assert_eq!(entry_max.age(), 63);
        assert!(entry_max.aspiration_fail());

        // Test with alternating bit patterns to check isolation
        let entry_pattern = TTEntry::new_with_aspiration(
            0xAAAA_AAAA_AAAA_AAAA,
            Some(Move::from_u16(0x5555)), // Alternating bits
            0x5555_u16 as i16,            // Alternating bits
            0x5555_u16 as i16,            // Alternating bits
            0x55,                         // Alternating bits (85 decimal)
            NodeType::LowerBound,
            0x2A, // Alternating bits (42 decimal)
            false,
        );

        assert_eq!(entry_pattern.score(), 0x5555_u16 as i16);
        assert_eq!(entry_pattern.eval(), 0x5555_u16 as i16);
        assert_eq!(entry_pattern.depth(), 0x55);
        assert_eq!(entry_pattern.node_type(), NodeType::LowerBound);
        assert_eq!(entry_pattern.age(), 0x2A);
        assert!(!entry_pattern.aspiration_fail());

        // Test that overflow values are properly masked
        let entry_overflow = TTEntry::new_with_aspiration(
            0x1234_5678_9ABC_DEF0,
            None,
            32767,
            -32768,
            255, // Should be masked to 127
            NodeType::Exact,
            255, // Should be masked to 63
            true,
        );

        assert_eq!(entry_overflow.depth(), 127); // 255 & 0x7F = 127
        assert_eq!(entry_overflow.age(), 63); // 255 & 0x3F = 63
        assert_eq!(entry_overflow.score(), 32767);
        assert_eq!(entry_overflow.eval(), -32768);
        assert!(entry_overflow.aspiration_fail());
    }
}

// Include parallel safety tests
#[cfg(test)]
#[path = "tt_parallel_test.rs"]
mod tt_parallel_test;
