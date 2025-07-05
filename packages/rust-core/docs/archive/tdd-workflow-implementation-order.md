# TDD Workflow and Implementation Order

## Overview

This document defines the exact Test-Driven Development (TDD) workflow and implementation order for the opening book conversion system. Following this guide ensures that each component is thoroughly tested and works correctly before moving to dependent components.

## Implementation Dependency Graph

```
┌─────────────────┐
│ Data Structures │ ← Start here (no dependencies)
└─────────┬───────┘
          │
          ▼
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│   SFEN Parser   │    │  Move Encoder   │    │Position Hasher  │
└─────────┬───────┘    └─────────┬───────┘    └─────────┬───────┘
          │                      │                      │
          │              ┌───────┴────────┬─────────────┘
          │              │                │
          ▼              ▼                ▼
          ┌─────────────────────────────────┐
          │       Position Filter          │
          └─────────────┬───────────────────┘
                        │
                        ▼
          ┌─────────────────────────────────┐
          │      Binary Converter          │
          └─────────────┬───────────────────┘
                        │
                        ▼
          ┌─────────────────────────────────┐
          │     Integration Tests          │
          └─────────────────────────────────┘
```

## Implementation Sessions

### Session 1: Data Structures (30 minutes)

**Goal**: Define and test all basic data structures

**Files to create**:
- `src/opening_book/data_structures.rs`
- `tests/data_structures_test.rs`

**Workflow**:
```bash
# 1. Red: Create failing tests
touch tests/data_structures_test.rs
# Copy test cases from step-by-step-implementation-with-tests.md
cargo test data_structures_tests # Should fail - no implementation yet

# 2. Green: Minimal implementation
mkdir -p src/opening_book
touch src/opening_book/data_structures.rs
touch src/opening_book/mod.rs
# Implement structs to make tests pass
cargo test data_structures_tests # Should pass

# 3. Refactor: Improve structure
cargo clippy
cargo fmt
cargo test # Ensure still passing
```

**Success Criteria**:
- [ ] All data structure tests pass
- [ ] `CompactPosition` is exactly 16 bytes
- [ ] `CompactMove` is exactly 6 bytes  
- [ ] All structs have proper `#[repr(C)]` for binary compatibility

### Session 2: SFEN Parser (45 minutes)

**Goal**: Parse YaneuraOu SFEN format correctly

**Files to create**:
- `src/opening_book/sfen_parser.rs`
- `tests/sfen_parser_test.rs`

**Dependencies**: Data Structures (Session 1)

**Workflow**:
```bash
# 1. Red: Create failing tests
touch tests/sfen_parser_test.rs
# Copy SFEN parser tests
cargo test sfen_parser_tests # Should fail

# 2. Green: Implement parser step by step
touch src/opening_book/sfen_parser.rs
# Add to mod.rs: pub mod sfen_parser;

# Implement in order:
# 2.1: SfenParser::new() - basic structure
# 2.2: parse_line() for header lines (return None)
cargo test test_parse_header_line # Should pass

# 2.3: parse_line() for position lines
cargo test test_parse_position_line # Should pass

# 2.4: parse_line() for move lines  
cargo test test_parse_move_line # Should pass

# 2.5: Complete entry on empty line
cargo test test_complete_entry_on_empty_line # Should pass

# 2.6: Handle complex positions
cargo test test_parse_complex_position # Should pass

# 2.7: Multiple moves per position
cargo test test_parse_multiple_moves # Should pass

# 2.8: Error handling
cargo test test_invalid_position_line # Should pass
cargo test test_invalid_move_line # Should pass

# 3. Refactor
cargo clippy
cargo fmt
cargo test sfen_parser_tests # All should pass
```

**Success Criteria**:
- [ ] All SFEN parser tests pass
- [ ] Can parse header, position, and move lines correctly
- [ ] Handles malformed input gracefully
- [ ] Completes entries on empty lines
- [ ] No panics on invalid input

