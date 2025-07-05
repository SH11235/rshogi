# Opening Book Data

This directory contains the opening book data files for the Shogi web application.

## Structure

- `openings/` - Generated binary opening book files
  - `opening_book_early.bin.gz` - Small dataset for early game (first 10 moves)
  - `opening_book_standard.bin.gz` - Standard dataset (first 20 moves)
  - `opening_book_full.bin.gz` - Full dataset (all moves)

## Generation

To generate opening book files from JSON source data:

```bash
# From rust-core directory
make convert-opening-book
```

To copy generated files to the web package:

```bash
# From rust-core directory
make generate-openings
```

## File Format

The binary files use a custom format optimized for WebAssembly:
- Position hash (8 bytes) using Zobrist hashing
- Move count (2 bytes)
- Padding (6 bytes)
- Move data: encoded move (2 bytes) + evaluation (2 bytes) + depth (1 byte) + padding (1 byte)

Files are compressed with gzip for efficient web delivery.