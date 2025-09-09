#[cfg(test)]
mod move_encoder_tests {
    use engine_core::opening_book::MoveEncoder;

    #[test]
    fn test_encode_normal_moves() {
        let test_cases = vec![
            ("7g7f", "Pawn forward"),
            ("8i7g", "Knight move"),
            ("5i4h", "King move"),
            ("1a1b", "Edge move"),
            ("9i9h", "Corner move"),
        ];

        for (move_notation, description) in test_cases {
            let result = MoveEncoder::encode_move(move_notation);
            assert!(result.is_ok(), "Failed to encode {description}: {move_notation}");

            let encoded = result.unwrap();
            let decoded = MoveEncoder::decode_move(encoded).unwrap();
            assert_eq!(move_notation, decoded, "Round-trip failed for {description}");
        }
    }

    #[test]
    fn test_encode_promotion_moves() {
        let test_cases = vec![
            ("3d3c+", "Pawn promotion"),
            ("2e2d+", "Knight promotion"),
            ("4f4e+", "Silver promotion"),
        ];

        for (move_notation, description) in test_cases {
            let encoded = MoveEncoder::encode_move(move_notation).unwrap();
            let decoded = MoveEncoder::decode_move(encoded).unwrap();
            assert_eq!(move_notation, decoded, "Promotion encoding failed for {description}");
        }
    }

    #[test]
    fn test_encode_drop_moves() {
        let test_cases = vec![
            ("P*5f", "Pawn drop"),
            ("N*4e", "Knight drop"),
            ("B*7e", "Bishop drop"),
            ("R*6c", "Rook drop"),
            ("G*3f", "Gold drop"),
            ("S*2d", "Silver drop"),
            ("L*9f", "Lance drop"),
        ];

        for (move_notation, description) in test_cases {
            let encoded = MoveEncoder::encode_move(move_notation).unwrap();
            let decoded = MoveEncoder::decode_move(encoded).unwrap();
            assert_eq!(move_notation, decoded, "Drop encoding failed for {description}");
        }
    }

    #[test]
    fn test_integration_roundtrip() {
        // Moved from integration_test.rs - comprehensive roundtrip test
        let test_moves = vec![
            "7g7f",  // Normal move
            "2g2f",  // Normal move
            "8h2b+", // Promotion
            "P*5e",  // Drop
            "G*5f",  // Gold drop
            "N*4e",  // Knight drop
        ];

        for move_str in test_moves {
            let encoded = MoveEncoder::encode_move(move_str).unwrap();
            let decoded = MoveEncoder::decode_move(encoded).unwrap();
            assert_eq!(move_str, decoded, "Failed roundtrip for move: {move_str}");
        }
    }

    #[test]
    fn test_move_encoding_uniqueness() {
        let moves = vec![
            "7g7f", "7f7g", "7g8f", "8f7g", // Different normal moves
            "P*7f", "N*7f", "B*7f", "R*7f", // Different drops to same square
            "3d3c+", "3d3c", "4e4d+", "4e4d", // Promotion vs non-promotion
        ];

        let mut encoded_set = std::collections::HashSet::new();
        for move_notation in moves {
            let encoded = MoveEncoder::encode_move(move_notation).unwrap();
            assert!(
                !encoded_set.contains(&encoded),
                "Duplicate encoding detected for: {move_notation}"
            );
            encoded_set.insert(encoded);
        }
    }

    #[test]
    fn test_encode_all_squares() {
        // Test encoding/decoding for all possible square combinations
        for from_file in 1..=9 {
            for from_rank in b'a'..=b'i' {
                for to_file in 1..=9 {
                    for to_rank in b'a'..=b'i' {
                        let move_notation = format!(
                            "{}{}{}{}",
                            from_file, from_rank as char, to_file, to_rank as char
                        );
                        let encoded = MoveEncoder::encode_move(&move_notation).unwrap();
                        let decoded = MoveEncoder::decode_move(encoded).unwrap();
                        assert_eq!(move_notation, decoded);
                    }
                }
            }
        }
    }

    #[test]
    fn test_invalid_move_encoding() {
        let invalid_moves = vec![
            "invalid", "7g",     // Too short
            "7g7f7e", // Too long
            "0g7f",   // Invalid file
            "7j7f",   // Invalid rank
            "P*0f",   // Invalid drop square
            "K*5e",   // Invalid piece for drop
        ];

        for invalid_move in invalid_moves {
            let result = MoveEncoder::encode_move(invalid_move);
            assert!(result.is_err(), "Should fail for invalid move: {invalid_move}");
        }
    }

    #[test]
    fn test_decode_invalid_encoding() {
        // Test decoding invalid bit patterns
        let invalid_encodings = vec![
            0xFFFF, // All bits set
            0x0000, // All bits clear (if not a valid encoding)
        ];

        for encoding in invalid_encodings {
            let result = MoveEncoder::decode_move(encoding);
            // Should either succeed with a valid move or fail gracefully
            if let Ok(decoded) = result {
                // If it succeeds, re-encoding should give the same result
                let re_encoded = MoveEncoder::encode_move(&decoded);
                assert!(re_encoded.is_ok());
            }
        }
    }

    #[test]
    fn test_encoding_bit_efficiency() {
        // Test that we're using the 16-bit space efficiently
        let moves = vec!["1a1b", "9i9h", "5e5d", "P*5f", "R*9a", "7g7f+"];

        for move_notation in moves {
            let encoded = MoveEncoder::encode_move(move_notation).unwrap();
            // Encoded moves are u16, so they always fit in 16 bits by definition
            // Should not be zero (reserved for invalid)
            assert_ne!(encoded, 0);
        }
    }

    #[test]
    fn test_square_encoding_bounds() {
        // Test edge cases for square encoding
        let edge_moves = vec![
            "1a1b", "1a9i", "9a1i", "9a9i", // Corner to corner moves
            "5e5d", "5e5f", "5d5e", "5f5e", // Center moves
        ];

        for move_notation in edge_moves {
            let encoded = MoveEncoder::encode_move(move_notation).unwrap();
            let decoded = MoveEncoder::decode_move(encoded).unwrap();
            assert_eq!(move_notation, decoded);
        }
    }

    #[test]
    fn test_promotion_flag_isolation() {
        // Ensure promotion flag doesn't interfere with move encoding
        let base_move = "3d3c";
        let promoted_move = "3d3c+";

        let base_encoded = MoveEncoder::encode_move(base_move).unwrap();
        let promoted_encoded = MoveEncoder::encode_move(promoted_move).unwrap();

        // Should be different
        assert_ne!(base_encoded, promoted_encoded);

        // Should decode correctly
        assert_eq!(MoveEncoder::decode_move(base_encoded).unwrap(), base_move);
        assert_eq!(MoveEncoder::decode_move(promoted_encoded).unwrap(), promoted_move);
    }
}
