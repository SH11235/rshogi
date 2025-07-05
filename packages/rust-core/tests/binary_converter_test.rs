#[cfg(test)]
mod binary_converter_tests {
    use shogi_core::opening_book::*;
    use std::io::Cursor;

    fn create_test_entries() -> Vec<RawSfenEntry> {
        vec![
            RawSfenEntry {
                position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL".to_string(),
                turn: 'b',
                hand: "-".to_string(),
                move_count: 1,
                moves: vec![
                    RawMove {
                        move_notation: "7g7f".to_string(),
                        move_type: "none".to_string(),
                        evaluation: 50,
                        depth: 10,
                        nodes: 10000,
                    },
                    RawMove {
                        move_notation: "2g2f".to_string(),
                        move_type: "none".to_string(),
                        evaluation: 40,
                        depth: 8,
                        nodes: 8000,
                    },
                ],
            },
            RawSfenEntry {
                position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL".to_string(),
                turn: 'w',
                hand: "-".to_string(),
                move_count: 2,
                moves: vec![
                    RawMove {
                        move_notation: "3c3d".to_string(),
                        move_type: "none".to_string(),
                        evaluation: -45,
                        depth: 9,
                        nodes: 9000,
                    },
                ],
            },
        ]
    }

    #[test]
    fn test_convert_single_entry() {
        let converter = BinaryConverter::new();
        let entry = create_test_entries().into_iter().next().unwrap();
        
        let result = converter.convert_entry(&entry);
        assert!(result.is_ok());
        
        let binary_entry = result.unwrap();
        assert_ne!(binary_entry.header.position_hash, 0);
        assert_eq!(binary_entry.header.move_count, 2);
        assert_eq!(binary_entry.moves.len(), 2);
    }

    #[test]
    fn test_encode_decode_position_header() {
        let header = CompactPosition {
            position_hash: 0x123456789ABCDEF0,
            best_move: 0x1234,
            evaluation: 50,
            depth: 10,
            move_count: 2,
            popularity: 1,
            reserved: 0,
        };
        
        let encoded = BinaryConverter::encode_position_header(&header);
        assert_eq!(encoded.len(), 16); // Should be exactly 16 bytes
        
        let decoded = BinaryConverter::decode_position_header(&encoded).unwrap();
        assert_eq!(decoded.position_hash, header.position_hash);
        assert_eq!(decoded.best_move, header.best_move);
        assert_eq!(decoded.evaluation, header.evaluation);
        assert_eq!(decoded.depth, header.depth);
        assert_eq!(decoded.move_count, header.move_count);
        assert_eq!(decoded.popularity, header.popularity);
    }

    #[test]
    fn test_encode_decode_move() {
        let move_data = CompactMove {
            move_encoded: 0x1234,
            evaluation: -100,
            depth: 15,
            reserved: 0,
        };
        
        let encoded = BinaryConverter::encode_move(&move_data);
        assert_eq!(encoded.len(), 6); // Should be exactly 6 bytes
        
        let decoded = BinaryConverter::decode_move(&encoded).unwrap();
        assert_eq!(decoded.move_encoded, move_data.move_encoded);
        assert_eq!(decoded.evaluation, move_data.evaluation);
        assert_eq!(decoded.depth, move_data.depth);
    }

    #[test]
    fn test_write_read_binary_file() {
        let converter = BinaryConverter::new();
        let entries = create_test_entries();
        
        // Write to memory buffer
        let mut buffer = Vec::new();
        let stats = converter.write_binary(&entries, &mut buffer).unwrap();
        
        assert!(stats.positions_written > 0);
        assert!(stats.bytes_written > 0);
        assert_eq!(stats.positions_written, 2);
        
        // Read back from buffer
        let mut cursor = Cursor::new(buffer);
        let read_entries = converter.read_binary(&mut cursor).unwrap();
        
        assert_eq!(read_entries.len(), 2);
    }

