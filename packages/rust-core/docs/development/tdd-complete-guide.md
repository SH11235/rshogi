# TDD Complete Guide for Opening Book System

This comprehensive guide consolidates all TDD practices, workflows, and test cases for the opening book conversion system.

## Table of Contents

1. [TDD Principles and Workflow](#tdd-principles-and-workflow)
2. [Implementation Order](#implementation-order)
3. [Component Test Cases](#component-test-cases)
4. [Incremental Validation Process](#incremental-validation-process)
5. [Best Practices](#best-practices)

## TDD Principles and Workflow

### Core TDD Cycle

1. **Red**: Write a failing test first
2. **Green**: Write minimal code to make the test pass
3. **Refactor**: Improve code quality while keeping tests green

### Workflow for Each Component

```
1. Analyze requirements
2. Design public API
3. Write unit tests for API
4. Implement minimal functionality
5. Add edge case tests
6. Refactor for clarity and performance
7. Add integration tests
8. Document public API
```

## Implementation Order

### Phase 1: Core Data Structures (Session 1-2)
**Goal**: Define fundamental types for the opening book system

1. **SFEN Format Types**
   - `RawSfenEntry`: Raw parsed data structure
   - `RawMove`: Individual move data
   - Test: Parse sample SFEN lines

2. **Binary Format Types**
   - `CompactPosition`: 16-byte position header
   - `CompactMove`: 6-byte move data
   - `BinaryEntry`: Combined position and moves
   - Test: Encode/decode roundtrip

### Phase 2: Parsing (Session 3-4)
**Goal**: Parse YaneuraOu SFEN format correctly

1. **SFEN Parser**
   - Parse position lines with metadata
   - Parse move lines with evaluations
   - Handle multi-line positions
   - Handle edge cases (empty lines, comments)
   - Test: Parse complete sample file

### Phase 3: Encoding/Hashing (Session 5)
**Goal**: Efficient binary encoding and position identification

1. **Move Encoder**
   - Encode normal moves (e.g., "7g7f")
   - Encode promotions (e.g., "3d3c+")
   - Encode drops (e.g., "P*5f")
   - Test: All 81x81 square combinations

2. **Position Hasher**
   - Generate Zobrist hash tables
   - Hash board positions deterministically
   - Handle collision statistics
   - Test: Hash uniqueness for different positions

### Phase 4: Filtering and Conversion (Session 6)
**Goal**: Apply quality filters and convert to binary

1. **Position Filter**
   - Filter by move number from initial position
   - Filter by analysis depth
   - Filter by evaluation range
   - Test: Various filter combinations

2. **Binary Converter**
   - Convert entries to binary format
   - Write binary with headers
   - Read binary and reconstruct
   - Test: Large file handling

### Phase 5: Compression and I/O (Session 7)
**Goal**: Optimize file size and performance

1. **Compression**
   - Gzip compression/decompression
   - Streaming for large files
   - Test: Compression ratios

2. **Performance Optimization**
   - Parallel processing with Rayon
   - Memory-efficient chunking
   - Progress reporting
   - Test: Benchmark performance

### Phase 6: Verification Tools (Session 8)
**Goal**: Validate converted data integrity

1. **Verification Tool**
   - Load and validate binary files
   - Compare with original SFEN
   - Export human-readable format
   - Test: End-to-end verification

## Component Test Cases

### SFEN Parser Tests

```rust
#[test]
fn test_parse_basic_position() {
    let mut parser = SfenParser::new();
    let line = "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let result = parser.parse_line(line).unwrap();
    assert!(result.is_some());
}

#[test]
fn test_parse_move_line() {
    let mut parser = SfenParser::new();
    parser.parse_line("sfen ...").unwrap();
    let result = parser.parse_line("7g7f none 50 10 100000").unwrap();
    assert!(result.is_none()); // Move added to current position
}

#[test]
fn test_continuous_positions_without_empty_lines() {
    // Test YaneuraOu format where positions aren't separated
    let mut parser = SfenParser::new();
    let lines = vec![
        "sfen position1...",
        "7g7f none 50 10 100000",
        "sfen position2...",  // Should return position1 here
        "2g2f none 45 10 95000",
    ];
    // Verify correct handling
}
```

### Move Encoder Tests

```rust
#[test]
fn test_encode_all_move_types() {
    // Normal moves
    assert_roundtrip("7g7f");
    // Promotions
    assert_roundtrip("3d3c+");
    // Drops
    assert_roundtrip("P*5f");
}

#[test]
fn test_invalid_moves() {
    assert!(MoveEncoder::encode_move("invalid").is_err());
    assert!(MoveEncoder::encode_move("0g7f").is_err());  // Invalid file
    assert!(MoveEncoder::encode_move("7j7f").is_err());  // Invalid rank
}
```

### Position Hasher Tests

```rust
#[test]
fn test_deterministic_hashing() {
    let pos = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let hash1 = PositionHasher::hash_position(pos).unwrap();
    let hash2 = PositionHasher::hash_position(pos).unwrap();
    assert_eq!(hash1, hash2);
}

#[test]
fn test_different_positions_different_hashes() {
    let initial = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let after_move = "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2";
    
    let hash1 = PositionHasher::hash_position(initial).unwrap();
    let hash2 = PositionHasher::hash_position(after_move).unwrap();
    assert_ne!(hash1, hash2);
}
```

### Binary Format Tests

```rust
#[test]
fn test_binary_header_size() {
    let header = CompactPosition { /* ... */ };
    let encoded = BinaryConverter::encode_position_header(&header);
    assert_eq!(encoded.len(), 16);
}

#[test]
fn test_binary_roundtrip() {
    let entries = vec![/* test data */];
    let mut buffer = Vec::new();
    
    let write_stats = converter.write_binary(&entries, &mut buffer).unwrap();
    let read_entries = converter.read_binary(&mut Cursor::new(buffer)).unwrap();
    
    assert_eq!(entries.len(), read_entries.len());
}
```

### Integration Tests

```rust
#[test]
fn test_end_to_end_conversion() {
    // 1. Parse SFEN data
    let sfen_data = load_test_sfen();
    let entries = parse_sfen(sfen_data);
    
    // 2. Apply filters
    let filter = PositionFilter::new(50, 5, -500, 500);
    let filtered = filter_entries(entries, filter);
    
    // 3. Convert to binary
    let binary = convert_to_binary(filtered);
    
    // 4. Compress
    let compressed = compress_data(binary);
    
    // 5. Verify
    let decompressed = decompress_data(compressed);
    let verified = read_binary(decompressed);
    
    assert_data_integrity(verified);
}
```

## Incremental Validation Process

### Stage 1: Unit Testing
- Test each component in isolation
- Mock dependencies where needed
- Focus on edge cases and error handling

### Stage 2: Integration Testing
- Test component interactions
- Use real data samples
- Verify data flow through pipeline

### Stage 3: Performance Testing
- Benchmark with large files
- Measure memory usage
- Profile CPU usage

### Stage 4: Acceptance Testing
- Full conversion of real opening book
- Verify output with verification tool
- Compare with original data

### Validation Checkpoints

1. **After Parsing**: Verify position count matches expectations
2. **After Filtering**: Check filtered count is reasonable
3. **After Conversion**: Validate binary format structure
4. **After Compression**: Ensure data integrity maintained
5. **Final Output**: Full verification against original

## Best Practices

### 1. Test Organization
```
tests/
├── unit/
│   ├── parser_test.rs
│   ├── encoder_test.rs
│   └── hasher_test.rs
├── integration/
│   └── end_to_end_test.rs
└── fixtures/
    └── sample_data.sfen
```

### 2. Test Data Management
- Use small, focused test fixtures
- Create specific test cases for edge conditions
- Keep real-world samples for integration tests

### 3. Error Testing
- Test all error paths explicitly
- Verify error messages are helpful
- Ensure graceful degradation

### 4. Performance Considerations
- Use `#[bench]` for performance-critical code
- Profile before optimizing
- Maintain baseline benchmarks

### 5. Documentation
- Document why tests exist, not just what they test
- Include examples in API documentation
- Keep test names descriptive

### 6. Continuous Integration
- Run all tests on every commit
- Use `cargo clippy` for linting
- Enforce test coverage thresholds

## Test Execution Strategy

```bash
# Run all tests
cargo test

# Run specific test module
cargo test parser

# Run with verbose output
cargo test -- --nocapture

# Run benchmarks
cargo bench

# Check test coverage
cargo tarpaulin
```

## Troubleshooting Common Issues

### Parser Tests Failing
- Check line ending handling (CRLF vs LF)
- Verify UTF-8 encoding
- Ensure proper state reset between tests

### Hash Collisions
- Verify Zobrist table initialization
- Check for proper modulo operations
- Ensure deterministic random seed

### Binary Format Mismatches
- Verify byte order (endianness)
- Check struct packing/alignment
- Ensure version compatibility

### Performance Degradation
- Profile with `cargo flamegraph`
- Check for unnecessary allocations
- Verify parallel processing efficiency