# Step-by-Step Implementation with Test Cases

## Overview

This document defines the implementation order and specific test cases for each component of the opening book conversion system. Following Test-Driven Development (TDD) principles, each implementation step includes failing tests that must be satisfied before moving to the next step.

## Implementation Order and Dependencies

```
1. Basic Data Structures
   ↓
2. SFEN Parser
   ↓
3. Move Encoder
   ↓
4. Position Hasher
   ↓
5. Position Filter
   ↓
6. Binary Converter
   ↓
7. File I/O Operations
   ↓
8. Integration Tests
```

## Step 1: Basic Data Structures

### Implementation Goal
Define the core data structures that all other components will use.

### Test Cases to Implement

```rust
// tests/data_structures_test.rs
#[cfg(test)]
mod data_structures_tests {
    use super::*;

    #[test]
    fn test_raw_sfen_entry_creation() {
        let entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 1,
            moves: vec![],
        };
        
        assert_eq!(entry.position.len(), 71); // Expected SFEN position length
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
}
```

### Implementation Steps
1. **Step 1.1**: Define `RawSfenEntry` struct - Test should fail initially
2. **Step 1.2**: Define `RawMove` struct - Test should fail initially  
3. **Step 1.3**: Define `CompactPosition` struct with correct memory layout
4. **Step 1.4**: Define `CompactMove` struct with correct memory layout
5. **Step 1.5**: Run tests and ensure they all pass

## Step 2: SFEN Parser

### Implementation Goal
Parse YaneuraOu SFEN format line by line, handling position lines and move lines correctly.

### Test Cases to Implement

```rust
// tests/sfen_parser_test.rs
#[cfg(test)]
mod sfen_parser_tests {
    use super::*;

    #[test]
    fn test_parse_header_line() {
        let parser = SfenParser::new();
        let result = parser.parse_line("#YANEURAOU-DB2016 1.00");
        
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Header should not return an entry
    }

    #[test]
    fn test_parse_position_line() {
        let parser = SfenParser::new();
        let line = "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let result = parser.parse_line(line);
        
        assert!(result.is_ok());
        // Should start a new position but not return completed entry yet
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_parse_move_line() {
        let mut parser = SfenParser::new();
        
        // First set up a position
        parser.parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
        
        // Then parse a move
        let result = parser.parse_line("7g7f none 50 2 1000");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Move added but entry not complete
    }

    #[test]
    fn test_complete_entry_on_empty_line() {
        let mut parser = SfenParser::new();
        
        // Set up position
        parser.parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
        
        // Add move
        parser.parse_line("7g7f none 50 2 1000").unwrap();
        
        // Empty line should complete the entry
        let result = parser.parse_line("");
        assert!(result.is_ok());
        let entry = result.unwrap();
        assert!(entry.is_some());
        
        let entry = entry.unwrap();
        assert_eq!(entry.position, "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL");
        assert_eq!(entry.turn, 'b');
        assert_eq!(entry.hand, "-");
        assert_eq!(entry.move_count, 1);
        assert_eq!(entry.moves.len(), 1);
        assert_eq!(entry.moves[0].move_notation, "7g7f");
    }

    #[test]
    fn test_parse_complex_position() {
        let mut parser = SfenParser::new();
        let complex_sfen = "sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0";
        
        let result = parser.parse_line(complex_sfen);
        assert!(result.is_ok());
        
        // Complete the entry
        let result = parser.parse_line("");
        assert!(result.is_ok());
        let entry = result.unwrap().unwrap();
        
        assert_eq!(entry.position, "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R");
        assert_eq!(entry.turn, 'b');
        assert_eq!(entry.hand, "NLP2sl2p");
        assert_eq!(entry.move_count, 0);
    }

    #[test]
    fn test_parse_multiple_moves() {
        let mut parser = SfenParser::new();
        
        parser.parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
        parser.parse_line("7g7f none 50 2 1000").unwrap();
        parser.parse_line("3c3d none 45 2 800").unwrap();
        parser.parse_line("P*5f none 20 1 500").unwrap();
        
        let result = parser.parse_line("");
        let entry = result.unwrap().unwrap();
        
        assert_eq!(entry.moves.len(), 3);
        assert_eq!(entry.moves[0].move_notation, "7g7f");
        assert_eq!(entry.moves[1].move_notation, "3c3d");
        assert_eq!(entry.moves[2].move_notation, "P*5f");
    }

    #[test]
    fn test_invalid_position_line() {
        let parser = SfenParser::new();
        let result = parser.parse_line("sfen invalid_format");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_move_line() {
        let mut parser = SfenParser::new();
        parser.parse_line("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
        
        let result = parser.parse_line("invalid move format");
        assert!(result.is_err());
    }
}
```

