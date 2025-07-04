# Comprehensive Test Cases for Opening Book Conversion

## Overview

This document defines comprehensive test cases to ensure the accuracy and reliability of the opening book conversion process. All tests are designed to validate that the converted data maintains the same semantic meaning as the original YaneuraOu SFEN format.

## Test Categories

### 1. Unit Tests

#### 1.1 SFEN Parsing Tests
```rust
#[cfg(test)]
mod sfen_parsing_tests {
    use super::*;
    use test_case::test_case;

    #[test_case(
        "sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0",
        "Standard midgame position"
    )]
    #[test_case(
        "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        "Initial position"
    )]
    #[test_case(
        "sfen +B+B4gkl/4+Lg1s1/pK4np1/5Pp1p/1P1+r5/2P2GPPP/P3+n4/2+p3+s2/L6N1 b SL3Prgsn3p 0",
        "Complex promoted pieces position"
    )]
    fn test_sfen_position_parsing(sfen: &str, description: &str) {
        let result = SfenParser::parse_position_line(sfen);
        assert!(result.is_ok(), "Failed to parse {}: {}", description, sfen);
        
        let parsed = result.unwrap();
        let reconstructed = parsed.to_sfen_string();
        assert_eq!(sfen, reconstructed, "Round-trip failed for {}", description);
    }

    #[test_case("8i7g", "Normal pawn move")]
    #[test_case("3d3c+", "Promotion move")]
    #[test_case("P*6g", "Pawn drop")]
    #[test_case("N*4e", "Knight drop")]
    #[test_case("1a1b+", "Promotion at edge")]
    #[test_case("9i9h", "Corner move")]
    fn test_move_parsing(move_notation: &str, description: &str) {
        let result = SfenParser::parse_move_line(&format!("{} none 100 3 1000", move_notation));
        assert!(result.is_ok(), "Failed to parse move {}: {}", description, move_notation);
        
        let parsed = result.unwrap();
        assert_eq!(parsed.move_notation, move_notation);
    }

    #[test]
    fn test_malformed_sfen_handling() {
        let malformed_cases = vec![
            "sfen invalid_board b - 0",  // Invalid board
            "sfen lnsgkgsnl/1r5b1 invalid_turn - 0",  // Invalid turn
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - invalid_count",  // Invalid move count
        ];

        for case in malformed_cases {
            let result = SfenParser::parse_position_line(case);
            assert!(result.is_err(), "Should fail for malformed SFEN: {}", case);
        }
    }
}
```

#### 1.2 Move Encoding Tests
```rust
#[cfg(test)]
mod move_encoding_tests {
    use super::*;
    use test_case::test_case;

    #[test_case("8i7g", "Normal move")]
    #[test_case("3d3c+", "Promotion move")]
    #[test_case("1a9i", "Diagonal move")]
    #[test_case("5e5d", "Central move")]
    fn test_normal_move_encoding(move_notation: &str, description: &str) {
        let encoded = MoveEncoder::encode_move(move_notation).unwrap();
        let decoded = MoveEncoder::decode_move(encoded).unwrap();
        assert_eq!(move_notation, decoded, "Round-trip failed for {}", description);
    }

    #[test_case("P*5g", "Pawn drop")]
    #[test_case("N*4e", "Knight drop")]
    #[test_case("B*7e", "Bishop drop")]
    #[test_case("R*6c", "Rook drop")]
    #[test_case("G*3f", "Gold drop")]
    #[test_case("S*2d", "Silver drop")]
    #[test_case("L*9f", "Lance drop")]
    fn test_drop_move_encoding(move_notation: &str, description: &str) {
        let encoded = MoveEncoder::encode_move(move_notation).unwrap();
        let decoded = MoveEncoder::decode_move(encoded).unwrap();
        assert_eq!(move_notation, decoded, "Round-trip failed for {}", description);
    }

    #[test]
    fn test_move_encoding_edge_cases() {
        // Test all squares
        for file in 1..=9 {
            for rank in 1..=9 {
                for target_file in 1..=9 {
                    for target_rank in 1..=9 {
                        let move_notation = format!("{}{}{}{}", file, rank, target_file, target_rank);
                        let encoded = MoveEncoder::encode_move(&move_notation).unwrap();
                        let decoded = MoveEncoder::decode_move(encoded).unwrap();
                        assert_eq!(move_notation, decoded);
                    }
                }
            }
        }
    }

    #[test]
    fn test_encoding_uniqueness() {
        let moves = vec![
            "8i7g", "7g8i", "8i7h", "7h8i",
            "P*5g", "N*5g", "B*5g", "R*5g",
            "3d3c+", "3d3c", "4e4d+", "4e4d"
        ];

        let mut encoded_moves = std::collections::HashSet::new();
        for move_notation in moves {
            let encoded = MoveEncoder::encode_move(move_notation).unwrap();
            assert!(!encoded_moves.contains(&encoded), 
                   "Duplicate encoding for move: {}", move_notation);
            encoded_moves.insert(encoded);
        }
    }
}
```

