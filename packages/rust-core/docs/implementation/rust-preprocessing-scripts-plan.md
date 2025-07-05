# Rust Preprocessing Scripts Implementation Plan

## Overview

This document outlines the implementation plan for Rust scripts that will preprocess the 500MB YaneuraOu opening book into an optimized format for web deployment.

## Script Architecture

### Core Components

```
packages/rust-core/src/
├── bin/                              # Binary executables
│   ├── convert_opening_book.rs       # Main conversion script
│   ├── create_position_index.rs      # Index creation
│   ├── filter_positions.rs           # Position filtering
│   ├── validate_conversion.rs        # Data validation
│   └── analyze_book_stats.rs         # Statistics analysis
├── opening_book/                     # Library modules
│   ├── mod.rs                        # Module definition
│   ├── parser.rs                     # SFEN parser
│   ├── compact_format.rs             # Binary format
│   ├── position_hasher.rs            # Position hashing
│   ├── filter.rs                     # Filtering logic
│   └── validator.rs                  # Validation utilities
└── utils/                            # Utility functions
    ├── mod.rs
    ├── sfen_utils.rs                 # SFEN manipulation
    └── hash_utils.rs                 # Hashing utilities
```

## Data Structures

### 1. Input Data Structures

```rust
// Raw SFEN entry from file
#[derive(Debug, Clone)]
pub struct RawSfenEntry {
    pub position: String,
    pub turn: char,
    pub hand: String,
    pub move_count: u32,
    pub moves: Vec<RawMove>,
}

#[derive(Debug, Clone)]
pub struct RawMove {
    pub move_notation: String,
    pub move_type: String,
    pub evaluation: i32,
    pub depth: u32,
    pub nodes: u64,
}
```

### 2. Compact Binary Format

```rust
// Compact position representation
#[derive(Debug, Clone)]
#[repr(C)]
pub struct CompactPosition {
    pub position_hash: u64,      // 8 bytes
    pub best_move: u16,          // 2 bytes - encoded move
    pub evaluation: i16,         // 2 bytes
    pub depth: u8,               // 1 byte
    pub move_count: u8,          // 1 byte - number of moves
    pub popularity: u8,          // 1 byte - usage frequency
    pub reserved: u8,            // 1 byte - alignment
    // Total: 16 bytes header
}

// Alternative moves (variable length)
#[derive(Debug, Clone)]
#[repr(C)]
pub struct CompactMove {
    pub move_encoded: u16,       // 2 bytes
    pub evaluation: i16,         // 2 bytes
    pub depth: u8,               // 1 byte
    pub reserved: u8,            // 1 byte - alignment
    // Total: 6 bytes per move
}
```

### 3. Index Structure

```rust
// Position index for O(1) lookup
#[derive(Debug, Clone)]
#[repr(C)]
pub struct PositionIndex {
    pub hash: u64,               // 8 bytes
    pub offset: u32,             // 4 bytes - file offset
    pub length: u16,             // 2 bytes - data length
    pub reserved: u16,           // 2 bytes - alignment
    // Total: 16 bytes per index entry
}
```

## Script Implementations

### 1. SFEN Parser (`parser.rs`)

```rust
use std::io::{BufRead, BufReader};
use std::fs::File;
use anyhow::Result;

pub struct SfenParser {
    reader: BufReader<File>,
    current_position: Option<RawSfenEntry>,
}

impl SfenParser {
    pub fn new(file_path: &str) -> Result<Self> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        Ok(Self {
            reader,
            current_position: None,
        })
    }

    pub fn parse_line(&mut self, line: &str) -> Result<Option<RawSfenEntry>> {
        if line.starts_with("#") {
            // Skip header
            return Ok(None);
        }

        if line.starts_with("sfen ") {
            // New position
            self.current_position = Some(self.parse_position_line(line)?);
            Ok(None)
        } else if !line.trim().is_empty() {
            // Move line
            if let Some(ref mut pos) = self.current_position {
                let move_data = self.parse_move_line(line)?;
                pos.moves.push(move_data);
            }
            Ok(None)
        } else {
            // Empty line - end of position
            Ok(self.current_position.take())
        }
    }

    fn parse_position_line(&self, line: &str) -> Result<RawSfenEntry> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Parse: sfen <position> <turn> <hand> <move_count>
        Ok(RawSfenEntry {
            position: parts[1].to_string(),
            turn: parts[2].chars().next().unwrap(),
            hand: parts[3].to_string(),
            move_count: parts[4].parse()?,
            moves: Vec::new(),
        })
    }

    fn parse_move_line(&self, line: &str) -> Result<RawMove> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Parse: <move> <type> <eval> <depth> <nodes>
        Ok(RawMove {
            move_notation: parts[0].to_string(),
            move_type: parts[1].to_string(),
            evaluation: parts[2].parse()?,
            depth: parts[3].parse()?,
            nodes: parts[4].parse()?,
        })
    }
}
```

### 2. Position Hasher (`position_hasher.rs`)