### Implementation Steps
1. **Step 2.1**: Create `SfenParser` struct with `new()` method - Tests should fail
2. **Step 2.2**: Implement `parse_line` method for header lines
3. **Step 2.3**: Implement parsing for position lines (sfen lines)
4. **Step 2.4**: Implement parsing for move lines
5. **Step 2.5**: Implement entry completion logic on empty lines
6. **Step 2.6**: Add error handling for malformed input
7. **Step 2.7**: Run all tests and ensure they pass

## Step 3: Move Encoder

### Implementation Goal
Encode and decode move notation to/from 16-bit integers efficiently.

### Test Cases to Implement

```rust
// tests/move_encoder_test.rs
#[cfg(test)]
mod move_encoder_tests {
    use super::*;

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
            assert!(result.is_ok(), "Failed to encode {}: {}", description, move_notation);
            
            let encoded = result.unwrap();
            let decoded = MoveEncoder::decode_move(encoded).unwrap();
            assert_eq!(move_notation, decoded, "Round-trip failed for {}", description);
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
            assert_eq!(move_notation, decoded, "Promotion encoding failed for {}", description);
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
            assert_eq!(move_notation, decoded, "Drop encoding failed for {}", description);
        }
    }

    #[test]
    fn test_move_encoding_uniqueness() {
        let moves = vec![
            "7g7f", "7f7g", "7g8f", "8f7g",  // Different normal moves
            "P*7f", "N*7f", "B*7f", "R*7f", // Different drops to same square
            "3d3c+", "3d3c", "4e4d+", "4e4d", // Promotion vs non-promotion
        ];

        let mut encoded_set = std::collections::HashSet::new();
        for move_notation in moves {
            let encoded = MoveEncoder::encode_move(move_notation).unwrap();
            assert!(!encoded_set.contains(&encoded), 
                   "Duplicate encoding detected for: {}", move_notation);
            encoded_set.insert(encoded);
        }
    }

    #[test]
    fn test_encode_all_squares() {
        // Test encoding/decoding for all possible square combinations
        for from_file in 1..=9 {
            for from_rank in 1..=9 {
                for to_file in 1..=9 {
                    for to_rank in 1..=9 {
                        let move_notation = format!("{}{}{}{}", from_file, from_rank, to_file, to_rank);
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
            "invalid",
            "7g",      // Too short
            "7g7f7e",  // Too long
            "0g7f",    // Invalid file
            "7a7f",    // Invalid rank
            "P*0f",    // Invalid drop square
            "K*5e",    // Invalid piece for drop
        ];

        for invalid_move in invalid_moves {
            let result = MoveEncoder::encode_move(invalid_move);
            assert!(result.is_err(), "Should fail for invalid move: {}", invalid_move);
        }
    }

    #[test]
    fn test_decode_invalid_encoding() {
        // Test decoding invalid bit patterns
        let invalid_encodings = vec![
            0xFFFF,  // All bits set
            0x0000,  // All bits clear (if not a valid encoding)
        ];

        for encoding in invalid_encodings {
            let result = MoveEncoder::decode_move(encoding);
            // Should either succeed with a valid move or fail gracefully
            if result.is_ok() {
                // If it succeeds, re-encoding should give the same result
                let decoded = result.unwrap();
                let re_encoded = MoveEncoder::encode_move(&decoded);
                assert!(re_encoded.is_ok());
            }
        }
    }
}
```

### Implementation Steps
1. **Step 3.1**: Create `MoveEncoder` struct - Tests should fail
2. **Step 3.2**: Implement `encode_move` for normal moves (no promotion, no drops)
3. **Step 3.3**: Implement `decode_move` for normal moves
4. **Step 3.4**: Add promotion handling to encoding/decoding
5. **Step 3.5**: Add drop move encoding/decoding
6. **Step 3.6**: Add comprehensive error handling
7. **Step 3.7**: Optimize encoding format for uniqueness
8. **Step 3.8**: Run all tests and ensure they pass

## Step 4: Position Hasher

### Implementation Goal
Generate consistent and collision-resistant hashes for Shogi positions.

### Test Cases to Implement