#### 1.3 Position Hashing Tests
```rust
#[cfg(test)]
mod position_hashing_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_hash_consistency() {
        let position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL";
        let turn = 'b';
        let hand = "-";

        // Hash should be consistent across multiple calls
        let hash1 = PositionHasher::hash_position(position, turn, hand);
        let hash2 = PositionHasher::hash_position(position, turn, hand);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_sensitivity() {
        let base_position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL";
        let base_hash = PositionHasher::hash_position(base_position, 'b', "-");

        // Different positions should have different hashes
        let variations = vec![
            ("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL", 'w', "-"),  // Different turn
            ("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL", 'b', "P"),  // Different hand
            ("lnsgkgsnl/1r5b1/ppppppppp/9/9/7PP/PPPPPPP2/1B5R1/LNSGKGSNL", 'b', "-"),  // Different position
        ];

        for (position, turn, hand) in variations {
            let hash = PositionHasher::hash_position(position, turn, hand);
            assert_ne!(base_hash, hash, "Hash collision detected for position: {}", position);
        }
    }

    #[test]
    fn test_hash_distribution() {
        let test_positions = generate_test_positions(1000);
        let mut hashes = HashSet::new();
        let mut collisions = 0;

        for entry in test_positions {
            let hash = PositionHasher::hash_sfen_entry(&entry);
            if hashes.contains(&hash) {
                collisions += 1;
            }
            hashes.insert(hash);
        }

        // Expect very few collisions for good hash distribution
        assert!(collisions < 5, "Too many hash collisions: {}", collisions);
    }
}
```

### 2. Integration Tests