### Session 3: Move Encoder (45 minutes)

**Goal**: Encode/decode moves to 16-bit integers

**Files to create**:
- `src/opening_book/move_encoder.rs`
- `tests/move_encoder_test.rs`

**Dependencies**: Data Structures (Session 1)

**Workflow**:
```bash
# 1. Red: Create failing tests
touch tests/move_encoder_test.rs
cargo test move_encoder_tests # Should fail

# 2. Green: Implement encoder step by step
touch src/opening_book/move_encoder.rs

# 2.1: Basic normal moves (no promotion, no drops)
cargo test test_encode_normal_moves # Should pass for basic cases

# 2.2: Add decode functionality  
# Ensure round-trip works for normal moves

# 2.3: Add promotion support
cargo test test_encode_promotion_moves # Should pass

# 2.4: Add drop move support
cargo test test_encode_drop_moves # Should pass

# 2.5: Test uniqueness
cargo test test_move_encoding_uniqueness # Should pass

# 2.6: Test all squares
cargo test test_encode_all_squares # Should pass

# 2.7: Error handling
cargo test test_invalid_move_encoding # Should pass
cargo test test_decode_invalid_encoding # Should pass

# 3. Refactor: Optimize bit layout
cargo clippy
cargo fmt
cargo test move_encoder_tests # All should pass
```

**Success Criteria**:
- [ ] All move encoder tests pass
- [ ] Perfect round-trip encoding/decoding
- [ ] Supports normal moves, promotions, and drops
- [ ] All encodings are unique
- [ ] Handles all 81 squares correctly
- [ ] Graceful error handling for invalid input

### Session 4: Position Hasher (30 minutes)

**Goal**: Generate collision-resistant position hashes

**Files to create**:
- `src/opening_book/position_hasher.rs`
- `tests/position_hasher_test.rs`

**Dependencies**: Data Structures (Session 1)

**Workflow**:
```bash
# 1. Red: Create failing tests
touch tests/position_hasher_test.rs
cargo test position_hasher_tests # Should fail

# 2. Green: Implement hasher
touch src/opening_book/position_hasher.rs

# 2.1: Basic hash_position function
cargo test test_hash_consistency # Should pass

# 2.2: Sensitivity to position changes
cargo test test_hash_sensitivity_to_position # Should pass

# 2.3: Sensitivity to turn
cargo test test_hash_sensitivity_to_turn # Should pass

# 2.4: Sensitivity to hand
cargo test test_hash_sensitivity_to_hand # Should pass

# 2.5: Entry wrapper function
cargo test test_hash_sfen_entry # Should pass

# 2.6: Distribution test
cargo test test_hash_distribution # Should pass

# 2.7: Performance test
cargo test test_hash_performance # Should pass

# 3. Refactor: Consider using a proper hash crate (e.g., xxhash-rust)
cargo clippy
cargo fmt
cargo test position_hasher_tests # All should pass
```

**Success Criteria**:
- [ ] All position hasher tests pass
- [ ] Consistent hashes for identical positions
- [ ] Different hashes for different positions/turn/hand
- [ ] No collisions in test set
- [ ] Performance: 10k hashes in < 100ms

### Session 5: Position Filter (30 minutes)

**Goal**: Filter positions based on quality criteria

**Files to create**:
- `src/opening_book/position_filter.rs`
- `tests/position_filter_test.rs`

**Dependencies**: Data Structures (Session 1), SFEN Parser (Session 2)

