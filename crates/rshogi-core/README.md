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
use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::search::{Search, LimitsType};

// Create a new position (starting position)
let mut pos = Position::new();
pos.set_sfen(SFEN_HIRATE).unwrap();

// Create search engine (transposition table size: 64MB)
let mut search = Search::new(64);

// Set search limits
let mut limits = LimitsType::new();
limits.depth = 10;

// Run search
let result = search.go(&mut pos, limits, None::<fn(&_)>);
println!("Best move: {}", result.best_move.to_usi());
```

## License

GPL-3.0-only License

## 参考・影響 / Acknowledgements

本クレートは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) およびチェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。
アルゴリズムや評価のアイデアに影響を受けていますが、実装と構成は独自です。