#### 2.1 End-to-End Conversion Tests
```rust
#[cfg(test)]
mod integration_tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_small_sample_conversion() {
        let temp_dir = TempDir::new().unwrap();
        let input_file = temp_dir.path().join("test_input.sfen");
        let output_file = temp_dir.path().join("test_output.bin");
        let index_file = temp_dir.path().join("test_output.idx");

        // Create test input
        let test_data = create_test_sfen_data();
        fs::write(&input_file, test_data).unwrap();

        // Run conversion
        let result = convert_opening_book(
            input_file.to_str().unwrap(),
            output_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::default()
        );

        assert!(result.is_ok(), "Conversion failed: {:?}", result.err());

        // Validate output files exist
        assert!(output_file.exists(), "Output binary file not created");
        assert!(index_file.exists(), "Output index file not created");

        // Validate file sizes are reasonable
        let output_size = fs::metadata(&output_file).unwrap().len();
        let index_size = fs::metadata(&index_file).unwrap().len();
        assert!(output_size > 0, "Output file is empty");
        assert!(index_size > 0, "Index file is empty");
    }

    #[test]
    fn test_conversion_with_filtering() {
        let temp_dir = TempDir::new().unwrap();
        let input_file = temp_dir.path().join("test_input.sfen");
        let output_file = temp_dir.path().join("test_output.bin");
        let index_file = temp_dir.path().join("test_output.idx");

        // Create test input with some low-quality entries
        let test_data = create_mixed_quality_test_data();
        fs::write(&input_file, test_data).unwrap();

        let options = ConversionOptions {
            min_depth: 1,
            max_moves: 50,
            min_evaluation: -500,
            max_evaluation: 500,
        };

        let result = convert_opening_book(
            input_file.to_str().unwrap(),
            output_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            options
        );

        assert!(result.is_ok());

        // Verify filtering worked
        let stats = result.unwrap();
        assert!(stats.filtered_positions < stats.total_positions);
    }

    #[test]
    fn test_round_trip_accuracy() {
        let temp_dir = TempDir::new().unwrap();
        let original_file = temp_dir.path().join("original.sfen");
        let binary_file = temp_dir.path().join("converted.bin");
        let index_file = temp_dir.path().join("converted.idx");
        let reconstructed_file = temp_dir.path().join("reconstructed.sfen");

        // Create test data
        let original_data = create_comprehensive_test_data();
        fs::write(&original_file, &original_data).unwrap();

        // Convert to binary
        let conversion_result = convert_opening_book(
            original_file.to_str().unwrap(),
            binary_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::default()
        );
        assert!(conversion_result.is_ok());

        // Convert back to SFEN
        let reconstruction_result = reconstruct_sfen_from_binary(
            binary_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            reconstructed_file.to_str().unwrap()
        );
        assert!(reconstruction_result.is_ok());

        // Compare original and reconstructed
        let original_entries = parse_sfen_file(&original_file).unwrap();
        let reconstructed_entries = parse_sfen_file(&reconstructed_file).unwrap();

        assert_eq!(original_entries.len(), reconstructed_entries.len());

        for (orig, recon) in original_entries.iter().zip(reconstructed_entries.iter()) {
            assert_eq!(orig.position, recon.position);
            assert_eq!(orig.turn, recon.turn);
            assert_eq!(orig.hand, recon.hand);
            assert_eq!(orig.move_count, recon.move_count);
            
            // Check best moves match
            let orig_best = orig.moves.iter().max_by_key(|m| m.evaluation).unwrap();
            let recon_best = recon.moves.iter().max_by_key(|m| m.evaluation).unwrap();
            assert_eq!(orig_best.move_notation, recon_best.move_notation);
            assert!((orig_best.evaluation - recon_best.evaluation).abs() <= 1);
        }
    }
}
```

#### 2.2 Performance Tests
```rust
#[cfg(test)]
mod performance_tests {
    use super::*;
    use criterion::{criterion_group, criterion_main, Criterion};
    use std::time::Instant;

    #[test]
    fn test_search_performance() {
        let book = create_test_opening_book(10000);  // 10k positions
        let test_positions = create_test_search_positions(1000);
        
        let start = Instant::now();
        for position in &test_positions {
            let _ = book.lookup_position(&position.sfen);
        }
        let total_time = start.elapsed();
        let avg_time = total_time / test_positions.len() as u32;

        // Should average under 1ms per lookup
        assert!(avg_time.as_millis() < 1, 
               "Search too slow: {} ms average", avg_time.as_millis());
    }

    #[test]
    fn test_memory_usage() {
        let book = create_test_opening_book(50000);  // 50k positions
        
        // Rough estimate of memory usage
        let estimated_memory = std::mem::size_of_val(&book) + 
                              book.positions.len() * std::mem::size_of::<CompactPosition>() +
                              book.index.len() * (std::mem::size_of::<u64>() + std::mem::size_of::<usize>());
        
        // Should use less than 100MB for 50k positions
        assert!(estimated_memory < 100 * 1024 * 1024,
               "Memory usage too high: {} bytes", estimated_memory);
    }

    fn bench_position_lookup(c: &mut Criterion) {
        let book = create_test_opening_book(10000);
        let test_position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        
        c.bench_function("position_lookup", |b| {
            b.iter(|| book.lookup_position(test_position))
        });
    }

    criterion_group!(benches, bench_position_lookup);
    criterion_main!(benches);
}
```