```rust
// tests/position_hasher_test.rs
#[cfg(test)]
mod position_hasher_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_hash_consistency() {
        let position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL";
        let turn = 'b';
        let hand = "-";

        // Hash should be identical across multiple calls
        let hash1 = PositionHasher::hash_position(position, turn, hand);
        let hash2 = PositionHasher::hash_position(position, turn, hand);
        let hash3 = PositionHasher::hash_position(position, turn, hand);

        assert_eq!(hash1, hash2);
        assert_eq!(hash2, hash3);
        assert_ne!(hash1, 0); // Hash should not be zero
    }

    #[test]
    fn test_hash_sensitivity_to_position() {
        let base_position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL";
        let base_hash = PositionHasher::hash_position(base_position, 'b', "-");

        let position_variants = vec![
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/7PP/PPPPPPP2/1B5R1/LNSGKGSNL", // Moved pawn
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSN1", // Different piece order
            "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R", // Complex position
        ];

        for variant in position_variants {
            let variant_hash = PositionHasher::hash_position(variant, 'b', "-");
            assert_ne!(base_hash, variant_hash, "Hash collision between positions: {} vs {}", base_position, variant);
        }
    }

    #[test]
    fn test_hash_sensitivity_to_turn() {
        let position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL";
        let hand = "-";

        let black_hash = PositionHasher::hash_position(position, 'b', hand);
        let white_hash = PositionHasher::hash_position(position, 'w', hand);

        assert_ne!(black_hash, white_hash, "Hash should differ for different turns");
    }

    #[test]
    fn test_hash_sensitivity_to_hand() {
        let position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL";
        let turn = 'b';

        let empty_hand_hash = PositionHasher::hash_position(position, turn, "-");
        let pawn_hand_hash = PositionHasher::hash_position(position, turn, "P");
        let multi_hand_hash = PositionHasher::hash_position(position, turn, "NLP2sl2p");

        assert_ne!(empty_hand_hash, pawn_hand_hash, "Hash should differ for different hands");
        assert_ne!(pawn_hand_hash, multi_hand_hash, "Hash should differ for different hands");
        assert_ne!(empty_hand_hash, multi_hand_hash, "Hash should differ for different hands");
    }

    #[test]
    fn test_hash_sfen_entry() {
        let entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 1,
            moves: vec![],
        };

        let hash1 = PositionHasher::hash_sfen_entry(&entry);
        let hash2 = PositionHasher::hash_position(&entry.position, entry.turn, &entry.hand);

        assert_eq!(hash1, hash2, "hash_sfen_entry should match hash_position");
    }

    #[test]
    fn test_hash_distribution() {
        // Generate many different positions and check for reasonable distribution
        let mut positions = Vec::new();
        
        // Add various initial positions with different hands
        for pieces in &["-", "P", "PP", "NP", "BRP", "NLSG", "NLP2sl2p"] {
            positions.push(("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL", 'b', *pieces));
            positions.push(("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL", 'w', *pieces));
        }

        // Add some complex positions
        positions.push(("+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R", 'b', "NLP2sl2p"));
        positions.push(("+B+B4gkl/4+Lg1s1/pK4np1/5Pp1p/1P1+r5/2P2GPPP/P3+n4/2+p3+s2/L6N1", 'b', "SL3Prgsn3p"));

        let mut hashes = HashSet::new();
        let mut collisions = 0;

        for (position, turn, hand) in positions {
            let hash = PositionHasher::hash_position(position, turn, hand);
            if hashes.contains(&hash) {
                collisions += 1;
                println!("Hash collision for: {} {} {}", position, turn, hand);
            }
            hashes.insert(hash);
        }

        assert_eq!(collisions, 0, "Found {} hash collisions", collisions);
    }

    #[test]
    fn test_hash_performance() {
        let position = "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R";
        let turn = 'b';
        let hand = "NLP2sl2p";

        let start = std::time::Instant::now();
        for _ in 0..10000 {
            let _ = PositionHasher::hash_position(position, turn, hand);
        }
        let duration = start.elapsed();

        // Should be able to hash 10k positions in under 100ms
        assert!(duration.as_millis() < 100, "Hashing too slow: {} ms for 10k positions", duration.as_millis());
    }
}
```

