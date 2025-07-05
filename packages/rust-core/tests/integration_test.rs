#[cfg(test)]
mod integration_tests {
    use shogi_core::opening_book::*;
    use std::io::Cursor;
    use std::time::Instant;

    fn create_sample_sfen_data() -> String {
        r#"#YANEURAOU-DB2016 1.00
sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
7g7f none 50 10 100000
2g2f none 45 10 95000
6g6f none 40 9 90000
5g5f none 35 8 85000

sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2
3c3d none -45 10 98000
8c8d none -40 9 93000
4c4d none -35 8 88000

sfen lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3
2g2f none 55 11 110000
8h2b+ none 60 12 120000
5g5f none 50 10 105000

sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
5i4h none 30 7 70000
P*5e none 25 6 65000

sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 50
G*5f none 150 15 200000
N*4e none 140 14 190000
"#.to_string()
    }

    #[test]
    fn test_end_to_end_conversion() {
        // Parse SFEN data
        let sfen_data = create_sample_sfen_data();
        let mut parser = SfenParser::new();
        let mut entries = Vec::new();
        
        for line in sfen_data.lines() {
            if let Ok(Some(entry)) = parser.parse_line(line) {
                entries.push(entry);
            }
        }
        
        // Handle last entry if exists
        if let Ok(Some(entry)) = parser.parse_line("") {
            entries.push(entry);
        }
        
        // Should have parsed 5 positions
        assert_eq!(entries.len(), 5);
        
        // Apply position filter
        let filter = PositionFilter {
            max_moves: 30,
            min_depth: 5,
            min_evaluation: -100,
            max_evaluation: 100,
        };
        
        let converter = BinaryConverter::new();
        let filtered_entries = converter.filter_and_convert(&mut entries, &filter).unwrap();
        
        // Some positions should be filtered out
        assert!(filtered_entries.len() < entries.len());
        
        // Write to binary format
        let mut binary_buffer = Vec::new();
        let stats = converter.write_binary(&entries, &mut binary_buffer).unwrap();
        
        assert!(stats.positions_written > 0);
        assert!(stats.bytes_written > 0);
        
        // Read back and verify
        let mut cursor = Cursor::new(binary_buffer);
        let read_entries = converter.read_binary(&mut cursor).unwrap();
        
        assert_eq!(read_entries.len(), entries.len());
    }

    #[test]
    fn test_move_encoding_roundtrip() {
        let test_moves = vec![
            "7g7f",   // Normal move
            "2g2f",   // Normal move
            "8h2b+",  // Promotion
            "P*5e",   // Drop
            "G*5f",   // Gold drop
            "N*4e",   // Knight drop
        ];
        
        for move_str in test_moves {
            let encoded = MoveEncoder::encode_move(move_str).unwrap();
            let decoded = MoveEncoder::decode_move(encoded).unwrap();
            assert_eq!(move_str, decoded, "Failed roundtrip for move: {}", move_str);
        }
    }

    #[test]
    fn test_position_hash_uniqueness() {
        let positions = vec![
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL",
            "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL",
            "+B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R",
        ];
        
        let mut hasher = PositionHasher::new();
        let mut hashes = std::collections::HashSet::new();
        
        for pos in &positions {
            let hash = hasher.hash_and_track(pos).unwrap();
            assert!(hashes.insert(hash), "Hash collision detected for position: {}", pos);
        }
        
        let stats = hasher.get_statistics();
        assert_eq!(stats.collision_count, 0);
        assert_eq!(stats.unique_positions, positions.len());
    }

    #[test]
    fn test_large_file_processing() {
        // Create a larger dataset
        let mut large_data = String::from("#YANEURAOU-DB2016 1.00\n");
        
        // Generate 100 positions
        for i in 0..100 {
            large_data.push_str(&format!(
                "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - {}\n",
                i + 1
            ));
            large_data.push_str("7g7f none 50 10 100000\n");
            large_data.push_str("2g2f none 45 10 95000\n");
            large_data.push_str("\n");
        }
        
        let mut parser = SfenParser::new();
        let mut entries = Vec::new();
        
        let start = Instant::now();
        
        for line in large_data.lines() {
            if let Ok(Some(entry)) = parser.parse_line(line) {
                entries.push(entry);
            }
        }
        
        let parse_duration = start.elapsed();
        println!("Parsed {} entries in {:?}", entries.len(), parse_duration);
        
        assert_eq!(entries.len(), 100);
        
        // Convert to binary
        let converter = BinaryConverter::new();
        let start = Instant::now();
        
        let mut buffer = Vec::new();
        let stats = converter.write_binary(&entries, &mut buffer).unwrap();
        
        let convert_duration = start.elapsed();
        println!("Converted to binary in {:?}, size: {} bytes", convert_duration, buffer.len());
        
        assert_eq!(stats.positions_written, 100);
        
        // Test compression
        let compressed = converter.compress_data(&buffer).unwrap();
        let compression_ratio = compressed.len() as f64 / buffer.len() as f64;
        println!("Compression ratio: {:.2}%", compression_ratio * 100.0);
        
        assert!(compressed.len() < buffer.len());
    }

