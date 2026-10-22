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
rshogi-core = "0.8"
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

## NNUE の読み込み

`NNUENetwork::read` は `Read + Seek`、LayerStacks FT の `read_leb128` は `Read` のみで利用できます。
LEB128 ブロックは実際に読めたバイト数に応じてバッファを確保し、重みの確保前に要素数を検証します。
既定の universal edition では、非圧縮の動的な重み配列も必要バイト数と入力の残長を確保前に照合します。
これらの入力が途中で切れていたり要素数が合わなかったりする場合は `io::Error` を返します。
これらの重みと圧縮バッファの確保失敗は、取得可能な場合 `ErrorKind::OutOfMemory` で返します。
圧縮ブロックの上限は 2 GiB です。

## LayerStacks の progress 係数

`nnue::get_layer_stack_progress_kpabs_weights()` は所有権付きの
`ProgressKpAbsWeights` を返す。`&weights` を `&[f32]` として渡せる。
係数を再設定・resetしても取得済みのsnapshotは有効で、最後の所有者が破棄されるまで保持される。
探索stackへの反映は生成・reset時に行う。探索中はstackが保持する係数を参照する。
固定editionと実行時寸法版の `NNUEEvaluator` も同じ f32 係数をresetまで保持する。
`refresh()` はアキュムレータの再計算のみで、係数のsnapshotは更新しない。

stackを受け取らない低level API `NetworkLayerStacks::evaluate()` と
`evaluate_with_diagnostics()` は、評価ごとにglobal設定のsnapshotを取得する。
係数の寿命を評価器に固定する場合は `NNUEEvaluator` を使う。

## License

GPL-3.0-or-later License

## 参考・影響 / Acknowledgements

本クレートは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) およびチェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。