    #[test]
    fn test_file_header() {
        let converter = BinaryConverter::new();
        let header = BinaryFileHeader {
            magic: *b"SFEN",
            version: 1,
            position_count: 100,
            checksum: 0x12345678,
        };
        
        let encoded = converter.encode_file_header(&header);
        assert_eq!(encoded.len(), 16); // Fixed header size
        
        let decoded = converter.decode_file_header(&encoded).unwrap();
        assert_eq!(&decoded.magic, b"SFEN");
        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.position_count, 100);
        assert_eq!(decoded.checksum, 0x12345678);
    }

    #[test]
    fn test_convert_with_filter() {
        let converter = BinaryConverter::new();
        let filter = PositionFilter {
            max_moves: 10,
            min_depth: 5,
            min_evaluation: -200,
            max_evaluation: 200,
        };
        
        let mut entries = create_test_entries();
        let filtered = converter.filter_and_convert(&mut entries, &filter).unwrap();
        
        // Should filter based on criteria
        assert!(filtered.len() <= entries.len());
        
        // Check that filtered entries meet criteria
        for entry in &filtered {
            assert!(entry.header.depth >= filter.min_depth as u8);
            assert!(entry.header.evaluation >= filter.min_evaluation as i16);
            assert!(entry.header.evaluation <= filter.max_evaluation as i16);
        }
    }

    #[test]
    fn test_compression() {
        let converter = BinaryConverter::new();
        // Create more entries for better compression ratio
        let mut entries = Vec::new();
        for i in 0..10 {
            let mut entry = create_test_entries()[0].clone();
            entry.move_count = i + 1;
            entries.push(entry);
        }
        
        // Convert to binary
        let mut uncompressed = Vec::new();
        converter.write_binary(&entries, &mut uncompressed).unwrap();
        
        // Compress - with small data, compressed might be larger due to gzip headers
        let compressed = converter.compress_data(&uncompressed).unwrap();
        
        // For small data, just verify compression/decompression works correctly
        let decompressed = converter.decompress_data(&compressed).unwrap();
        assert_eq!(decompressed, uncompressed);
        
        // For larger data (>1KB), compression should reduce size
        if uncompressed.len() > 1024 {
            assert!(compressed.len() < uncompressed.len());
        }
    }

    #[test]
    fn test_statistics() {
        let converter = BinaryConverter::new();
        let entries = create_test_entries();
        
        let mut buffer = Vec::new();
        let stats = converter.write_binary(&entries, &mut buffer).unwrap();
        
        assert_eq!(stats.positions_written, 2);
        assert_eq!(stats.total_moves, 3); // 2 + 1
        assert!(stats.bytes_written > 0);
        assert!(stats.compression_ratio > 0.0);
    }

    #[test]
    fn test_checksum_validation() {
        let converter = BinaryConverter::new();
        let entries = create_test_entries();
        
        // Write with checksum
        let mut buffer = Vec::new();
        converter.write_binary(&entries, &mut buffer).unwrap();
        
        // Verify checksum on read
        let mut cursor = Cursor::new(&buffer);
        let result = converter.read_binary(&mut cursor);
        assert!(result.is_ok()); // Should validate checksum successfully
        
        // Corrupt data
        if buffer.len() > 20 {
            buffer[20] ^= 0xFF; // Flip some bits
        }
        
        // Should fail checksum validation
        let mut cursor = Cursor::new(&buffer);
        let result = converter.read_binary(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_incremental_conversion() {
        let converter = BinaryConverter::new();
        let entries = create_test_entries();
        
        // Test chunk-based processing
        let chunk_size = 1;
        let mut all_converted = Vec::new();
        
        for chunk in entries.chunks(chunk_size) {
            let converted = converter.convert_entries(chunk).unwrap();
            all_converted.extend(converted);
        }
        
        assert_eq!(all_converted.len(), entries.len());
    }
}