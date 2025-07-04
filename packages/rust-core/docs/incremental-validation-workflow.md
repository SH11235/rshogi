# Incremental Validation Workflow

## Overview

This document outlines the step-by-step workflow for safely processing the 500MB opening book with comprehensive validation at each stage.

## Workflow Phases

### Phase 1: Initial Setup and Sample Testing

#### 1.1 Environment Setup
```bash
# Create working directories
mkdir -p converted_openings/samples
mkdir -p converted_openings/chunks
mkdir -p converted_openings/checkpoints
mkdir -p test_results

# Build all conversion tools
cargo build --release --bin extract_sample
cargo build --release --bin convert_opening_book
cargo build --release --bin validate_conversion
cargo build --release --bin test_conversion
```

#### 1.2 Extract Test Samples
```bash
# Extract diverse samples of different sizes
echo "Extracting small sample for quick testing..."
cargo run --release --bin extract_sample -- \
    --input user_book1.db \
    --output converted_openings/samples/sample_100.sfen \
    --count 100 \
    --strategy diverse

echo "Extracting medium sample for comprehensive testing..."
cargo run --release --bin extract_sample -- \
    --input user_book1.db \
    --output converted_openings/samples/sample_1000.sfen \
    --count 1000 \
    --strategy diverse

echo "Extracting large sample for performance testing..."
cargo run --release --bin extract_sample -- \
    --input user_book1.db \
    --output converted_openings/samples/sample_10000.sfen \
    --count 10000 \
    --strategy diverse
```

#### 1.3 Convert and Validate Samples
```bash
# Test conversion pipeline with increasing complexity
for sample in sample_100 sample_1000 sample_10000; do
    echo "Processing $sample..."
    
    # Convert to binary
    cargo run --release --bin convert_opening_book -- \
        --input converted_openings/samples/${sample}.sfen \
        --output converted_openings/samples/${sample}.bin \
        --index converted_openings/samples/${sample}.idx \
        --verbose
    
    # Validate conversion
    cargo run --release --bin validate_conversion -- \
        --original converted_openings/samples/${sample}.sfen \
        --converted converted_openings/samples/${sample}.bin \
        --index converted_openings/samples/${sample}.idx \
        --test-mode comprehensive \
        --report test_results/${sample}_validation.json
    
    # Run round-trip tests
    cargo run --release --bin test_conversion -- \
        --input converted_openings/samples/${sample}.bin \
        --index converted_openings/samples/${sample}.idx \
        --output test_results/${sample}_roundtrip.sfen \
        --compare converted_openings/samples/${sample}.sfen
    
    echo "$sample processing complete. Check test_results/ for validation reports."
done
```

### Phase 2: Chunk Processing

#### 2.1 Calculate Optimal Chunk Size
```bash
# Analyze file size and estimate chunks
echo "Analyzing source file..."
wc -l user_book1.db
ls -lh user_book1.db

# Calculate chunk size (aim for ~50,000 positions per chunk)
total_lines=$(wc -l < user_book1.db)
chunk_size=50000
num_chunks=$(( (total_lines + chunk_size - 1) / chunk_size ))

echo "File has $total_lines lines, will process in $num_chunks chunks of $chunk_size positions each"
```

#### 2.2 Process Chunks with Validation
```bash
#!/bin/bash
# chunk_processing.sh

set -e  # Exit on any error

CHUNK_SIZE=50000
TOTAL_LINES=$(wc -l < user_book1.db)
NUM_CHUNKS=$(( (TOTAL_LINES + CHUNK_SIZE - 1) / CHUNK_SIZE ))

echo "Starting chunk processing: $NUM_CHUNKS chunks of $CHUNK_SIZE positions each"

for i in $(seq 1 $NUM_CHUNKS); do
    echo "Processing chunk $i/$NUM_CHUNKS..."
    
    skip=$(( (i - 1) * CHUNK_SIZE ))
    chunk_name="chunk_$(printf "%03d" $i)"
    
    # Extract chunk
    cargo run --release --bin extract_sample -- \
        --input user_book1.db \
        --output converted_openings/chunks/${chunk_name}.sfen \
        --skip $skip \
        --count $CHUNK_SIZE \
        --strategy sequential
    
    # Convert chunk
    cargo run --release --bin convert_opening_book -- \
        --input converted_openings/chunks/${chunk_name}.sfen \
        --output converted_openings/chunks/${chunk_name}.bin \
        --index converted_openings/chunks/${chunk_name}.idx \
        --progress
    
    # Quick validation (essential checks only)
    cargo run --release --bin validate_conversion -- \
        --original converted_openings/chunks/${chunk_name}.sfen \
        --converted converted_openings/chunks/${chunk_name}.bin \
        --index converted_openings/chunks/${chunk_name}.idx \
        --test-mode quick \
        --report test_results/${chunk_name}_validation.json
    
    # Check validation result
    if [ $? -eq 0 ]; then
        echo "✓ Chunk $i validation passed"
        # Clean up intermediate files to save space
        rm converted_openings/chunks/${chunk_name}.sfen
    else
        echo "✗ Chunk $i validation failed - stopping processing"
        exit 1
    fi
    
    # Save progress
    echo "Completed chunks: $i/$NUM_CHUNKS" > converted_openings/progress.txt
done

echo "All chunks processed successfully!"
```

