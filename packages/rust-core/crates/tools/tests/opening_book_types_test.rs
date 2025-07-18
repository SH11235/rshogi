#[cfg(test)]
mod data_structures_tests {
    use shogi_core::opening_book::*;

    #[test]
    fn test_raw_sfen_entry_creation() {
        let entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 1,
            moves: vec![],
        };

        assert_eq!(entry.position.len(), 57); // Expected SFEN position length
        assert_eq!(entry.turn, 'b');
        assert_eq!(entry.hand, "-");
        assert_eq!(entry.move_count, 1);
        assert!(entry.moves.is_empty());
    }

    #[test]
    fn test_raw_move_creation() {
        let raw_move = RawMove {
            move_notation: "7g7f".to_string(),
            move_type: "none".to_string(),
            evaluation: 50,
            depth: 2,
            nodes: 1000,
        };

        assert_eq!(raw_move.move_notation, "7g7f");
        assert_eq!(raw_move.evaluation, 50);
        assert_eq!(raw_move.depth, 2);
        assert_eq!(raw_move.nodes, 1000);
    }

    #[test]
    fn test_compact_position_size() {
        // Verify CompactPosition is exactly 16 bytes
        assert_eq!(std::mem::size_of::<CompactPosition>(), 16);
    }

    #[test]
    fn test_compact_move_size() {
        // Verify CompactMove is exactly 6 bytes
        assert_eq!(std::mem::size_of::<CompactMove>(), 6);
    }

    #[test]
    fn test_position_index_size() {
        // Verify PositionIndex is exactly 16 bytes for efficient lookup
        assert_eq!(std::mem::size_of::<PositionIndex>(), 16);
    }

    #[test]
    fn test_compact_position_default_values() {
        let compact = CompactPosition {
            position_hash: 0x1234567890ABCDEF,
            best_move: 0x1234,
            evaluation: 100,
            depth: 3,
            move_count: 5,
            popularity: 10,
            reserved: 0,
        };

        assert_eq!(compact.position_hash, 0x1234567890ABCDEF);
        assert_eq!(compact.best_move, 0x1234);
        assert_eq!(compact.evaluation, 100);
        assert_eq!(compact.depth, 3);
        assert_eq!(compact.move_count, 5);
        assert_eq!(compact.popularity, 10);
        assert_eq!(compact.reserved, 0);
    }

    #[test]
    fn test_compact_move_creation() {
        let compact_move = CompactMove {
            move_encoded: 0x1234,
            evaluation: -50,
            depth: 2,
            reserved: 0,
        };

        assert_eq!(compact_move.move_encoded, 0x1234);
        assert_eq!(compact_move.evaluation, -50);
        assert_eq!(compact_move.depth, 2);
        assert_eq!(compact_move.reserved, 0);
    }

    #[test]
    fn test_position_index_creation() {
        let index = PositionIndex {
            hash: 0x1234567890ABCDEF,
            offset: 1024,
            length: 256,
            reserved: 0,
        };

        assert_eq!(index.hash, 0x1234567890ABCDEF);
        assert_eq!(index.offset, 1024);
        assert_eq!(index.length, 256);
        assert_eq!(index.reserved, 0);
    }
}
