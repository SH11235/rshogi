//! Transposition table for caching search results
//!
//! Lock-free implementation suitable for parallel search

use super::moves::Move;
use std::sync::atomic::{AtomicU64, Ordering};

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
        // Use high 48 bits of key (lower 16 bits used for indexing)
        let key = key & 0xFFFF_FFFF_FFFF_0000;

        // Pack move into 16 bits
        let move_data = match mv {
            Some(m) => m.to_u16(),
            None => 0,
        };

        // Pack all data into 64 bits:
        // [63-48]: move (16 bits)
        // [47-32]: score (16 bits)
        // [31-24]: depth (8 bits)
        // [23-22]: node type (2 bits)
        // [21-16]: age (6 bits)
        // [15-0]: static eval (16 bits)
        let data = ((move_data as u64) << 48)
            | ((score as u16 as u64) << 32)
            | ((depth as u64) << 24)
            | ((node_type as u64) << 22)
            | (((age & 0x3F) as u64) << 16)
            | (eval as u16 as u64);

        TTEntry { key, data }
    }

    /// Check if entry matches the given key
    #[inline]
    pub fn matches(&self, key: u64) -> bool {
        (self.key & 0xFFFF_FFFF_FFFF_0000) == (key & 0xFFFF_FFFF_FFFF_0000)
    }

    /// Extract move from entry
    pub fn get_move(&self) -> Option<Move> {
        let move_data = ((self.data >> 48) & 0xFFFF) as u16;

        if move_data == 0 {
            return None;
        }

        Some(Move::from_u16(move_data))
    }

    /// Get score from entry
    #[inline]
    pub fn score(&self) -> i16 {
        ((self.data >> 32) & 0xFFFF) as i16
    }

    /// Get static evaluation from entry
    #[inline]
    pub fn eval(&self) -> i16 {
        (self.data & 0xFFFF) as i16
    }

    /// Get search depth
    #[inline]
    pub fn depth(&self) -> u8 {
        ((self.data >> 24) & 0xFF) as u8
    }

    /// Get node type
    #[inline]
    pub fn node_type(&self) -> NodeType {
        match (self.data >> 22) & 0x3 {
            0 => NodeType::Exact,
            1 => NodeType::LowerBound,
            2 => NodeType::UpperBound,
            _ => unreachable!(),
        }
    }

    /// Get age
    #[inline]
    pub fn age(&self) -> u8 {
        ((self.data >> 16) & 0x3F) as u8
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
        let size = (size_mb * 1024 * 1024) / entry_size;

        // Round down to power of 2 for fast indexing
        let size = size.next_power_of_two() / 2;

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

        // Read both parts atomically
        let key = self.table[base_idx].load(Ordering::Relaxed);
        let data = self.table[base_idx + 1].load(Ordering::Relaxed);

        let entry = TTEntry { key, data };

        if entry.matches(hash) && entry.depth() > 0 {
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
        let idx = self.index(hash);
        let base_idx = idx * 2;

        // Create new entry
        let entry = TTEntry::new(hash, mv, score, eval, depth, node_type, self.age);

        // Read existing entry
        let old_key = self.table[base_idx].load(Ordering::Relaxed);
        let old_data = self.table[base_idx + 1].load(Ordering::Relaxed);
        let old_entry = TTEntry {
            key: old_key,
            data: old_data,
        };

        // Replacement strategy:
        // 1. Always replace if empty or different position
        // 2. For same position, replace if:
        //    - New entry is from current generation and old is not
        //    - Both from same generation but new has greater depth
        let should_replace = !old_entry.matches(hash)
            || old_entry.depth() == 0
            || (entry.age() == self.age && old_entry.age() != self.age)
            || (entry.age() == old_entry.age() && depth >= old_entry.depth());

        if should_replace {
            // Store atomically (order matters for concurrent access)
            self.table[base_idx].store(entry.key, Ordering::Relaxed);
            self.table[base_idx + 1].store(entry.data, Ordering::Relaxed);
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
        self.age = self.age.wrapping_add(1) & 0x3F; // 6-bit age
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
        assert_eq!(entry.age(), 42);

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
}