### Implementation Steps
1. **Step 4.1**: Create `PositionHasher` struct - Tests should fail
2. **Step 4.2**: Implement basic `hash_position` using simple string concatenation + hash
3. **Step 4.3**: Improve hash function to handle position components separately
4. **Step 4.4**: Implement `hash_sfen_entry` wrapper method
5. **Step 4.5**: Optimize for performance
6. **Step 4.6**: Test for hash distribution and collision resistance
7. **Step 4.7**: Run all tests and ensure they pass

## Step 5: Position Filter

### Implementation Goal
Filter positions based on quality criteria (depth, evaluation, move count).

### Test Cases to Implement

```rust
// tests/position_filter_test.rs
#[cfg(test)]
mod position_filter_tests {
    use super::*;

    #[test]
    fn test_default_filter_creation() {
        let filter = PositionFilter::new();
        
        // Test default values are reasonable
        assert!(filter.min_depth >= 0);
        assert!(filter.max_moves > 0);
        assert!(filter.min_evaluation < 0);
        assert!(filter.max_evaluation > 0);
    }

    #[test]
    fn test_filter_by_move_count() {
        let mut filter = PositionFilter::new();
        filter.max_moves = 20;

        let valid_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 15,
            moves: vec![create_test_move("7g7f", 50, 2)],
        };

        let invalid_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 25,  // Exceeds max_moves
            moves: vec![create_test_move("7g7f", 50, 2)],
        };

        assert!(filter.should_include(&valid_entry));
        assert!(!filter.should_include(&invalid_entry));
    }

    #[test]
    fn test_filter_by_depth() {
        let mut filter = PositionFilter::new();
        filter.min_depth = 2;

        let valid_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![
                create_test_move("7g7f", 50, 1),  // Below min_depth
                create_test_move("2g2f", 45, 3),  // Above min_depth
            ],
        };

        let invalid_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![
                create_test_move("7g7f", 50, 1),  // All moves below min_depth
                create_test_move("2g2f", 45, 1),
            ],
        };

        assert!(filter.should_include(&valid_entry));  // Has at least one deep move
        assert!(!filter.should_include(&invalid_entry)); // No deep moves
    }

    #[test]
    fn test_filter_by_evaluation() {
        let mut filter = PositionFilter::new();
        filter.min_evaluation = -200;
        filter.max_evaluation = 500;

        let valid_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![create_test_move("7g7f", 100, 2)], // Within range
        };

        let invalid_low_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![create_test_move("7g7f", -300, 2)], // Too low
        };

        let invalid_high_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![create_test_move("7g7f", 600, 2)], // Too high
        };

        assert!(filter.should_include(&valid_entry));
        assert!(!filter.should_include(&invalid_low_entry));
        assert!(!filter.should_include(&invalid_high_entry));
    }

    #[test]
    fn test_filter_moves() {
        let filter = PositionFilter::new();
        
        let mut entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![
                create_test_move("move1", 100, 2),
                create_test_move("move2", 80, 2),
                create_test_move("move3", 60, 2),
                create_test_move("move4", 40, 2),
                create_test_move("move5", 20, 2),
                create_test_move("move6", 0, 2),
                create_test_move("move7", -20, 2),
                create_test_move("move8", -40, 2),
                create_test_move("move9", -60, 2),
                create_test_move("move10", -80, 2),
            ],
        };

        let original_len = entry.moves.len();
        filter.filter_moves(&mut entry);

        // Should keep only top moves and sort by evaluation
        assert!(entry.moves.len() <= 8);  // Default max moves to keep
        assert!(entry.moves.len() < original_len);
        
        // Should be sorted by evaluation (best first)
        for i in 1..entry.moves.len() {
            assert!(entry.moves[i-1].evaluation >= entry.moves[i].evaluation);
        }
    }

    #[test]
    fn test_empty_moves_filtered() {
        let filter = PositionFilter::new();
        
        let empty_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 10,
            moves: vec![],
        };

        assert!(!filter.should_include(&empty_entry));
    }

    #[test]
    fn test_combined_filtering_criteria() {
        let mut filter = PositionFilter::new();
        filter.min_depth = 2;
        filter.max_moves = 30;
        filter.min_evaluation = -100;
        filter.max_evaluation = 300;

        let valid_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 25,  // Within move count limit
            moves: vec![
                create_test_move("move1", 50, 3),   // Good depth and evaluation
                create_test_move("move2", -50, 1),  // Poor depth but best eval is good
            ],
        };

        let invalid_entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 35,  // Exceeds move count limit
            moves: vec![create_test_move("move1", 50, 3)],
        };

        assert!(filter.should_include(&valid_entry));
        assert!(!filter.should_include(&invalid_entry));
    }

    // Helper function
    fn create_test_move(notation: &str, eval: i32, depth: u32) -> RawMove {
        RawMove {
            move_notation: notation.to_string(),
            move_type: "none".to_string(),
            evaluation: eval,
            depth,
            nodes: 1000,
        }
    }
}
```