**Workflow**:
```bash
# 1. Red: Create failing tests
touch tests/position_filter_test.rs
cargo test position_filter_tests # Should fail

# 2. Green: Implement filter
touch src/opening_book/position_filter.rs

# 2.1: Basic structure and defaults
cargo test test_default_filter_creation # Should pass

# 2.2: Move count filtering
cargo test test_filter_by_move_count # Should pass

# 2.3: Depth filtering (at least one move meets criteria)
cargo test test_filter_by_depth # Should pass

# 2.4: Evaluation filtering
cargo test test_filter_by_evaluation # Should pass

# 2.5: Move filtering (keep top N moves)
cargo test test_filter_moves # Should pass

# 2.6: Empty moves handling
cargo test test_empty_moves_filtered # Should pass

# 2.7: Combined criteria
cargo test test_combined_filtering_criteria # Should pass

# 3. Refactor
cargo clippy
cargo fmt
cargo test position_filter_tests # All should pass
```

**Success Criteria**:
- [ ] All position filter tests pass
- [ ] Filters by move count, depth, and evaluation
- [ ] Keeps only top N moves per position
- [ ] Rejects positions with no valid moves
- [ ] Combines multiple criteria correctly

### Session 6: Binary Converter (60 minutes)

**Goal**: Convert SFEN entries to compact binary format

**Files to create**:
- `src/opening_book/binary_converter.rs`
- `tests/binary_converter_test.rs`

**Dependencies**: All previous sessions

**Workflow**:
```bash
# 1. Red: Create failing tests
touch tests/binary_converter_test.rs
cargo test binary_converter_tests # Should fail

# 2. Green: Implement converter step by step
touch src/opening_book/binary_converter.rs

# 2.1: Basic structure with file creation
cargo test test_convert_single_entry # Should pass

# 2.2: Multiple entries support
cargo test test_convert_multiple_entries # Should pass

# 2.3: Compact position conversion
cargo test test_compact_position_conversion # Should pass

# 2.4: Hash uniqueness verification
cargo test test_position_hash_uniqueness # Should pass

# 2.5: File format compliance
cargo test test_file_format_compliance # Should pass

# 2.6: Error handling
cargo test test_error_handling # Should pass

# 3. Refactor: Optimize file I/O
cargo clippy
cargo fmt
cargo test binary_converter_tests # All should pass
```

**Success Criteria**:
- [ ] All binary converter tests pass
- [ ] Creates valid binary and index files
- [ ] Correct compact format conversion
- [ ] Unique position hashes
- [ ] Proper error handling

### Session 7: Integration Tests (45 minutes)

**Goal**: Test complete conversion pipeline

**Files to create**:
- `src/bin/convert_opening_book.rs`
- `src/opening_book/opening_book.rs` (for loading/searching)
- `tests/integration_test.rs`

**Dependencies**: All previous sessions

**Workflow**:
```bash
# 1. Red: Create failing tests
touch tests/integration_test.rs
cargo test integration_tests # Should fail

# 2. Green: Implement integration
touch src/bin/convert_opening_book.rs
touch src/opening_book/opening_book.rs

# 2.1: Main conversion function
cargo test test_complete_conversion_pipeline # Should pass

# 2.2: Round-trip conversion
cargo test test_round_trip_conversion # Should pass

# 2.3: Performance targets
cargo test test_performance_targets # Should pass

# 2.4: Compression ratio
cargo test test_compression_ratio # Should pass

# 2.5: Error recovery
cargo test test_error_recovery # Should pass

# 3. Refactor: Optimize end-to-end performance
cargo clippy
cargo fmt
cargo test integration_tests # All should pass
cargo test # All tests should pass
```

**Success Criteria**:
- [ ] All integration tests pass
- [ ] Complete conversion pipeline works
- [ ] Round-trip accuracy maintained
- [ ] Performance targets met (< 60s for 10k positions, < 1ms search)
- [ ] Good compression ratio (< 50% of original size)
- [ ] Graceful error handling

## Daily Implementation Schedule

### Day 1: Core Components
- **Morning (2 hours)**: Sessions 1-2 (Data Structures + SFEN Parser)
- **Afternoon (2 hours)**: Sessions 3-4 (Move Encoder + Position Hasher)

### Day 2: Processing and Integration  
- **Morning (1 hour)**: Session 5 (Position Filter)
- **Afternoon (2.5 hours)**: Sessions 6-7 (Binary Converter + Integration)