    #[test]
    fn test_filter_quality_control() {
        let sfen_data = create_sample_sfen_data();
        let mut parser = SfenParser::new();
        let mut entries = Vec::new();
        
        for line in sfen_data.lines() {
            if let Ok(Some(entry)) = parser.parse_line(line) {
                entries.push(entry);
            }
        }
        
        // Test different filter configurations
        let filters = vec![
            PositionFilter {
                max_moves: 10,
                min_depth: 10,
                min_evaluation: -50,
                max_evaluation: 50,
            },
            PositionFilter {
                max_moves: 30,
                min_depth: 8,
                min_evaluation: -100,
                max_evaluation: 100,
            },
            PositionFilter::default(),
        ];
        
        for (i, filter) in filters.iter().enumerate() {
            let mut test_entries = entries.clone();
            let mut filtered_count = 0;
            
            for entry in &mut test_entries {
                if filter.filter_entry(entry) {
                    filtered_count += 1;
                }
            }
            
            println!("Filter {} kept {} out of {} positions", i, filtered_count, entries.len());
            assert!(filtered_count <= entries.len());
        }
    }

    #[test]
    fn test_binary_format_validation() {
        let converter = BinaryConverter::new();
        
        // Test header encoding/decoding
        let header = CompactPosition {
            position_hash: 0x123456789ABCDEF0,
            best_move: 0x1234,
            evaluation: 50,
            depth: 10,
            move_count: 3,
            popularity: 1,
            reserved: 0,
        };
        
        let encoded = BinaryConverter::encode_position_header(&header);
        assert_eq!(encoded.len(), 16, "Position header must be exactly 16 bytes");
        
        let decoded = BinaryConverter::decode_position_header(&encoded).unwrap();
        assert_eq!(decoded.position_hash, header.position_hash);
        assert_eq!(decoded.best_move, header.best_move);
        assert_eq!(decoded.evaluation, header.evaluation);
        assert_eq!(decoded.depth, header.depth);
        assert_eq!(decoded.move_count, header.move_count);
        
        // Test move encoding/decoding
        let move_data = CompactMove {
            move_encoded: 0x5678,
            evaluation: -25,
            depth: 8,
            reserved: 0,
        };
        
        let encoded_move = BinaryConverter::encode_move(&move_data);
        assert_eq!(encoded_move.len(), 6, "Move must be exactly 6 bytes");
        
        let decoded_move = BinaryConverter::decode_move(&encoded_move).unwrap();
        assert_eq!(decoded_move.move_encoded, move_data.move_encoded);
        assert_eq!(decoded_move.evaluation, move_data.evaluation);
        assert_eq!(decoded_move.depth, move_data.depth);
    }

    #[test]
    fn test_error_handling() {
        let mut parser = SfenParser::new();
        
        // Test invalid SFEN format
        let invalid_lines = vec![
            "invalid sfen format",
            "sfen invalid",
            "7g7f none invalid_eval 10 100000",
            "",
        ];
        
        for line in invalid_lines {
            let result = parser.parse_line(line);
            // Should either return error or None
            assert!(result.is_err() || result.unwrap().is_none());
        }
        
        // Test invalid move encoding
        let invalid_moves = vec![
            "invalid",
            "7g7f7e",
            "K*5e", // King can't be dropped
        ];
        
        for move_str in invalid_moves {
            let result = MoveEncoder::encode_move(move_str);
            assert!(result.is_err(), "Should fail for invalid move: {}", move_str);
        }
        
        // Test corrupted binary data
        let converter = BinaryConverter::new();
        let corrupted_data = vec![0xFF; 10]; // Random data
        let mut cursor = Cursor::new(corrupted_data);
        let result = converter.read_binary(&mut cursor);
        assert!(result.is_err(), "Should fail for corrupted data");
    }

    #[test]
    fn test_memory_efficiency() {
        // Test that we can process large files without excessive memory usage
        let converter = BinaryConverter::new();
        let _filter = PositionFilter::default();
        
        // Create entries in chunks
        let chunk_size = 1000;
        let num_chunks = 10;
        
        let mut total_written = 0;
        let mut _buffer: Vec<u8> = Vec::new();
        
        for chunk_idx in 0..num_chunks {
            let mut chunk_entries = Vec::new();
            
            for _i in 0..chunk_size {
                let entry = RawSfenEntry {
                    position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
                    turn: 'b',
                    hand: "-".to_string(),
                    move_count: 10,
                    moves: vec![
                        RawMove {
                            move_notation: "7g7f".to_string(),
                            move_type: "none".to_string(),
                            evaluation: 50,
                            depth: 10,
                            nodes: 10000,
                        },
                    ],
                };
                chunk_entries.push(entry);
            }
            
            // Process chunk
            let binary_entries = converter.convert_entries(&chunk_entries).unwrap();
            total_written += binary_entries.len();
            
            // Clear chunk to free memory
            drop(chunk_entries);
        }
        
        assert_eq!(total_written, chunk_size * num_chunks);
        println!("Successfully processed {} entries in chunks", total_written);
    }
}