### 3. Validation Tests

#### 3.1 Chess Logic Validation Tests
```rust
#[cfg(test)]
mod chess_logic_tests {
    use super::*;

    #[test]
    fn test_position_validity() {
        let validator = PositionValidator::new();
        
        let valid_positions = vec![
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0",
        ];

        for position in valid_positions {
            let result = validator.validate_position(position);
            assert!(result.is_ok(), "Valid position rejected: {}", position);
        }
    }

    #[test]
    fn test_invalid_position_detection() {
        let validator = PositionValidator::new();
        
        let invalid_positions = vec![
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1",  // Missing king
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b P20 1",  // Too many pawns
            "invalid/board/format",  // Malformed board
        ];

        for position in invalid_positions {
            let result = validator.validate_position(position);
            assert!(result.is_err(), "Invalid position accepted: {}", position);
        }
    }

    #[test]
    fn test_move_legality() {
        let validator = MoveLegalityValidator::new();
        let position = SfenParser::parse_position_line(
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
        ).unwrap();

        // Test legal opening moves
        let legal_moves = vec!["7g7f", "2g2f", "P*5f"];  // Pawn push, pawn push, pawn drop
        for move_notation in legal_moves {
            let result = validator.validate_move_in_position(&position, move_notation);
            assert!(result.is_ok(), "Legal move rejected: {}", move_notation);
        }

        // Test illegal moves
        let illegal_moves = vec!["1a1b", "9a9b", "K*5e"];  // Invalid squares, king drop
        for move_notation in illegal_moves {
            let result = validator.validate_move_in_position(&position, move_notation);
            assert!(result.is_err(), "Illegal move accepted: {}", move_notation);
        }
    }
}
```

#### 3.2 Data Integrity Tests
```rust
#[cfg(test)]
mod data_integrity_tests {
    use super::*;

    #[test]
    fn test_evaluation_preservation() {
        let original_entries = create_test_entries_with_evaluations();
        
        for entry in original_entries {
            let compact = convert_entry_to_compact(&entry).unwrap();
            let reconstructed = convert_compact_to_entry(&compact).unwrap();
            
            // Check evaluations are preserved within tolerance
            for (orig_move, recon_move) in entry.moves.iter().zip(reconstructed.moves.iter()) {
                let eval_diff = (orig_move.evaluation - recon_move.evaluation).abs();
                assert!(eval_diff <= 1, 
                       "Evaluation not preserved: {} vs {} (diff: {})",
                       orig_move.evaluation, recon_move.evaluation, eval_diff);
            }
        }
    }

    #[test]
    fn test_best_move_preservation() {
        let original_entries = create_test_entries_varied_moves();
        
        for entry in original_entries {
            let original_best = entry.moves.iter()
                .max_by_key(|m| m.evaluation)
                .unwrap();
            
            let compact = convert_entry_to_compact(&entry).unwrap();
            let best_move_decoded = MoveEncoder::decode_move(compact.best_move).unwrap();
            
            assert_eq!(original_best.move_notation, best_move_decoded,
                      "Best move not preserved: {} vs {}", 
                      original_best.move_notation, best_move_decoded);
        }
    }

    #[test]
    fn test_no_data_loss() {
        let original_file = create_comprehensive_test_file();
        let temp_dir = TempDir::new().unwrap();
        let binary_file = temp_dir.path().join("test.bin");
        let index_file = temp_dir.path().join("test.idx");

        // Convert to binary
        convert_opening_book(
            &original_file,
            binary_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::no_filtering()
        ).unwrap();

        // Load binary and verify all positions can be found
        let book = OpeningBook::load(
            binary_file.to_str().unwrap(),
            index_file.to_str().unwrap()
        ).unwrap();

        let original_entries = parse_sfen_file(&original_file).unwrap();
        for entry in original_entries {
            let hash = PositionHasher::hash_sfen_entry(&entry);
            let found = book.lookup_by_hash(hash);
            assert!(found.is_some(), "Position not found in converted book");
        }
    }
}
```

