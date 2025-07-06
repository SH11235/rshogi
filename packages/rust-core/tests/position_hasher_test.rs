#[cfg(test)]
mod position_hasher_tests {
    use shogi_core::opening_book::*;

    #[test]
    fn test_hash_initial_position() {
        let initial_sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let hash = PositionHasher::hash_position(initial_sfen).unwrap();

        // Hash should be deterministic
        let hash2 = PositionHasher::hash_position(initial_sfen).unwrap();
        assert_eq!(hash, hash2);

        // Hash should be non-zero
        assert_ne!(hash, 0);
    }

    #[test]
    fn test_hash_different_positions() {
        let pos1 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let pos2 = "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R w 2P2p 30";

        let hash1 = PositionHasher::hash_position(pos1).unwrap();
        let hash2 = PositionHasher::hash_position(pos2).unwrap();

        // Different positions should have different hashes
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_position_with_promotion() {
        let normal_pos = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let promoted_pos = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1+B5R1/LNSGKGSNL b - 1";

        let hash1 = PositionHasher::hash_position(normal_pos).unwrap();
        let hash2 = PositionHasher::hash_position(promoted_pos).unwrap();

        // Promoted vs non-promoted should have different hashes
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_empty_squares() {
        let pos1 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let pos2 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/4P4/PPPPP1PPP/1B5R1/LNSGKGSNL w - 2";

        let hash1 = PositionHasher::hash_position(pos1).unwrap();
        let hash2 = PositionHasher::hash_position(pos2).unwrap();

        // Different piece placements should have different hashes
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_complex_position() {
        let complex_pos = "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b 2P2p 35";
        let hash = PositionHasher::hash_position(complex_pos).unwrap();

        // Should handle complex positions with multiple promoted pieces
        assert_ne!(hash, 0);

        // Should be deterministic
        let hash2 = PositionHasher::hash_position(complex_pos).unwrap();
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_hash_position_uniqueness() {
        let positions = vec![
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PPPPP1PPP/1B5R1/LNSGKGSNL w - 2",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSN1 b - 1",
            "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b 2P2p 35",
        ];

        let mut hash_set = std::collections::HashSet::new();
        for pos in positions {
            let hash = PositionHasher::hash_position(pos).unwrap();
            hash_set.insert(hash);
        }

        // Should have good distribution (at least 4 unique hashes from 5 positions)
        assert!(hash_set.len() >= 4);
    }

    #[test]
    fn test_invalid_position_format() {
        let invalid_positions = vec![
            "invalid_sfen",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1", // Missing last rank
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL/extra", // Extra rank
            "lnsgkgsnl/1r5b1/ppppppppp/10/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL", // Invalid rank length
            "",
        ];

        for invalid_pos in invalid_positions {
            let result = PositionHasher::hash_position(invalid_pos);
            assert!(result.is_err(), "Should fail for invalid position: {invalid_pos}");
        }
    }

    #[test]
    fn test_hash_zobrist_properties() {
        let pos1 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let pos2 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PPPPP1PPP/1B5R1/LNSGKGSNL w - 2";

        let hash1 = PositionHasher::hash_position(pos1).unwrap();
        let hash2 = PositionHasher::hash_position(pos2).unwrap();

        // Zobrist hashing should provide good distribution
        let diff = hash1 ^ hash2;
        assert_ne!(diff, 0);

        // XOR should have roughly half bits set (good property for Zobrist)
        let bit_count = diff.count_ones();
        assert!(bit_count > 16 && bit_count < 48); // Roughly 25-75% of bits
    }

    #[test]
    fn test_hash_collision_detection() {
        let mut hasher = PositionHasher::new();

        let pos1 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let pos2 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PPPPP1PPP/1B5R1/LNSGKGSNL w - 2";

        let hash1 = hasher.hash_and_track(pos1).unwrap();
        let hash2 = hasher.hash_and_track(pos2).unwrap();

        // Different positions should have different hashes
        assert_ne!(hash1, hash2);

        // Should detect if same position is hashed again
        let hash1_again = hasher.hash_and_track(pos1).unwrap();
        assert_eq!(hash1, hash1_again);
    }

    #[test]
    fn test_hash_statistics() {
        let positions = vec![
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PPPPP1PPP/1B5R1/LNSGKGSNL w - 2",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSN1 b - 1",
            "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b 2P2p 35",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        ];

        let mut hasher = PositionHasher::new();
        for pos in positions {
            hasher.hash_and_track(pos).unwrap();
        }

        let stats = hasher.get_statistics();
        assert!(stats.total_positions >= 4); // At least 4 unique positions
        assert!(stats.collision_count == 0); // No collisions expected with good hash
        assert!(stats.unique_positions >= 4);
    }
}