### Implementation Steps
1. **Step 5.1**: Create `PositionFilter` struct with configuration - Tests should fail
2. **Step 5.2**: Implement `new()` with reasonable defaults
3. **Step 5.3**: Implement move count filtering
4. **Step 5.4**: Implement depth filtering (at least one move meets criteria)
5. **Step 5.5**: Implement evaluation filtering (best move within range)
6. **Step 5.6**: Implement `filter_moves` to keep only top moves
7. **Step 5.7**: Combine all filtering criteria in `should_include`
8. **Step 5.8**: Run all tests and ensure they pass

## Step 6: Binary Converter

### Implementation Goal
Convert parsed SFEN entries to compact binary format with indexing.

### Test Cases to Implement

```rust
// tests/binary_converter_test.rs
#[cfg(test)]
mod binary_converter_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_convert_single_entry() {
        let temp_dir = TempDir::new().unwrap();
        let bin_path = temp_dir.path().join("test.bin");
        let idx_path = temp_dir.path().join("test.idx");

        let mut converter = BinaryConverter::new(
            bin_path.to_str().unwrap(),
            idx_path.to_str().unwrap()
        ).unwrap();

        let entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 1,
            moves: vec![
                create_test_move("7g7f", 50, 2),
                create_test_move("2g2f", 45, 2),
            ],
        };

        let result = converter.write_position(&entry);
        assert!(result.is_ok());

        converter.finalize().unwrap();

        // Check files were created
        assert!(bin_path.exists());
        assert!(idx_path.exists());
        assert!(bin_path.metadata().unwrap().len() > 0);
        assert!(idx_path.metadata().unwrap().len() > 0);
    }

    #[test]
    fn test_convert_multiple_entries() {
        let temp_dir = TempDir::new().unwrap();
        let bin_path = temp_dir.path().join("test.bin");
        let idx_path = temp_dir.path().join("test.idx");

        let mut converter = BinaryConverter::new(
            bin_path.to_str().unwrap(),
            idx_path.to_str().unwrap()
        ).unwrap();

        let entries = vec![
            create_test_entry("position1", 1, vec![("7g7f", 50, 2)]),
            create_test_entry("position2", 2, vec![("2g2f", 45, 2), ("3c3d", 40, 2)]),
            create_test_entry("position3", 3, vec![("P*5f", 30, 1)]),
        ];

        for entry in &entries {
            converter.write_position(entry).unwrap();
        }

        converter.finalize().unwrap();

        // Verify index has correct number of entries
        let index_size = idx_path.metadata().unwrap().len();
        let expected_entries = entries.len();
        assert_eq!(index_size as usize, expected_entries * std::mem::size_of::<PositionIndex>());
    }

    #[test]
    fn test_compact_position_conversion() {
        let entry = RawSfenEntry {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 1,
            moves: vec![
                create_test_move("7g7f", 100, 3),
                create_test_move("2g2f", 80, 2),
                create_test_move("P*5f", 60, 1),
            ],
        };

        let compact = BinaryConverter::entry_to_compact(&entry).unwrap();

        // Verify compact format
        assert_ne!(compact.position_hash, 0);
        assert_eq!(compact.move_count, 3);
        assert_eq!(compact.evaluation, 100); // Best move evaluation
        assert_eq!(compact.depth, 3);        // Best move depth

        // Verify best move encoding
        let best_move = MoveEncoder::decode_move(compact.best_move).unwrap();
        assert_eq!(best_move, "7g7f");
    }

    #[test]
    fn test_position_hash_uniqueness() {
        let entries = vec![
            create_test_entry("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL", 1, vec![("7g7f", 50, 2)]),
            create_test_entry("lnsgkgsnl/1r5b1/ppppppppp/9/9/7PP/PPPPPPP2/1B5R1/LNSGKGSNL", 2, vec![("2g2f", 45, 2)]),
            create_test_entry("+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R", 3, vec![("8i7g", 40, 2)]),
        ];

        let mut hashes = std::collections::HashSet::new();
        for entry in entries {
            let compact = BinaryConverter::entry_to_compact(&entry).unwrap();
            assert!(!hashes.contains(&compact.position_hash), 
                   "Hash collision detected for position: {}", entry.position);
            hashes.insert(compact.position_hash);
        }
    }

    #[test]
    fn test_file_format_compliance() {
        let temp_dir = TempDir::new().unwrap();
        let bin_path = temp_dir.path().join("test.bin");
        let idx_path = temp_dir.path().join("test.idx");

        let mut converter = BinaryConverter::new(
            bin_path.to_str().unwrap(),
            idx_path.to_str().unwrap()
        ).unwrap();

        let entry = create_test_entry("test_position", 1, vec![("7g7f", 50, 2)]);
        converter.write_position(&entry).unwrap();
        converter.finalize().unwrap();

        // Read back binary data and verify format
        let bin_data = std::fs::read(&bin_path).unwrap();
        let idx_data = std::fs::read(&idx_path).unwrap();

        // Verify index entry size
        assert_eq!(idx_data.len(), std::mem::size_of::<PositionIndex>());

        // Verify binary data starts with CompactPosition
        assert!(bin_data.len() >= std::mem::size_of::<CompactPosition>());
    }

    #[test]
    fn test_error_handling() {
        // Test with invalid path
        let result = BinaryConverter::new("/invalid/path/test.bin", "/invalid/path/test.idx");
        assert!(result.is_err());

        // Test with empty entry
        let temp_dir = TempDir::new().unwrap();
        let bin_path = temp_dir.path().join("test.bin");
        let idx_path = temp_dir.path().join("test.idx");

        let mut converter = BinaryConverter::new(
            bin_path.to_str().unwrap(),
            idx_path.to_str().unwrap()
        ).unwrap();

        let empty_entry = RawSfenEntry {
            position: "".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 0,
            moves: vec![],
        };

        let result = converter.write_position(&empty_entry);
        assert!(result.is_err());
    }

    // Helper functions
    fn create_test_entry(position: &str, move_count: u32, moves: Vec<(&str, i32, u32)>) -> RawSfenEntry {
        RawSfenEntry {
            position: position.to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count,
            moves: moves.into_iter().map(|(notation, eval, depth)| 
                create_test_move(notation, eval, depth)
            ).collect(),
        }
    }

    fn create_test_move(notation: &str, eval: i32, depth: u32) -> RawMove {
        RawMove {
            move_notation: notation.to_string(),
            move_type: "none".to_string(),
            evaluation: eval,
            depth,
            nodes: 1000,
        }
    }
}
```