### 4. Stress Tests

#### 4.1 Large Data Tests
```rust
#[cfg(test)]
mod stress_tests {
    use super::*;

    #[test]
    #[ignore]  // Long-running test
    fn test_large_file_conversion() {
        let temp_dir = TempDir::new().unwrap();
        let large_file = temp_dir.path().join("large_test.sfen");
        
        // Generate large test file (100k positions)
        generate_large_test_file(&large_file, 100_000);
        
        let binary_file = temp_dir.path().join("large_test.bin");
        let index_file = temp_dir.path().join("large_test.idx");

        let start_time = Instant::now();
        let result = convert_opening_book(
            large_file.to_str().unwrap(),
            binary_file.to_str().unwrap(),
            index_file.to_str().unwrap(),
            ConversionOptions::default()
        );
        let conversion_time = start_time.elapsed();

        assert!(result.is_ok(), "Large file conversion failed");
        
        // Should complete within reasonable time (adjust based on hardware)
        assert!(conversion_time.as_secs() < 300, 
               "Conversion too slow: {} seconds", conversion_time.as_secs());

        // Verify compression ratio
        let original_size = fs::metadata(&large_file).unwrap().len();
        let binary_size = fs::metadata(&binary_file).unwrap().len();
        let compression_ratio = binary_size as f64 / original_size as f64;
        
        assert!(compression_ratio < 0.3, 
               "Insufficient compression: {:.2}%", compression_ratio * 100.0);
    }

    #[test]
    fn test_memory_stress() {
        // Test with many concurrent operations
        let book = create_test_opening_book(10000);
        let test_positions = create_test_search_positions(10000);
        
        // Concurrent lookups
        use rayon::prelude::*;
        let results: Vec<_> = test_positions.par_iter()
            .map(|pos| book.lookup_position(&pos.sfen))
            .collect();
        
        // Verify all lookups completed
        assert_eq!(results.len(), test_positions.len());
    }

    #[test]
    fn test_hash_collision_stress() {
        // Generate many similar positions to stress-test hash function
        let positions = generate_similar_positions(100_000);
        let mut hashes = std::collections::HashSet::new();
        let mut collisions = 0;

        for position in positions {
            let hash = PositionHasher::hash_sfen_entry(&position);
            if hashes.contains(&hash) {
                collisions += 1;
            }
            hashes.insert(hash);
        }

        // Should have very few collisions even with similar positions
        let collision_rate = collisions as f64 / hashes.len() as f64;
        assert!(collision_rate < 0.001, 
               "Hash collision rate too high: {:.4}%", collision_rate * 100.0);
    }
}
```

### 5. Regression Tests

#### 5.1 Format Compatibility Tests
```rust
#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn test_yaneuraou_format_compatibility() {
        // Test with actual YaneuraOu format samples
        let yaneuraou_samples = vec![
            "#YANEURAOU-DB2016 1.00",
            "sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0",
            "8i7g none 194 3 0",
            "3d3c+ none -203 0 0",
            "7h7g none -325 0 0",
            "",
        ];

        let parser = SfenParser::new();
        let mut current_entry: Option<RawSfenEntry> = None;

        for line in yaneuraou_samples {
            match parser.parse_line(line) {
                Ok(Some(entry)) => {
                    current_entry = Some(entry);
                }
                Ok(None) => {
                    // Continue processing
                }
                Err(e) => {
                    panic!("Failed to parse YaneuraOu format line '{}': {}", line, e);
                }
            }
        }

        assert!(current_entry.is_some(), "No entry was parsed from YaneuraOu format");
        let entry = current_entry.unwrap();
        assert!(!entry.moves.is_empty(), "No moves parsed from YaneuraOu format");
    }

    #[test]
    fn test_backward_compatibility() {
        // Ensure new versions can read old format
        let old_format_data = load_old_format_test_data();
        let temp_dir = TempDir::new().unwrap();
        let old_file = temp_dir.path().join("old_format.sfen");
        fs::write(&old_file, old_format_data).unwrap();

        let result = parse_sfen_file(&old_file);
        assert!(result.is_ok(), "Failed to parse old format data");
    }
}
```

