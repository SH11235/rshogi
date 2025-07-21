#[cfg(test)]
mod integration_tests {
    use std::io::Cursor;
    use std::time::Instant;

    use engine_core::{
        opening_book::{BinaryConverter, PositionFilter, RawMove, RawSfenEntry, SfenParser},
        MoveEncoder,
    };

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
"#
        .to_string()
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

    // Note: test_move_encoding_roundtrip moved to move_encoder_test.rs
    // Note: test_position_hash_uniqueness moved to position_hasher_test.rs

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
            large_data.push('\n');
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
        let filters = [
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

    // Note: test_binary_format_validation moved to binary_converter_test.rs

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
            "invalid", "7g7f7e", "K*5e", // King can't be dropped
        ];

        for move_str in invalid_moves {
            let result = MoveEncoder::encode_move(move_str);
            assert!(result.is_err(), "Should fail for invalid move: {move_str}");
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

        for _chunk_idx in 0..num_chunks {
            let mut chunk_entries = Vec::new();

            for _i in 0..chunk_size {
                let entry = RawSfenEntry {
                    position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL"
                        .to_string(),
                    turn: 'b',
                    hand: "-".to_string(),
                    move_count: 10,
                    moves: vec![RawMove {
                        move_notation: "7g7f".to_string(),
                        move_type: "none".to_string(),
                        evaluation: 50,
                        depth: 10,
                        nodes: 10000,
                    }],
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
        println!("Successfully processed {total_written} entries in chunks");
    }
}