### Implementation Steps
1. **Step 6.1**: Create `BinaryConverter` struct with file handles - Tests should fail
2. **Step 6.2**: Implement `new()` method with file creation
3. **Step 6.3**: Implement `entry_to_compact()` conversion method
4. **Step 6.4**: Implement `write_position()` method
5. **Step 6.5**: Implement index creation and management
6. **Step 6.6**: Implement `finalize()` method
7. **Step 6.7**: Add error handling for file I/O operations
8. **Step 6.8**: Run all tests and ensure they pass

## Step 7: Integration Tests

### Implementation Goal
Test the complete conversion pipeline from SFEN input to binary output.

### Test Cases to Implement

```rust
// tests/integration_test.rs
#[cfg(test)]
mod integration_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_complete_conversion_pipeline() {
        let temp_dir = TempDir::new().unwrap();
        let input_file = temp_dir.path().join("input.sfen");
        let output_file = temp_dir.path().join("output.bin");
        let index_file = temp_dir.path().join("output.idx");

        // Create test input file
        let test_content = create_test_sfen_file_content();
        std::fs::write(&input_file, test_content).unwrap();

        // Run conversion
        let result = convert_opening_book(
            input_file.to_str().unwrap(),
            output_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::default()
        );

        assert!(result.is_ok());
        let stats = result.unwrap();

        // Verify conversion stats
        assert!(stats.total_positions > 0);
        assert!(stats.filtered_positions > 0);
        assert!(stats.filtered_positions <= stats.total_positions);

        // Verify output files
        assert!(output_file.exists());
        assert!(index_file.exists());
        assert!(output_file.metadata().unwrap().len() > 0);
        assert!(index_file.metadata().unwrap().len() > 0);
    }

    #[test]
    fn test_round_trip_conversion() {
        let temp_dir = TempDir::new().unwrap();
        let original_file = temp_dir.path().join("original.sfen");
        let binary_file = temp_dir.path().join("converted.bin");
        let index_file = temp_dir.path().join("converted.idx");

        // Create comprehensive test data
        let test_content = create_comprehensive_test_data();
        std::fs::write(&original_file, test_content).unwrap();

        // Convert to binary
        convert_opening_book(
            original_file.to_str().unwrap(),
            binary_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::no_filtering()
        ).unwrap();

        // Load and verify all positions can be found
        let book = OpeningBook::load(
            binary_file.to_str().unwrap(),
            index_file.to_str().unwrap()
        ).unwrap();

        // Parse original file and check each position
        let original_entries = parse_sfen_file(&original_file).unwrap();
        
        for original_entry in original_entries {
            let position_sfen = format!("{} {} {} {}", 
                original_entry.position, 
                original_entry.turn, 
                original_entry.hand, 
                original_entry.move_count);

            let found_move = book.lookup_position(&position_sfen);
            assert!(found_move.is_some(), "Position not found: {}", position_sfen);

            // Verify best move matches
            if let Some(best_move) = found_move {
                let original_best = original_entry.moves.iter()
                    .max_by_key(|m| m.evaluation)
                    .unwrap();
                assert_eq!(best_move.move_notation, original_best.move_notation);
                assert!((best_move.evaluation - original_best.evaluation).abs() <= 1);
            }
        }
    }

    #[test]
    fn test_performance_targets() {
        let temp_dir = TempDir::new().unwrap();
        let input_file = temp_dir.path().join("perf_test.sfen");
        let output_file = temp_dir.path().join("perf_test.bin");
        let index_file = temp_dir.path().join("perf_test.idx");

        // Create large test dataset
        let large_content = create_large_test_dataset(10000); // 10k positions
        std::fs::write(&input_file, large_content).unwrap();

        // Measure conversion time
        let start_time = std::time::Instant::now();
        let result = convert_opening_book(
            input_file.to_str().unwrap(),
            output_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::default()
        );
        let conversion_time = start_time.elapsed();

        assert!(result.is_ok());

        // Performance assertions
        assert!(conversion_time.as_secs() < 60, "Conversion too slow: {} seconds", conversion_time.as_secs());

        // Load book and test search performance
        let book = OpeningBook::load(
            output_file.to_str().unwrap(),
            index_file.to_str().unwrap()
        ).unwrap();

        let test_position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        
        let search_start = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = book.lookup_position(test_position);
        }
        let search_time = search_start.elapsed();
        let avg_search_time = search_time / 1000;

        assert!(avg_search_time.as_micros() < 1000, "Search too slow: {} μs", avg_search_time.as_micros());
    }

    #[test]
    fn test_compression_ratio() {
        let temp_dir = TempDir::new().unwrap();
        let input_file = temp_dir.path().join("compression_test.sfen");
        let output_file = temp_dir.path().join("compression_test.bin");
        let index_file = temp_dir.path().join("compression_test.idx");

        let test_content = create_test_sfen_file_content();
        std::fs::write(&input_file, &test_content).unwrap();

        convert_opening_book(
            input_file.to_str().unwrap(),
            output_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::default()
        ).unwrap();

        let original_size = input_file.metadata().unwrap().len();
        let binary_size = output_file.metadata().unwrap().len();
        let index_size = index_file.metadata().unwrap().len();
        let total_compressed = binary_size + index_size;

        let compression_ratio = total_compressed as f64 / original_size as f64;

        // Should achieve significant compression
        assert!(compression_ratio < 0.5, "Insufficient compression: {:.2}%", compression_ratio * 100.0);
        assert!(compression_ratio > 0.05, "Suspiciously high compression: {:.2}%", compression_ratio * 100.0);
    }

    #[test]
    fn test_error_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let input_file = temp_dir.path().join("error_test.sfen");
        let output_file = temp_dir.path().join("error_test.bin");
        let index_file = temp_dir.path().join("error_test.idx");

        // Create input with some malformed entries
        let mixed_content = create_mixed_quality_test_data();
        std::fs::write(&input_file, mixed_content).unwrap();

        let result = convert_opening_book(
            input_file.to_str().unwrap(),
            output_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::default()
        );

        // Should succeed despite some bad entries
        assert!(result.is_ok());
        let stats = result.unwrap();
        
        // Should have filtered out bad entries
        assert!(stats.filtered_positions < stats.total_positions);
        assert!(stats.error_count > 0);

        // Output files should still be created
        assert!(output_file.exists());
        assert!(index_file.exists());
    }

    // Helper functions
    fn create_test_sfen_file_content() -> String {
        format!(
            "{}\n{}\n{}\n{}\n{}\n\n{}\n{}\n{}\n\n{}\n{}\n{}\n{}\n\n",
            "#YANEURAOU-DB2016 1.00",
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "7g7f none 50 2 1000",
            "2g2f none 48 2 950",
            "P*5f none 20 1 500",
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
            "3c3d none 45 2 800",
            "8c8d none 42 2 750",
            "sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0",
            "8i7g none 194 3 1500",
            "3d3c+ none -203 0 0",
            "7h7g none -325 0 0"
        )
    }

    fn create_comprehensive_test_data() -> String {
        // Generate varied test data that covers edge cases
        let mut content = String::new();
        content.push_str("#YANEURAOU-DB2016 1.00\n");

        // Add various position types
        let positions = vec![
            ("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL", "b", "-", 1),
            ("+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R", "b", "NLP2sl2p", 0),
            ("+B+B4gkl/4+Lg1s1/pK4np1/5Pp1p/1P1+r5/2P2GPPP/P3+n4/2+p3+s2/L6N1", "b", "SL3Prgsn3p", 20),
        ];

        for (pos, turn, hand, move_count) in positions {
            content.push_str(&format!("sfen {} {} {} {}\n", pos, turn, hand, move_count));
            content.push_str("7g7f none 100 3 2000\n");
            content.push_str("2g2f none 80 2 1500\n");
            content.push_str("P*5f none 60 1 1000\n");
            content.push('\n');
        }

        content
    }

    fn create_large_test_dataset(position_count: usize) -> String {
        let mut content = String::new();
        content.push_str("#YANEURAOU-DB2016 1.00\n");

        for i in 0..position_count {
            content.push_str(&format!(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - {}\n",
                i + 1
            ));
            content.push_str(&format!("7g7f none {} 2 1000\n", 100 - (i % 200) as i32));
            content.push_str(&format!("2g2f none {} 2 950\n", 90 - (i % 180) as i32));
            content.push('\n');
        }

        content
    }

    fn create_mixed_quality_test_data() -> String {
        format!(
            "{}\n{}\n{}\n{}\n\n{}\n{}\n\ninvalid line format\n\n{}\n{}\n{}\n\n",
            "#YANEURAOU-DB2016 1.00",
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "7g7f none 50 2 1000",
            "invalid_move none 100 2 1000",  // Invalid move
            "sfen invalid_board_format b - 1",  // Invalid board
            "7g7f none 50 2 1000",
            "sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0",
            "8i7g none 194 3 1500",
            "3d3c+ none -203 0 0"
        )
    }
}
```