## Test Data Generation

### Helper Functions for Test Data Creation
```rust
mod test_data_generation {
    use super::*;

    pub fn create_test_sfen_data() -> String {
        format!(
            "{}\n{}\n{}\n{}\n{}\n\n{}\n{}\n{}\n\n",
            "#YANEURAOU-DB2016 1.00",
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "7g7f none 50 2 1000",
            "2g2f none 48 2 950",
            "P*5f none 20 1 500",
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
            "3c3d none 45 2 800",
            "8c8d none 42 2 750"
        )
    }

    pub fn create_comprehensive_test_data() -> String {
        let mut data = String::new();
        data.push_str("#YANEURAOU-DB2016 1.00\n");

        // Generate positions with various characteristics
        for i in 0..100 {
            let position = generate_test_position(i);
            let moves = generate_test_moves_for_position(&position, 5);
            
            data.push_str(&format!("sfen {} b - {}\n", position, i));
            for move_data in moves {
                data.push_str(&format!("{} none {} {} {}\n", 
                    move_data.move_notation, 
                    move_data.evaluation, 
                    move_data.depth, 
                    move_data.nodes));
            }
            data.push('\n');
        }

        data
    }

    pub fn generate_test_position(seed: u32) -> String {
        // Generate varied but valid positions based on seed
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed as u64);
        
        // Start with initial position and make random valid moves
        let mut position = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string();
        
        // Apply 0-20 random moves
        let num_moves = rng.gen_range(0..=20);
        for _ in 0..num_moves {
            position = apply_random_move(position, &mut rng);
        }
        
        position
    }

    pub fn generate_test_moves_for_position(position: &str, count: usize) -> Vec<RawMove> {
        let mut moves = Vec::new();
        let legal_moves = generate_legal_moves_for_position(position);
        
        for (i, move_notation) in legal_moves.iter().take(count).enumerate() {
            moves.push(RawMove {
                move_notation: move_notation.clone(),
                move_type: "none".to_string(),
                evaluation: 100 - (i as i32 * 20),  // Decreasing evaluations
                depth: 2,
                nodes: 1000,
            });
        }
        
        moves
    }

    fn generate_legal_moves_for_position(position: &str) -> Vec<String> {
        // Simplified legal move generation for testing
        // In real implementation, this would use proper shogi rules
        vec![
            "7g7f".to_string(),
            "2g2f".to_string(),
            "P*5f".to_string(),
            "8g8f".to_string(),
            "3g3f".to_string(),
        ]
    }
}
```

## Test Execution Strategy

### Automated Test Pipeline
```bash
#!/bin/bash
# run_comprehensive_tests.sh

set -e

echo "Running comprehensive opening book conversion tests..."

# Unit tests
echo "1. Running unit tests..."
cargo test --lib

# Integration tests  
echo "2. Running integration tests..."
cargo test --test integration_tests

# Performance tests
echo "3. Running performance tests..."
cargo test --release --test performance_tests

# Validation tests with real samples
echo "4. Running validation tests with samples..."
cargo run --bin extract_sample -- \
    --input user_book1.db \
    --output test_sample.sfen \
    --count 1000 \
    --strategy diverse

cargo test --test validation_tests -- --test-threads=1

# Stress tests (optional)
if [ "$RUN_STRESS_TESTS" = "true" ]; then
    echo "5. Running stress tests..."
    cargo test --release --test stress_tests -- --ignored
fi

echo "All tests completed successfully!"
```

This comprehensive test suite ensures that the opening book conversion maintains perfect data integrity while achieving the required performance targets.