#### 2.3 Combine Chunks
```bash
# Combine all chunk files
echo "Combining chunks into final files..."

cargo run --release --bin combine_chunks -- \
    --input-dir converted_openings/chunks \
    --output converted_openings/opening_book.bin \
    --index converted_openings/opening_book.idx \
    --verify-integrity
```

### Phase 3: Final Validation

#### 3.1 Comprehensive Final Validation
```bash
echo "Running comprehensive final validation..."

# Full validation suite
cargo run --release --bin validate_conversion -- \
    --original user_book1.db \
    --converted converted_openings/opening_book.bin \
    --index converted_openings/opening_book.idx \
    --test-mode full \
    --sample-count 10000 \
    --report test_results/final_validation.json \
    --verbose

# Performance benchmarks
cargo run --release --bin benchmark_search -- \
    --book converted_openings/opening_book.bin \
    --index converted_openings/opening_book.idx \
    --iterations 100000 \
    --report test_results/performance_benchmark.json

# Statistical analysis
cargo run --release --bin analyze_conversion -- \
    --original user_book1.db \
    --converted converted_openings/opening_book.bin \
    --index converted_openings/opening_book.idx \
    --report test_results/conversion_analysis.json
```

#### 3.2 Integration Tests
```bash
echo "Running integration tests..."

# Test WebAssembly module
cd packages/rust-core
npm run build:wasm

# Run WebAssembly tests
npm run test:wasm -- --grep "opening book"

# Test web integration
cd ../web
npm run test -- --grep "opening book service"
```

## Validation Criteria

### Correctness Validation

#### 1. SFEN Format Validation
```rust
pub fn validate_sfen_format(sfen: &str) -> ValidationResult {
    ValidationResult {
        is_valid: true,
        checks: vec![
            Check::new("board_format", validate_board_format(sfen)),
            Check::new("turn_indicator", validate_turn_indicator(sfen)),
            Check::new("hand_format", validate_hand_format(sfen)),
            Check::new("move_count", validate_move_count(sfen)),
        ]
    }
}
```

#### 2. Chess Logic Validation
```rust
pub fn validate_chess_logic(position: &Position, moves: &[Move]) -> ValidationResult {
    let mut result = ValidationResult::new();
    
    for move_data in moves {
        // Validate move is legal in position
        if !is_legal_move(position, &move_data.move_notation) {
            result.add_error(format!("Illegal move: {}", move_data.move_notation));
        }
        
        // Validate evaluation reasonableness
        if move_data.evaluation.abs() > MAX_REASONABLE_EVAL {
            result.add_warning(format!("Extreme evaluation: {}", move_data.evaluation));
        }
    }
    
    result
}
```

#### 3. Data Integrity Validation
```rust
pub fn validate_data_integrity(original: &SfenEntry, converted: &CompactPosition) -> ValidationResult {
    let mut result = ValidationResult::new();
    
    // Check position hash consistency
    let original_hash = calculate_position_hash(&original.position);
    if original_hash != converted.position_hash {
        result.add_error("Position hash mismatch");
    }
    
    // Check best move preservation
    let original_best = find_best_move(&original.moves);
    let converted_best = decode_move(converted.best_move);
    if original_best.move_notation != converted_best {
        result.add_error("Best move mismatch");
    }
    
    // Check evaluation preservation
    if (original_best.evaluation - converted.evaluation as i32).abs() > EVAL_TOLERANCE {
        result.add_error("Evaluation mismatch");
    }
    
    result
}
```

### Performance Validation

#### 1. File Size Validation
```rust
pub fn validate_compression(original_size: u64, compressed_size: u64) -> ValidationResult {
    let compression_ratio = compressed_size as f64 / original_size as f64;
    let mut result = ValidationResult::new();
    
    if compression_ratio > 0.3 {  // Expect at least 70% reduction
        result.add_warning(format!("Low compression ratio: {:.2}%", compression_ratio * 100.0));
    }
    
    if compression_ratio < 0.05 {  // Sanity check - too much compression
        result.add_error("Compression ratio too low - possible data loss");
    }
    
    result
}
```

