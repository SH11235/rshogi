# Shogi AI Engine

## Phase 1 Implementation Summary

### Completed Components

1. **Bitboard Representation** (`board.rs`)
   - 128-bit bitboards for 81-square shogi board
   - Efficient bit manipulation operations
   - Support for all piece types and promoted pieces
   - Zobrist hashing integration

2. **Move Generation** (`movegen.rs`)
   - Complete legal move generation
   - Support for all piece movements
   - Drop moves from captured pieces
   - Pin detection and check evasion
   - Promotion handling

3. **Attack Tables** (`attacks.rs`)
   - Pre-computed attack tables for non-sliding pieces
   - Efficient sliding piece attacks using classical approach
   - Direction-aware piece movements

4. **Search Engine** (`search.rs`)
   - Alpha-beta pruning search
   - Iterative deepening
   - Principal variation tracking
   - Time and node limits

5. **Evaluation Function** (`evaluate.rs`)
   - Material-based evaluation
   - Promotion bonuses
   - Basic king safety

6. **Zobrist Hashing** (`zobrist.rs`)
   - Deterministic hash generation
   - Support for pieces, hands, and side to move
   - Position repetition detection

### Performance Benchmarks

- **Move Generation**: ~2M moves/second
- **Search Speed**: ~4.6M nodes/second
- **Search Depth**: 8 plies in 5 seconds

### Known Issues

1. No transposition table yet
2. Basic evaluation function needs improvement

### Recently Fixed

1. Drop pawn mate (打ち歩詰め) detection is now complete with comprehensive test coverage including:
   - Basic drop pawn mate detection
   - Pinned defender scenarios
   - Multiple edge cases and false positive prevention

### Next Steps (Phase 2-4)

- Phase 2: SIMD optimization for bitboards
- Phase 3: Transposition table and advanced search
- Phase 4: NNUE evaluation and WebAssembly integration

## Usage

```rust
use shogi_core::ai::{Position, Searcher, SearchLimits};
use std::time::Duration;

// Create starting position
let pos = Position::startpos();

// Set search limits
let limits = SearchLimits {
    depth: 8,
    time: Some(Duration::from_secs(5)),
    nodes: None,
};

// Search for best move
let mut searcher = Searcher::new(limits);
let result = searcher.search(&pos);

println!("Best move: {:?}", result.best_move);
println!("Score: {}", result.score);
```

## Testing

Run all AI tests:
```bash
cargo test ai::
```

Run benchmark:
```bash
cargo run --release --bin shogi_benchmark
```