### Implementation Steps
1. **Step 7.1**: Create integration test framework - Tests should fail
2. **Step 7.2**: Implement `convert_opening_book` main function
3. **Step 7.3**: Implement `OpeningBook::load` method
4. **Step 7.4**: Implement position lookup functionality
5. **Step 7.5**: Add performance measurement utilities
6. **Step 7.6**: Add error handling and recovery
7. **Step 7.7**: Run all integration tests and ensure they pass

## TDD Workflow Summary

### For Each Implementation Step:

1. **Red Phase**: Write failing tests first
   ```bash
   cargo test [component_name] # Should fail
   ```

2. **Green Phase**: Write minimal implementation to pass tests
   ```bash
   cargo test [component_name] # Should pass
   ```

3. **Refactor Phase**: Improve code quality while keeping tests green
   ```bash
   cargo test [component_name] # Should still pass
   cargo clippy # Check for improvements
   cargo fmt # Format code
   ```

4. **Integration**: Run all tests to ensure no regressions
   ```bash
   cargo test # All tests should pass
   ```

### Implementation Order Enforcement

Each step builds on the previous ones:
- Step 2 (SfenParser) requires Step 1 (Data Structures)
- Step 3 (MoveEncoder) is independent but tested with Step 1
- Step 4 (PositionHasher) requires Step 1
- Step 5 (PositionFilter) requires Steps 1-2
- Step 6 (BinaryConverter) requires Steps 1-5
- Step 7 (Integration) requires all previous steps

This step-by-step approach with comprehensive test coverage ensures that each component works correctly before building the next layer, leading to a robust and reliable implementation.