#### 2. Search Performance Validation
```rust
pub fn validate_search_performance(book: &OpeningBook) -> ValidationResult {
    let mut result = ValidationResult::new();
    let test_positions = load_test_positions();
    
    let start = Instant::now();
    for position in &test_positions {
        let _ = book.lookup_position(&position.sfen);
    }
    let avg_time = start.elapsed() / test_positions.len() as u32;
    
    if avg_time > Duration::from_millis(1) {
        result.add_error(format!("Search too slow: {:?} per lookup", avg_time));
    }
    
    result
}
```

## Error Handling and Recovery

### Common Error Scenarios

#### 1. Parse Errors
```bash
# Handle malformed SFEN entries
echo "Checking for parse errors..."
if ! cargo run --release --bin validate_sfen -- --input user_book1.db --strict; then
    echo "Found parse errors. Running error analysis..."
    cargo run --release --bin analyze_parse_errors -- \
        --input user_book1.db \
        --output test_results/parse_errors.txt \
        --fix-mode interactive
fi
```

#### 2. Hash Collisions
```bash
# Check for hash collisions
echo "Checking for hash collisions..."
cargo run --release --bin check_hash_collisions -- \
    --input converted_openings/opening_book.bin \
    --index converted_openings/opening_book.idx \
    --report test_results/hash_collision_report.json

if [ $? -ne 0 ]; then
    echo "Hash collisions detected. Switching to different hash function..."
    # Reprocess with different hash algorithm
fi
```

#### 3. Memory Issues
```bash
# Monitor memory usage during processing
echo "Monitoring memory usage..."
cargo run --release --bin convert_opening_book -- \
    --input user_book1.db \
    --output converted_openings/opening_book.bin \
    --index converted_openings/opening_book.idx \
    --memory-limit 8GB \
    --checkpoint-every 100000 \
    --monitor-memory
```

### Recovery Procedures

#### 1. Checkpoint Recovery
```bash
# Resume from checkpoint if processing was interrupted
if [ -f converted_openings/checkpoints/latest.json ]; then
    echo "Found checkpoint, resuming conversion..."
    cargo run --release --bin convert_opening_book -- \
        --resume-from converted_openings/checkpoints/latest.json \
        --input user_book1.db \
        --output converted_openings/opening_book.bin \
        --index converted_openings/opening_book.idx
fi
```

#### 2. Partial Failure Recovery
```bash
# If validation fails on specific chunks, reprocess them
failed_chunks=$(grep -l "FAILED" test_results/chunk_*_validation.json | sed 's/.*chunk_\([0-9]*\)_validation.json/\1/')

for chunk_num in $failed_chunks; do
    echo "Reprocessing failed chunk $chunk_num..."
    # Reprocess with more conservative settings
    chunk_name="chunk_$(printf "%03d" $chunk_num)"
    
    cargo run --release --bin convert_opening_book -- \
        --input converted_openings/chunks/${chunk_name}.sfen \
        --output converted_openings/chunks/${chunk_name}.bin \
        --index converted_openings/chunks/${chunk_name}.idx \
        --conservative-mode \
        --extra-validation
done
```

## Success Criteria

### Validation Thresholds

#### 1. Data Integrity
- **Position Hash Accuracy**: 100% (no collisions allowed)
- **Move Notation Accuracy**: 100% (perfect round-trip)
- **Evaluation Preservation**: 99.9% within ±5 centipawns
- **Best Move Accuracy**: 99.95% exact match

#### 2. Performance
- **File Size Reduction**: 70-90% compression ratio
- **Search Speed**: < 1ms average lookup time
- **Memory Usage**: < 100MB for core book
- **Load Time**: < 5 seconds for initial load

#### 3. Chess Logic
- **Legal Moves**: 100% legal in their positions
- **Position Validity**: 100% valid SFEN format
- **Piece Count Validation**: 100% within legal limits
- **King Safety**: 100% both kings present

### Test Report Format

```json
{
  "validation_report": {
    "timestamp": "2024-01-15T10:30:00Z",
    "input_file": "user_book1.db",
    "output_files": {
      "binary": "opening_book.bin",
      "index": "opening_book.idx"
    },
    "statistics": {
      "original_size_mb": 500,
      "compressed_size_mb": 75,
      "compression_ratio": 0.15,
      "total_positions": 1234567,
      "filtered_positions": 456789,
      "avg_search_time_ms": 0.8
    },
    "validation_results": {
      "sfen_format": {"passed": 456789, "failed": 0},
      "chess_logic": {"passed": 456789, "failed": 0},
      "data_integrity": {"passed": 456785, "failed": 4},
      "performance": {"passed": true, "details": "All benchmarks met"}
    },
    "issues": [
      {
        "type": "warning",
        "message": "4 positions had evaluation discrepancies > 5 centipawns",
        "positions": ["pos_12345", "pos_23456", "pos_34567", "pos_45678"]
      }
    ],
    "conclusion": "PASSED - Ready for production deployment"
  }
}
```

This incremental validation workflow ensures that the large opening book conversion is performed safely with comprehensive error checking and recovery mechanisms at every step.