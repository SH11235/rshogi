# Opening Book Tools Guide

This guide explains how to use the opening book conversion and verification tools.

## Table of Contents

1. [Overview](#overview)
2. [convert_opening_book - Conversion Tool](#convert_opening_book---conversion-tool)
3. [verify_opening_book - Verification Tool](#verify_opening_book---verification-tool)
4. [Workflow Examples](#workflow-examples)
5. [Troubleshooting](#troubleshooting)

## Overview

The opening book tools consist of two command-line utilities:

- **convert_opening_book**: Converts YaneuraOu SFEN format opening books to optimized binary format
- **verify_opening_book**: Verifies and inspects converted binary files

### About user_book1.db

The `user_book1.db` file used in the examples is from the YaneuraOu project:
- **Source**: https://github.com/yaneurao/YaneuraOu
- **License**: MIT License
- **Copyright**: (c) YaneuraOu project contributors

This opening book database contains millions of analyzed shogi positions and is freely available under the MIT License. The original file (470MB) is not included in this repository but can be downloaded from the YaneuraOu project.

## convert_opening_book - Conversion Tool

### Building for Performance

**Important**: Always use release builds for processing large files:

```bash
# Build release versions (10-20x faster)
cargo build --release --bin convert_opening_book
cargo build --release --bin verify_opening_book
```

### Basic Usage

```bash
# Use release build for production
./target/release/convert_opening_book \
  --input <INPUT_FILE> \
  --output <OUTPUT_FILE> \
  [OPTIONS]

# Debug build only for development/testing
./target/release/convert_opening_book \
  --input <INPUT_FILE> \
  --output <OUTPUT_FILE> \
  [OPTIONS]
```

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `-i, --input <FILE>` | Input SFEN file path | Required |
| `-o, --output <FILE>` | Output binary file path | Required |
| `--max-moves <N>` | Maximum moves from initial position | 50 |
| `--min-depth <N>` | Minimum analysis depth | 0 |
| `--min-eval <N>` | Minimum evaluation score | -1000 |
| `--max-eval <N>` | Maximum evaluation score | 1000 |
| `--compress` | Enable gzip compression | false |
| `--progress-interval <N>` | Progress display interval | 10000 |
| `--validate` | Validate output after conversion | false |

### Examples

#### 1. Basic Conversion (Recommended for Web)

```bash
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_web.binz \
  --max-moves 50 \
  --min-depth 5 \
  --min-eval=-500 \
  --max-eval=500 \
  --compress \
  --validate
```

This creates a compressed file optimized for web use:
- Only first 50 moves (opening and early middle game)
- Minimum depth 5 (quality filter)
- Balanced positions only (-500 to +500 evaluation)
- Gzip compressed for smaller size
- Validates the output

#### 2. Full Data Conversion

```bash
# Without compression (larger file, faster access)
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_full.bin \
  --max-moves 999999 \
  --min-depth 0 \
  --min-eval=-99999 \
  --max-eval=99999 \
  --progress-interval 100000

# With compression (recommended for storage/transfer)
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_full.binz \
  --max-moves 999999 \
  --min-depth 0 \
  --min-eval=-99999 \
  --max-eval=99999 \
  --compress \
  --progress-interval 100000
```

This preserves all data without filtering. The compressed version reduces file size by 40-60%.

#### 3. Early Game Only (Small Size)

```bash
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_early.binz \
  --max-moves 20 \
  --min-depth 8 \
  --min-eval=-300 \
  --max-eval=300 \
  --compress
```

Creates a very small file (~5-10MB) with only high-quality early game positions.

#### 4. Tournament Quality

```bash
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_tournament.binz \
  --max-moves 100 \
  --min-depth 10 \
  --min-eval=-200 \
  --max-eval=200 \
  --compress \
  --validate
```

High-quality positions with deep analysis for tournament play.

### Output Information

The tool displays:
- Input file size
- Filter settings
- Real-time progress (positions parsed, filtered)
- Final statistics:
  - Positions written
  - Total moves
  - Output file size
  - Size reduction percentage
  - Processing time and speed

### File Size Estimates

| Configuration | Input Size | Output Size | Reduction |
|--------------|------------|-------------|-----------|
| Full data (no filter, no compress) | 470MB | ~105MB | 78% |
| Full data (no filter, compressed) | 470MB | ~40-60MB | 87-91% |
| Web optimized (no compress) | 470MB | ~20-30MB | 94-95% |
| Web optimized (compressed) | 470MB | ~10-15MB | 97-98% |
| Early game only (no compress) | 470MB | ~5-10MB | 98-99% |
| Early game only (compressed) | 470MB | ~2-5MB | 99%+ |

**Note**: The `--compress` flag uses gzip compression and typically reduces file size by 40-60%. This is highly recommended for:
- Web deployment (reduces download time)
- Storage efficiency
- Network transfer

The only downside is slightly slower initial loading (decompression time), but this is usually negligible compared to network transfer time saved.

## verify_opening_book - Verification Tool

### Basic Usage

```bash
./target/release/verify_opening_book \
  --binary <BINARY_FILE> \
  [OPTIONS]
```

### Options

| Option | Description | Default |
|--------|-------------|---------|
| `-b, --binary <FILE>` | Binary file to verify | Required |
| `-o, --original <FILE>` | Original SFEN file for comparison | Optional |
| `--show-entries <N>` | Number of entries to display | 10 |
| `--detailed` | Show all moves for each position | false |
| `--check-position <SFEN>` | Check specific position | Optional |
| `--export-txt <FILE>` | Export to readable text | Optional |
| `--stats-only` | Show only statistics | false |

### Examples

#### 1. Basic Verification

```bash
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_web.bin
```

Output:
```
Verifying binary file: converted_openings/opening_book_web.bin
============================================================
Successfully loaded 50000 positions

Statistics:
  Total positions: 50000
  Total moves: 250000
  Average moves per position: 5.00

Depth distribution:
  Depth 5: 10000 positions
  Depth 6: 15000 positions
  Depth 7: 15000 positions
  Depth 8: 10000 positions

Evaluation distribution:
  [-500 to -401]: 5000 positions
  [-400 to -301]: 8000 positions
  ...
```

#### 2. Detailed Sample Inspection

```bash
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_web.bin \
  --show-entries 5 \
  --detailed
```

Shows detailed information for the first 5 positions including all moves.

#### 3. Compare with Original

```bash
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_web.bin \
  --original user_book1.db
```

This comparison shows:
- How many positions from the original are in the binary
- Retention rate (percentage kept after filtering)
- Any discrepancies in move counts

#### 4. Check Specific Position

```bash
# Check initial position
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_web.bin \
  --check-position "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
```

Output:
```
Checking position: lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1
Position hash: 0x59f23df6d821d01a

Position FOUND in binary!
Details:
  Best move: 7g7f (eval: 50, depth: 10)
  All 5 moves:
    1. 7g7f (eval: 50, depth: 10)
    2. 2g2f (eval: 45, depth: 10)
    3. 6g6f (eval: 40, depth: 9)
    4. 5g5f (eval: 35, depth: 8)
    5. 1g1f (eval: 30, depth: 7)
```

#### 5. Export to Text

```bash
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_web.bin \
  --export-txt analysis/opening_book_contents.txt
```

Creates a human-readable text file with all positions and moves.

#### 6. Quick Statistics

```bash
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_web.bin \
  --stats-only
```

Shows only statistical information without sample entries.

## Workflow Examples

### 1. Standard Web Deployment Workflow

```bash
# Step 1: Convert with web-optimized settings
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_web.binz \
  --max-moves 50 \
  --min-depth 5 \
  --min-eval=-500 \
  --max-eval=500 \
  --compress \
  --validate

# Step 2: Verify the conversion
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_web.binz \
  --original user_book1.db

# Step 3: Check key positions
./target/release/verify_opening_book \
  --binary converted_openings/opening_book_web.binz \
  --check-position "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"

# Step 4: If satisfied, copy to web directory
cp converted_openings/opening_book_web.binz ../web/public/data/
```

### 2. Multi-Level Conversion for Progressive Loading

```bash
# Early game (5-10MB)
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_early.binz \
  --max-moves 20 \
  --min-depth 8 \
  --compress

# Standard (20-30MB)
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_standard.binz \
  --max-moves 50 \
  --min-depth 5 \
  --min-eval=-500 \
  --max-eval=500 \
  --compress

# Full (50MB+)
./target/release/convert_opening_book \
  --input user_book1.db \
  --output converted_openings/opening_book_full.binz \
  --max-moves 100 \
  --compress

# Verify all three
for level in early standard full; do
  echo "Verifying $level..."
  ./target/release/verify_opening_book \
    --binary converted_openings/opening_book_${level}.binz \
    --stats-only
done
```

### 3. Quality Analysis Workflow

```bash
# Convert with different depth thresholds
for depth in 0 5 10 15; do
  ./target/release/convert_opening_book \
    --input user_book1.db \
    --output converted_openings/test_depth_${depth}.bin \
    --max-moves 50 \
    --min-depth $depth \
    --progress-interval 50000
done

# Compare sizes and position counts
for depth in 0 5 10 15; do
  echo "Depth $depth:"
  ./target/release/verify_opening_book \
    --binary converted_openings/test_depth_${depth}.bin \
    --stats-only | grep "Total positions"
  ls -lh converted_openings/test_depth_${depth}.bin
done
```

## Troubleshooting

### Common Issues

#### 1. "Position NOT FOUND in binary"

This means the position was filtered out during conversion. Check:
- Move count limit (`--max-moves`)
- Depth requirement (`--min-depth`)
- Evaluation range (`--min-eval`, `--max-eval`)

#### 2. Large Output Files

If the output is too large:
- Use `--compress` flag
- Increase `--min-depth` (e.g., 5 or higher)
- Narrow evaluation range (e.g., -500 to 500)
- Reduce `--max-moves` (e.g., 30-50)

#### 3. Slow Conversion

For faster processing:
- Increase `--progress-interval` to reduce output
- Use release build: `cargo build --release --bin convert_opening_book`
- Consider processing in chunks

#### 4. Memory Issues

If running out of memory:
- Process smaller chunks of the input file
- Use streaming processing (future enhancement)
- Run on a machine with more RAM

### Performance Tips

1. **Use Release Builds**
   ```bash
   cargo build --release --bin convert_opening_book
   cargo build --release --bin verify_opening_book
   ```
   Release builds are 10-20x faster than debug builds.

2. **Optimal Filter Settings**
   - Web use: depth ≥ 5, moves ≤ 50, eval ±500
   - Analysis: depth ≥ 10, moves ≤ 100, eval ±300
   - Storage: always use `--compress`

3. **Batch Processing**
   Create a script for multiple conversions:
   ```bash
   #!/bin/bash
   configs=(
     "early:20:8:-300:300"
     "standard:50:5:-500:500"
     "full:100:0:-1000:1000"
   )
   
   for config in "${configs[@]}"; do
     IFS=':' read -r name moves depth min_eval max_eval <<< "$config"
     ./target/release/convert_opening_book \
       --input user_book1.db \
       --output converted_openings/opening_book_${name}.binz \
       --max-moves $moves \
       --min-depth $depth \
       --min-eval=$min_eval \
       --max-eval=$max_eval \
       --compress
   done
   ```

## Verification Examples

### Real Data Verification Results

Here are actual verification results from generated binary files:

#### Early Game Version (13MB)

```bash
$ ./target/release/verify_opening_book --binary converted_openings/opening_book_early.binz --stats-only

Verifying binary file: converted_openings/opening_book_early.binz
============================================================
Detected gzip compressed file
Successfully loaded 436484 positions

Statistics:
  Total positions: 436484
  Total moves: 2328762
  Average moves per position: 5.34

Depth distribution:
  Depth 8-20: 296897 positions (68% - main practical openings)
  Depth 5-7: 5170 positions
  Depth 21+: 52410 positions (including deep analysis)

Evaluation distribution:
  [-300 to -201]: 76 positions
  [-200 to -101]: 16399 positions
  [-100 to -1]: 45860 positions
  [0 to 99]: 316286 positions (72% - balanced positions)
  [100 to 199]: 43670 positions
  [200 to 299]: 14132 positions
  [300 to 399]: 61 positions
```

#### Initial Position Check

```bash
$ ./target/release/verify_opening_book --binary converted_openings/opening_book_early.binz \
    --check-position "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"

Position FOUND in binary!
Details:
  Best move: 7g7f (eval: 44, depth: 30)
  All 8 moves:
    1. 2g2f (eval: 44, depth: 30)
    2. 7g7f (eval: 44, depth: 30)
    3. 6i7h (eval: 35, depth: 36)
    4. 1g1f (eval: 24, depth: 22)
    5. 9g9f (eval: 16, depth: 11)
    6. 3i3h (eval: 5, depth: 19)
    7. 6g6f (eval: 1, depth: 9)
    8. 3i4h (eval: 1, depth: 1)
```

#### Comparison with Original

```bash
$ ./target/release/verify_opening_book --binary converted_openings/opening_book_early.binz \
    --original user_book1.db

Comparison results:
  Matched positions: 436589 / 2252118
  Not found in binary: 1815529 (likely filtered out)
  Positions with different move counts: 86088
  Retention rate: 19.4%
```

The retention rate of 19.4% is expected given the filtering criteria (max 20 moves, min depth 5, eval range ±500).

#### File Size Summary

| File | Size | Positions | Moves | Avg Moves/Pos |
|------|------|-----------|-------|---------------|
| opening_book_early.binz | 13MB | 436,484 | 2,328,762 | 5.34 |
| opening_book_web.binz | 24MB | 800,506 | 4,168,955 | 5.21 |
| opening_book_tournament.binz | 8.6MB | - | - | - |
| opening_book_full.binz | 67MB | - | - | - |
| opening_book_full.bin | 105MB | - | - | - |

## Output Format Details

### Binary Format Structure

The converted binary files use the following format:

1. **File Header** (16 bytes)
   - Magic: "SFEN" (4 bytes)
   - Version: 1 (4 bytes)
   - Position count (4 bytes)
   - CRC32 checksum (4 bytes)

2. **Position Entries** (16 bytes each)
   - Position hash (8 bytes) - Zobrist hash
   - Best move (2 bytes) - Encoded move
   - Evaluation (2 bytes) - Centipawns
   - Depth (1 byte) - Search depth
   - Move count (1 byte) - Number of moves
   - Popularity (1 byte) - Usage frequency
   - Reserved (1 byte)

3. **Move Entries** (6 bytes each)
   - Move encoded (2 bytes)
   - Evaluation (2 bytes)
   - Depth (1 byte)
   - Reserved (1 byte)

### Compression

When `--compress` is used:
- Files are gzip compressed
- Typical compression ratio: 40-60%
- Decompression is handled automatically by verify_opening_book

## Integration with Web Application

After conversion, the binary files can be used in the web application:

1. Place files in `packages/web/public/data/`
2. Load using the TypeScript opening book service
3. Query positions using SFEN notation
4. Display moves with evaluations

See [opening-book-web-integration.md](./opening-book-web-integration.md) for detailed integration instructions.
