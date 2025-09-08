# Rust Core for Shogi

[![codecov](https://codecov.io/gh/SH11235/shogi/branch/main/graph/badge.svg?flag=rust-core)](https://codecov.io/gh/SH11235/shogi)

This package contains the WebAssembly (WASM) implementation for advanced Shogi features including WebRTC communication, mate search, and opening book functionality.

## Features

- 🌐 WebRTC peer-to-peer communication for online play
- 🔍 Mate search algorithm implementation
- 📚 Opening book with binary format support
- 🎯 High-performance position hashing and move encoding
- 🤖 USI protocol engine with multiple search/evaluation modes
- 🧠 NNUE evaluation function support
- ⚡ Enhanced search with advanced pruning techniques
- 📊 NNUE training tools for machine learning

## Prerequisites

- Rust toolchain (install from https://rustup.rs/)
- wasm-pack (`cargo install wasm-pack`)
- cargo-tarpaulin (optional, for coverage reports): `cargo install cargo-tarpaulin`

## Project Structure

```
crates/
├── engine-core/             # Core engine implementation
│   ├── search/             # Search algorithms (basic & enhanced)
│   ├── evaluation/         # Evaluation functions (material & NNUE)
│   └── time_management/    # Time control
├── engine-usi/              # USI protocol command-line interface
└── webrtc-p2p/             # WebRTC communication

src/                         # Legacy WASM modules
├── lib.rs                   # Main library entry point
├── simple_webrtc.rs        # WebRTC implementation
├── mate_search.rs          # Mate search algorithm
├── opening_book/           # Opening book module
│   ├── mod.rs             # Module exports
│   ├── binary_converter.rs # Binary format conversion
│   ├── data_structures.rs  # Core data types
│   ├── move_encoder.rs     # Move encoding/decoding
│   ├── position_filter.rs  # Position filtering logic
│   ├── position_hasher.rs  # Position hashing
│   └── sfen_parser.rs      # SFEN format parsing
└── opening_book_reader.rs  # Opening book reader interface
```

## Documentation

- [Engine Types Guide](docs/engine-types-guide.md) - エンジンタイプの選択ガイド（推奨: EnhancedNnue）
- [Performance Documentation](docs/performance/) - ベンチマーク、プロファイリング、性能分析
- [Development Guide](docs/development/) - TDD開発ガイド、テスト戦略
- [Implementation Docs](docs/implementation/) - 実装詳細
- [Reference](docs/reference/) - フォーマット仕様など

## USI Engine Usage

### Quick Start
```bash
# Build and run the USI engine
cargo build --release --bin engine-usi
./target/release/engine-usi

# Set to strongest mode (EnhancedNnue)
setoption name EngineType value EnhancedNnue

# Basic commands
usi
isready
position startpos
go movetime 1000
quit
```

### Engine Types
- **EnhancedNnue** (推奨): 最強 - 高度な探索 + NNUE評価
- **Nnue**: 高速分析用
- **Enhanced**: 省メモリ環境用
- **Material**: デバッグ用

### Engine Options

| Option | Type | Default | Range | Description |
|--------|------|---------|-------|-------------|
| USI_Hash | Spin | 16 | 1-1024 | Hash table size in MB |
| Threads | Spin | 1 | 1-256 | Number of search threads |
| USI_Ponder | Check | true | true/false | Enable pondering (thinking on opponent's time) |
| EngineType | Combo | Material | Material/Nnue/Enhanced/EnhancedNnue | Engine evaluation and search type |
| ByoyomiPeriods | Spin | 1 | 1-10 or 'default' | Number of byoyomi periods (USI_ByoyomiPeriods alias also supported) |

#### ByoyomiPeriods Option

Controls the number of byoyomi periods when using byoyomi time control:

```bash
# Set default number of periods (used when not specified in go command)
setoption name ByoyomiPeriods value 3
# or using the alias
setoption name USI_ByoyomiPeriods value 3

# Reset to default (1 period)
setoption name ByoyomiPeriods value default

# Override in go command
go byoyomi 30000 periods 5  # 5 periods of 30 seconds each
```

## Building

### From project root
```bash
npm run build:wasm      # Production build (optimized)
npm run build:wasm:dev  # Development build (faster)
```


## Important Notes

⚠️ **WASM files must be built before running the web application!**

The build process:
1. Compiles Rust code to WebAssembly
2. Generates JavaScript bindings and TypeScript definitions
3. Copies the generated files to `packages/web/src/wasm/`

The generated files in `packages/web/src/wasm/` are:
- Excluded from git (in .gitignore)
- Required for the web application to run
- Must be regenerated when Rust code changes

## Development Workflow

1. Make changes to Rust code
2. Run quality checks: `cargo fmt`, `cargo clippy`, `cargo test`

## Testing

```bash
# Run standard Rust tests
cargo test

# Run WASM tests in browser (requires Chrome)
wasm-pack test --chrome --headless

# Generate code coverage report (requires cargo-tarpaulin)
cargo tarpaulin --out html --lib  # Generates tarpaulin-report.html
cargo tarpaulin --out Xml  # Generates cobertura.xml for CI

# Benchmark tests (ignored by default due to execution time)
cargo test -- --ignored              # Run only ignored tests (benchmarks)
cargo test -- --include-ignored      # Run all tests including benchmarks
cargo test test_benchmark -- --ignored  # Run specific benchmark test
```

## Code Quality

### Required Checks (run automatically on pre-commit)
```bash
cargo fmt                    # Format code
cargo clippy -- -D warnings  # Lint with warnings as errors
cargo check                  # Fast type checking
```

### Additional Tools
```bash
cargo audit      # Security vulnerability scan
cargo outdated   # Check for outdated dependencies
cargo machete    # Find unused dependencies (requires installation)
```

## API Documentation

### WebRTC Module
Provides simple WebRTC functionality for peer-to-peer connections:
- Connection establishment
- Message passing
- Error handling

### Mate Search Module
Implements efficient mate search algorithms:
- Depth-limited search
- Move ordering optimization
- Performance-oriented design

### Opening Book Module
Handles opening book data in binary format:
- **Binary Format**: Compact storage of positions and moves
- **Position Hashing**: Fast lookup using FNV-1a algorithm
- **Move Encoding**: Efficient 16-bit move representation
- **SFEN Support**: Parse and convert SFEN notation
- **Database**: Currently supports 100,000+ opening positions

### NNUE Training Tools
Machine learning tools for NNUE evaluation function:
- **train_wdl_baseline**: Lightweight WDL (Win/Draw/Loss) trainer for pipeline validation
- **train_nnue**: Full NNUE trainer with HalfKP features and row-sparse updates
  - Performance metrics: loader_ratio and examples/sec monitoring
  - Cache support for faster data loading

#### 手動ベンチ（GitHub Actions）: NNUE Stream Loader Bench
- 目的: stream-cache ローダとプリフェッチの効果検証（sps / loader_ratio を比較）。
- 実行: GitHub Actions → 「NNUE Stream Loader Bench (manual)」→ Run workflow。
- 仕様: 小規模データを合成し、prefetch=0（同期）/8（非同期）で 1 epoch 実行。ジョブサマリに sps と loader_ratio を出力。
- 備考: デフォルトで gzip を使用（zstd 機能は不要）。しきい値による自動失敗は未設定（必要なら追加）。

例: ストリーミング学習（事前ロードなし）
```bash
cargo run -p tools --bin train_nnue -- \
  -i runs/data.cache.gz -e 1 -b 16384 \
  --stream-cache --prefetch-batches 8 --throughput-interval 2.0
# ログ: [throughput] mode=stream ... sps=... loader_ratio=...%
```
- **build_feature_cache**: Pre-extract HalfKP features to binary cache format
  - Eliminates SFEN parsing and feature extraction overhead
  - Variable-length record format with metadata preservation
- **JSONL Support**: Direct training from annotated game data
- **Feature extraction**: HalfKP feature generation from positions

## Performance Considerations

- Use `--release` flag for production builds
- Opening book uses memory-mapped files for efficiency
- Position hashing optimized for fast lookups
- Move encoding reduces memory footprint

## License

MIT