```rust
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub struct PositionHasher;

impl PositionHasher {
    pub fn hash_position(position: &str, turn: char, hand: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        position.hash(&mut hasher);
        turn.hash(&mut hasher);
        hand.hash(&mut hasher);
        hasher.finish()
    }

    pub fn hash_sfen_entry(entry: &RawSfenEntry) -> u64 {
        Self::hash_position(&entry.position, entry.turn, &entry.hand)
    }
}
```

### 3. Move Encoder (`compact_format.rs`)

```rust
pub struct MoveEncoder;

impl MoveEncoder {
    pub fn encode_move(move_notation: &str) -> Result<u16> {
        // Encode move notation to 16-bit integer
        // Format: FFFFTTTT PPPPPPPP
        // F: From square (4 bits)
        // T: To square (4 bits)  
        // P: Piece type and flags (8 bits)
        
        if move_notation.contains("*") {
            // Drop move: P*5g
            Self::encode_drop_move(move_notation)
        } else {
            // Normal move: 8i7g or 3d3c+
            Self::encode_normal_move(move_notation)
        }
    }

    fn encode_normal_move(move_notation: &str) -> Result<u16> {
        let promotion = move_notation.ends_with("+");
        let move_part = if promotion {
            &move_notation[..move_notation.len()-1]
        } else {
            move_notation
        };

        if move_part.len() == 4 {
            let from_file = move_part.chars().nth(0).unwrap().to_digit(10).unwrap() as u16;
            let from_rank = move_part.chars().nth(1).unwrap().to_digit(10).unwrap() as u16;
            let to_file = move_part.chars().nth(2).unwrap().to_digit(10).unwrap() as u16;
            let to_rank = move_part.chars().nth(3).unwrap().to_digit(10).unwrap() as u16;

            let from_square = (from_file - 1) * 9 + (from_rank - 1);
            let to_square = (to_file - 1) * 9 + (to_rank - 1);
            let flags = if promotion { 0x8000 } else { 0 };

            Ok(flags | (from_square << 7) | to_square)
        } else {
            Err(anyhow::anyhow!("Invalid move notation: {}", move_notation))
        }
    }

    fn encode_drop_move(move_notation: &str) -> Result<u16> {
        let parts: Vec<&str> = move_notation.split('*').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!("Invalid drop notation: {}", move_notation));
        }

        let piece = Self::encode_piece(parts[0])?;
        let square = Self::encode_square(parts[1])?;

        Ok(0x4000 | (piece << 7) | square)
    }

    fn encode_piece(piece: &str) -> Result<u16> {
        match piece {
            "P" => Ok(0),
            "L" => Ok(1),
            "N" => Ok(2),
            "S" => Ok(3),
            "G" => Ok(4),
            "B" => Ok(5),
            "R" => Ok(6),
            _ => Err(anyhow::anyhow!("Invalid piece: {}", piece)),
        }
    }

    fn encode_square(square: &str) -> Result<u16> {
        if square.len() != 2 {
            return Err(anyhow::anyhow!("Invalid square: {}", square));
        }

        let file = square.chars().nth(0).unwrap().to_digit(10).unwrap() as u16;
        let rank = square.chars().nth(1).unwrap().to_digit(10).unwrap() as u16;

        Ok((file - 1) * 9 + (rank - 1))
    }
}
```

### 4. Position Filter (`filter.rs`)

```rust
pub struct PositionFilter {
    min_depth: u32,
    max_moves: usize,
    min_evaluation: i32,
    max_evaluation: i32,
}

impl PositionFilter {
    pub fn new() -> Self {
        Self {
            min_depth: 0,
            max_moves: 50,
            min_evaluation: -1000,
            max_evaluation: 1000,
        }
    }

    pub fn should_include(&self, entry: &RawSfenEntry) -> bool {
        // Filter by move count (early game only)
        if entry.move_count > self.max_moves as u32 {
            return false;
        }

        // Filter by move quality
        if entry.moves.is_empty() {
            return false;
        }

        // At least one move should have sufficient depth
        let has_analyzed_move = entry.moves.iter()
            .any(|m| m.depth >= self.min_depth);

        if !has_analyzed_move {
            return false;
        }

        // Best move evaluation should be reasonable
        let best_eval = entry.moves.iter()
            .map(|m| m.evaluation)
            .max()
            .unwrap_or(0);

        best_eval >= self.min_evaluation && best_eval <= self.max_evaluation
    }

    pub fn filter_moves(&self, entry: &mut RawSfenEntry) {
        // Keep only the best few moves
        entry.moves.sort_by(|a, b| b.evaluation.cmp(&a.evaluation));
        entry.moves.truncate(8); // Keep top 8 moves
    }
}
```

### 5. Main Conversion Script (`convert_opening_book.rs`)

