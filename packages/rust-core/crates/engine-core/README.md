# rshogi-core

A high-performance shogi (Japanese chess) engine core library written in Rust.

## Features

- **Bitboard-based board representation** - Fast move generation and position evaluation
- **NNUE evaluation** - Neural network-based evaluation with HalfKP architecture support
- **Alpha-beta search** - With various pruning techniques (null move, futility, LMR, etc.)
- **Transposition table** - Lock-free concurrent hash table
- **Time management** - Adaptive time control for various time settings
- **Multi-threaded search** - Lazy SMP parallel search support

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
rshogi-core = "0.1"
```

## Usage

```rust
use rshogi_core::{Position, SearchOptions, Engine};

// Create a new position (starting position)
let mut pos = Position::default();

// Create engine and search
let mut engine = Engine::new();
engine.set_position(pos);

let options = SearchOptions {
    depth: Some(10),
    ..Default::default()
};

let result = engine.search(&options);
println!("Best move: {}", result.best_move);
```

## License

MIT License