## TDD Principles to Follow

### Red-Green-Refactor Cycle

1. **Red Phase**: Write a failing test
   - Test should compile but fail
   - Test should test ONE specific behavior
   - Test should have clear expected outcome

2. **Green Phase**: Write minimal code to pass
   - Focus only on making the test pass
   - Don't worry about code quality yet
   - Don't implement features not tested

3. **Refactor Phase**: Improve code quality
   - Keep tests passing
   - Improve readability and structure
   - Remove duplication
   - Optimize performance if needed

### Test Quality Guidelines

```rust
// ✅ Good test: Specific, clear, focused
#[test]
fn test_encode_pawn_move_7g7f() {
    let encoded = MoveEncoder::encode_move("7g7f").unwrap();
    let decoded = MoveEncoder::decode_move(encoded).unwrap();
    assert_eq!("7g7f", decoded);
}

// ❌ Bad test: Testing multiple things, unclear
#[test]
fn test_move_encoder() {
    let encoder = MoveEncoder::new();
    assert!(encoder.encode_move("7g7f").is_ok());
    assert!(encoder.encode_move("P*5f").is_ok());
    assert!(encoder.encode_move("invalid").is_err());
    // Too many responsibilities in one test
}
```

### Error Handling Strategy

Every component should handle errors gracefully:

```rust
// Use Result<T, Error> for all fallible operations
pub fn encode_move(move_notation: &str) -> Result<u16, MoveEncodingError> {
    // Implementation
}

// Provide specific error types
#[derive(Debug, Clone)]
pub enum MoveEncodingError {
    InvalidFormat(String),
    InvalidSquare(String),
    InvalidPiece(String),
}
```

## Debugging and Troubleshooting

### When Tests Fail

1. **Read the test failure message carefully**
2. **Run a single test**: `cargo test test_name`
3. **Add debug prints if needed**:
   ```rust
   #[test]
   fn test_something() {
       let result = function_under_test();
       println!("Debug: result = {:?}", result); // Temporary debug
       assert_eq!(expected, result);
   }
   ```
4. **Use `cargo test -- --nocapture` to see debug output**

### Performance Issues

1. **Use `cargo test --release` for performance tests**
2. **Profile with `cargo bench` if available**
3. **Check for unnecessary allocations**
4. **Consider using `&str` instead of `String` where possible**

### Memory Issues

1. **Run tests with Valgrind on Linux**: `cargo test --target x86_64-unknown-linux-gnu`
2. **Check for proper cleanup in `Drop` implementations**
3. **Use `Rc<RefCell<>>` sparingly, prefer owned data**

## Code Review Checklist

Before moving to next session:

- [ ] All tests for current session pass
- [ ] Code follows Rust conventions (`cargo clippy` clean)
- [ ] Code is formatted (`cargo fmt`)
- [ ] Error handling is comprehensive
- [ ] Documentation comments for public APIs
- [ ] No `unwrap()` or `expect()` in production code paths
- [ ] Performance requirements met
- [ ] Memory usage is reasonable

## Integration with Main Project

### After All Sessions Complete:

1. **Add to main Cargo.toml**:
   ```toml
   [[bin]]
   name = "convert_opening_book"
   path = "src/bin/convert_opening_book.rs"
   ```

2. **Export public APIs in lib.rs**:
   ```rust
   pub mod opening_book;
   pub use opening_book::*;
   ```

3. **Run full test suite**:
   ```bash
   cargo test --all
   cargo clippy --all
   cargo fmt --all
   ```

4. **Test with actual user_book1.db sample**:
   ```bash
   cargo run --bin extract_sample -- --input user_book1.db --output test_sample.sfen --count 100
   cargo run --bin convert_opening_book -- --input test_sample.sfen --output test.bin --index test.idx
   ```

This step-by-step approach ensures that each component is solid before building upon it, resulting in a robust and reliable opening book conversion system.