```rust
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use anyhow::Result;

fn main() -> Result<()> {
    let input_file = "user_book1.db";
    let output_file = "opening_book.bin";
    let index_file = "opening_book.idx";

    println!("Converting opening book from {} to {}", input_file, output_file);

    let mut parser = SfenParser::new(input_file)?;
    let mut filter = PositionFilter::new();
    let mut converter = BinaryConverter::new(output_file, index_file)?;

    let file = File::open(input_file)?;
    let reader = BufReader::new(file);

    let mut total_positions = 0;
    let mut filtered_positions = 0;
    let mut current_entry: Option<RawSfenEntry> = None;

    for line in reader.lines() {
        let line = line?;
        
        if let Some(entry) = parser.parse_line(&line)? {
            current_entry = Some(entry);
        } else if line.trim().is_empty() {
            // Process completed entry
            if let Some(mut entry) = current_entry.take() {
                total_positions += 1;
                
                if filter.should_include(&entry) {
                    filter.filter_moves(&mut entry);
                    converter.write_position(&entry)?;
                    filtered_positions += 1;
                }

                if total_positions % 10000 == 0 {
                    println!("Processed {} positions, kept {}", 
                             total_positions, filtered_positions);
                }
            }
        }
    }

    // Process final entry
    if let Some(mut entry) = current_entry {
        total_positions += 1;
        if filter.should_include(&entry) {
            filter.filter_moves(&mut entry);
            converter.write_position(&entry)?;
            filtered_positions += 1;
        }
    }

    converter.finalize()?;

    println!("Conversion complete!");
    println!("Total positions: {}", total_positions);
    println!("Filtered positions: {}", filtered_positions);
    println!("Compression ratio: {:.2}%", 
             (filtered_positions as f64 / total_positions as f64) * 100.0);

    Ok(())
}
```

## Build Configuration

### Cargo.toml additions

```toml
[[bin]]
name = "convert_opening_book"
path = "src/bin/convert_opening_book.rs"

[[bin]]
name = "create_position_index"
path = "src/bin/create_position_index.rs"

[[bin]]
name = "filter_positions"
path = "src/bin/filter_positions.rs"

[[bin]]
name = "validate_conversion"
path = "src/bin/validate_conversion.rs"

[[bin]]
name = "analyze_book_stats"
path = "src/bin/analyze_book_stats.rs"

[[bin]]
name = "extract_sample"
path = "src/bin/extract_sample.rs"

[[bin]]
name = "test_conversion"
path = "src/bin/test_conversion.rs"

[[bin]]
name = "combine_chunks"
path = "src/bin/combine_chunks.rs"

[[bin]]
name = "benchmark_search"
path = "src/bin/benchmark_search.rs"

[dependencies]
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
bincode = "1.3"
clap = { version = "4.0", features = ["derive"] }
indicatif = "0.17"
rayon = "1.7"
sha2 = "0.10"
crc32fast = "1.3"
rand = "0.8"

[dev-dependencies]
criterion = "0.5"
tempfile = "3.8"
pretty_assertions = "1.4"
test-case = "3.3"
```

## Usage Commands

```bash
# Convert opening book
cargo run --bin convert_opening_book -- \
    --input user_book1.db \
    --output opening_book.bin \
    --index opening_book.idx \
    --filter-depth 0 \
    --max-moves 50

# Create position index
cargo run --bin create_position_index -- \
    --input opening_book.bin \
    --output opening_book.idx

# Validate conversion
cargo run --bin validate_conversion -- \
    --original user_book1.db \
    --converted opening_book.bin \
    --index opening_book.idx

# Analyze statistics
cargo run --bin analyze_book_stats -- \
    --input opening_book.bin \
    --index opening_book.idx
```

## Performance Optimizations

### 1. Parallel Processing
```rust
use rayon::prelude::*;

// Process positions in parallel
let results: Vec<_> = positions
    .par_iter()
    .filter_map(|entry| {
        if filter.should_include(entry) {
            Some(converter.convert_position(entry))
        } else {
            None
        }
    })
    .collect();
```

### 2. Memory Mapping
```rust
use memmap2::MmapOptions;

// Memory-map large files for efficient access
let file = File::open("user_book1.db")?;
let mmap = unsafe { MmapOptions::new().map(&file)? };
```

### 3. Buffered I/O
```rust
use std::io::BufWriter;

// Use buffered writer for output
let output_file = File::create("opening_book.bin")?;
let mut writer = BufWriter::new(output_file);
```

## Error Handling

### Validation Points
1. **Input File Format**: Validate SFEN format
2. **Position Parsing**: Handle malformed positions
3. **Move Notation**: Validate move formats
4. **Hash Collisions**: Detect and handle collisions
5. **File I/O**: Handle disk space and permissions

### Recovery Strategies
1. **Partial Conversion**: Save progress periodically
2. **Resumable Processing**: Support continuing from checkpoints
3. **Fallback Modes**: Graceful degradation on errors
4. **Detailed Logging**: Track conversion issues

## Testing Strategy

### Unit Tests
- SFEN parsing accuracy
- Move encoding/decoding
- Position hashing consistency
- Filter logic correctness

### Integration Tests
- Full conversion pipeline
- Binary format validation
- Index creation and lookup
- Performance benchmarks

### Validation Tests
- Compare original vs converted data
- Verify position lookup accuracy
- Test edge cases and malformed input
- Memory usage profiling

This comprehensive plan provides a robust foundation for converting the 500MB opening book into an optimized format suitable for web deployment.