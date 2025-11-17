# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a **Rust-based Shogi AI Engine** project implementing a high-performance game engine with NNUE (Efficiently Updatable Neural Network) evaluation. The project focuses on:

- Professional-grade Shogi engine with USI protocol support
- Advanced search algorithms (alpha-beta, iterative deepening, transposition tables)
- NNUE evaluation system with FMA optimizations
- Comprehensive training and analysis tools (45+ utility programs)
- 119,000+ lines of well-tested Rust code

## Repository Structure

```
packages/
└── rust-core/              # Shogi AI Engine (Rust workspace)
    ├── crates/
    │   ├── engine-core/    # Core engine implementation (152 .rs files)
    │   ├── engine-usi/     # USI protocol command-line interface
    │   └── tools/          # NNUE training/analysis tools (45+ binaries)
    ├── docs/               # Comprehensive documentation (50+ markdown files)
    └── Cargo.toml          # Workspace definition
```

## Principles

- **Code Excellence**: Follow Rust best practices and idioms
- **Type Safety**: Leverage Rust's type system for correctness
- **Performance**: Optimize for search speed and evaluation accuracy
- **Testing**: Comprehensive test coverage with unit and integration tests
- **Documentation**: Clear doc comments for all public APIs
- **"Premature optimization is the root of all evil" - Donald Knuth**: Only implement features that are immediately needed

## Development Commands

### Rust Development (packages/rust-core/)

**IMPORTANT**: All development work happens in `/home/user/shogi/packages/rust-core/`

#### Building

```bash
cd packages/rust-core

# Build all crates
cargo build

# Build with optimizations
cargo build --release

# Build specific crate
cargo build -p engine-core
cargo build -p engine-usi
cargo build -p tools
```

#### Testing

```bash
# Run all tests
cargo test

# Run tests for specific crate
cargo test -p engine-core
cargo test -p engine-usi

# Run specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture

# Run tests in release mode (faster for performance tests)
cargo test --release
```

#### Code Quality

**IMPORTANT**: After completing each coding task, ALWAYS run these checks:

```bash
# Format code (REQUIRED before commit)
cargo fmt

# Check formatting without modifying
cargo fmt --check

# Lint with Clippy (REQUIRED before commit)
cargo clippy -- -D warnings

# Fast type checking without building
cargo check

# Run all quality checks
cargo fmt && cargo clippy -- -D warnings && cargo test
```

#### Benchmarking

```bash
# Run benchmarks
cargo bench

# Run specific benchmark
cargo bench --bench search_benchmark

# Save benchmark baseline
cargo bench -- --save-baseline baseline_name
```

#### Running Tools

```bash
# Position analysis tool
cargo run --release --bin debug_position -- --sfen "SFEN_STRING" --depth 5

# NNUE benchmark
cargo run --release --bin nnue_benchmark

# NNUE network inspection
cargo run --release --bin nnue_inspect -- path/to/model.nnue

# Generate training data
cargo run --release --bin generate_nnue_training_data

# See all available tools
ls crates/tools/src/bin/
```

### USI Engine

```bash
# Build and run USI engine
cd packages/rust-core
cargo build --release -p engine-usi
./target/release/engine-usi

# Or run directly
cargo run --release -p engine-usi
```

### Root-Level Commands (Obsolete)

**NOTE**: The following npm commands are mostly obsolete as TypeScript packages were removed:

```bash
# These commands reference deleted packages and may not work:
npm run build      # Turbo build (no packages to build)
npm run dev        # No development servers
npm test           # No npm test suites
npm run lint       # Biome (not used for Rust)
npm run typecheck  # No TypeScript packages
```

## Code Quality Standards

### Rust Best Practices

1. **Error Handling**
   - Use `Result<T, E>` for fallible operations
   - Use `Option<T>` for optional values
   - Avoid `unwrap()` and `expect()` in production code
   - Prefer `?` operator for error propagation

2. **Type Safety**
   - Use newtype pattern for domain-specific types
   - Leverage enums for state machines
   - Use trait bounds to express requirements
   - Avoid excessive use of `clone()` - prefer references

3. **Performance**
   - Profile before optimizing (use `cargo bench`)
   - Use `#[inline]` judiciously for hot paths
   - Consider SIMD for performance-critical code
   - Use appropriate data structures (Vec, HashMap, etc.)

4. **Documentation**
   - Add doc comments (`///`) for all public items
   - Include examples in doc comments
   - Document panics, errors, and safety invariants
   - Keep documentation up-to-date with code

5. **Testing**
   - Write unit tests for individual functions
   - Write integration tests for module interactions
   - Use property-based testing (proptest) for complex logic
   - Test edge cases and error conditions

### Formatting & Linting

**Configuration files:**
- `rustfmt.toml` - Formatting rules
- `clippy.toml` - Linting rules

**Pre-commit requirements:**
- `cargo fmt` - Code must be formatted
- `cargo clippy -- -D warnings` - No clippy warnings allowed
- `cargo check` - Code must compile

## Engine Architecture

### Core Components

#### 1. Engine Core (`crates/engine-core/`)

**Key modules:**
- `engine/` - Engine state and UCI/USI interface
- `search/` - Alpha-beta search with advanced features:
  - Iterative deepening
  - Transposition table (TT)
  - Move ordering (PV, killer moves, MVV-LVA)
  - Root escape detection
  - Root verification
  - Win protection
  - Mate detection
- `evaluation/` - NNUE evaluation functions
  - Material evaluation
  - Positional evaluation
  - King safety
  - Piece mobility
  - Advanced NNUE with FMA optimizations
- `movegen/` - Move generation (sliding pieces, jumpers, drops)
- `shogi/` - Game board, pieces, and rules
- `usi/` - USI protocol implementation
- `simd/` - SIMD optimizations
- `time_management/` - Time allocation algorithms
- `opening_book/` - Opening book management
- `util/` - Utilities (panic handlers, logging)

**Key features:**
- Type-safe board representation
- Efficient move generation
- Transposition table with SIMD acceleration
- Configurable engine types (Material, Enhanced, NNUE variants)
- Parallel search with YBWC (Young Brothers Wait Concept)

#### 2. Engine USI (`crates/engine-usi/`)

Command-line USI protocol interface for integration with GUI applications.

#### 3. Tools (`crates/tools/`)

45+ utility programs for:
- Position analysis (`debug_position`)
- NNUE benchmarking and inspection
- Training data generation
- Tournament testing (`gauntlet`)
- Supervised learning labeling
- Performance profiling

### Engine Type Selection

**Recommended**: `EnhancedNnue` - Best combination of advanced search + NNUE evaluation

```bash
# Via USI protocol
setoption name EngineType value EnhancedNnue
```

See `packages/rust-core/docs/engine-types-guide.md` for detailed comparison.

### Compile-time Features

Enable features in Cargo.toml or via command line:

```bash
# Enable NNUE telemetry
cargo build --features nnue_telemetry

# Enable parallel search
cargo build --features ybwc

# Enable comprehensive diagnostics
cargo build --features diagnostics

# Multiple features
cargo build --features "nnue_telemetry,tt_metrics,diagnostics"
```

**Available features:**
- `nnue_telemetry` - NNUE evaluation metrics
- `nnue_fast_fma` - FMA optimization (default enabled)
- `tt_metrics` - Transposition table diagnostics
- `ybwc` - Parallel search (Young Brothers Wait Concept)
- `diagnostics` - Comprehensive logging
- `pv_debug_logs` - Principal variation debugging

## Documentation

### Rust-Core Documentation

Comprehensive documentation in `packages/rust-core/docs/`:

**Core Guides:**
- `engine-types-guide.md` - Engine selection and configuration
- `nnue-evaluation-guide.md` - NNUE implementation details
- `usi-engine-build.md` - Build and deployment instructions
- `search-algorithm.md` - Search implementation
- `opening-book.md` - Opening book format and usage

**Tools:**
- `tools/debug-position-tool.md` - Position analysis usage
- `tools/nnue-tools.md` - NNUE training and analysis
- `tools/gauntlet.md` - Tournament testing

**Development:**
- `development/testing-guide.md` - Testing strategies
- `development/optimization-guide.md` - Performance optimization
- `development/contributing.md` - Contribution guidelines

### Outdated Documentation

**WARNING**: The following documentation is outdated and describes deleted packages:

- `/home/user/shogi/README.md` - Describes 7 packages, only rust-core exists
- `/home/user/shogi/AGENTS.md` - References deleted TypeScript packages

These files describe a multi-package TypeScript/React monorepo that was removed on November 14, 2025.

## Testing Strategy

### Unit Tests

Located in `packages/rust-core/crates/engine-core/tests/`:

```bash
# Run all tests
cargo test

# Run specific test file
cargo test --test integration_test

# Run with verbose output
cargo test -- --nocapture

# Run in release mode (faster)
cargo test --release
```

### Property-Based Testing

Uses `proptest` for complex game logic:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_move_generation(board in any::<Board>()) {
        // Test that all generated moves are legal
        for mv in generate_moves(&board) {
            prop_assert!(is_legal(&board, &mv));
        }
    }
}
```

### Benchmarks

Located in `packages/rust-core/crates/engine-core/benches/`:

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench --bench search_benchmark

# Compare with baseline
cargo bench -- --baseline main
```

## Development Workflow

### Making Changes

1. **Make code changes**
   - Edit files in `packages/rust-core/crates/`
   - Follow Rust best practices

2. **Run quality checks** (REQUIRED)
   ```bash
   cd packages/rust-core
   cargo fmt
   cargo clippy -- -D warnings
   cargo test
   ```

3. **Build and test**
   ```bash
   cargo build --release
   cargo test --release
   ```

4. **Commit changes**
   - Git hooks will run cargo fmt and clippy automatically
   - Ensure all checks pass before pushing

### Debug Tools

#### Position Analysis

Use `debug_position` for investigating search issues:

```bash
cd packages/rust-core

# Analyze specific position
cargo run --release --bin debug_position -- --sfen "SFEN_STRING" --depth 5 --time 1000

# Compare engine types
cargo run --release --bin debug_position -- -s "SFEN" -e material
cargo run --release --bin debug_position -- -s "SFEN" -e enhanced_nnue

# Check move generation
cargo run --release --bin debug_position -- -s "SFEN" --moves

# Run perft (performance test)
cargo run --release --bin debug_position -- -s "SFEN" --perft 5
```

See `/packages/rust-core/docs/tools/debug-position-tool.md` for detailed usage.

### Performance Profiling

```bash
# Install cargo-flamegraph
cargo install flamegraph

# Generate flamegraph
cd packages/rust-core
cargo flamegraph --bin engine-usi

# Use perf (Linux)
cargo build --release
perf record -g ./target/release/engine-usi
perf report
```

## Build Configuration

### Optimization Profiles

**Debug profile** (`Cargo.toml`):
```toml
[profile.dev]
opt-level = 1      # Basic optimizations for reasonable test performance
panic = "unwind"   # Stack traces on panic
```

**Release profile**:
```toml
[profile.release]
opt-level = 3              # Maximum optimization
lto = "thin"               # Link-time optimization
overflow-checks = true     # Detect integer overflows
panic = "unwind"           # Stack traces even in release
```

### Cross-compilation

```bash
# Add target
rustup target add x86_64-pc-windows-gnu

# Build for Windows
cargo build --release --target x86_64-pc-windows-gnu

# Build for multiple targets
cargo build --release --target x86_64-unknown-linux-gnu
cargo build --release --target x86_64-apple-darwin
```

## Git Workflow

### Branch Strategy

- **Feature branches**: `feature/description` or `claude/session-id`
- **Main branch**: Stable releases
- **Development**: Direct commits to feature branches

### Commit Messages

Follow conventional commits:

```
feat: Add new NNUE architecture
fix: Correct move generation for promoted pieces
perf: Optimize transposition table lookup
docs: Update engine types guide
test: Add property tests for move generation
refactor: Simplify search logic
```

### Pre-commit Hooks

Configured via Husky (`.husky/pre-commit`):
- Runs `cargo fmt --check`
- Runs `cargo clippy -- -D warnings`
- Prevents commits with formatting/linting issues

## Common Tasks

### Adding a New Engine Feature

1. **Design**: Plan the feature in `docs/`
2. **Implement**: Write code in `crates/engine-core/src/`
3. **Test**: Add unit tests in `tests/` or module tests
4. **Benchmark**: Add benchmark if performance-critical
5. **Document**: Update doc comments and guides
6. **Validate**: Run full test suite and benchmarks

### Training a New NNUE Model

1. **Generate training data**:
   ```bash
   cargo run --release --bin generate_nnue_training_data
   ```

2. **Label with USI teacher**:
   ```bash
   cargo run --release --bin label_with_usi_teacher
   ```

3. **Train model**: Use external NNUE trainer

4. **Benchmark**:
   ```bash
   cargo run --release --bin nnue_benchmark -- path/to/model.nnue
   ```

5. **Integrate**: Copy model to engine resources

### Running Tournament Testing

```bash
cd packages/rust-core
cargo run --release --bin gauntlet -- --config tournament.yaml
```

## Additional Resources

### Rust Learning

- [The Rust Book](https://doc.rust-lang.org/book/)
- [Rust by Example](https://doc.rust-lang.org/rust-by-example/)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)

### Shogi Programming

- [Shogi Programming Documentation](packages/rust-core/docs/shogi-programming.md)
- [USI Protocol Specification](packages/rust-core/docs/usi-protocol.md)
- [NNUE Evaluation](packages/rust-core/docs/nnue-evaluation-guide.md)

### Project-Specific

- All documentation in `packages/rust-core/docs/`
- 50+ markdown files covering all aspects of the engine

## MCP Integration

### Available MCP Servers

Configured in `.mcp.json`:

```json
{
  "playwright": "npx @playwright/mcp@latest",  // E2E testing (legacy)
  "serena": "Docker container for IDE assistance"
}
```

**NOTE**: Playwright MCP is legacy from when web packages existed. May be removed.

## Discord Conversation Logger Rules

**DEPRECATED**: The discord-conversation-logger MCP was used for the old TypeScript project.

If still needed:

### Important Message Logging

Log in these cases:

#### 1. User Messages (human)
- Task start/change/completion instructions
- Important decisions or confirmations
- Error reports or issue identification

#### 2. Assistant Messages (assistant)
- Task completion reports
- Important suggestions or solutions
- Error resolution methods
- Summary of significant changes made

#### 3. System Messages (system)
- Critical errors or warnings
- Important environment changes
- Security-related notifications

### Logging Format

```
mcp__discord-conversation-logger__log_conversation(
  message: "Actual message content",
  role: "human" | "assistant" | "system",
  context: "Brief context description"
)
```

## Troubleshooting

### Common Issues

**Build failures:**
```bash
# Clean build
cargo clean
cargo build --release

# Update dependencies
cargo update
```

**Test failures:**
```bash
# Run tests with backtrace
RUST_BACKTRACE=1 cargo test

# Run tests in single thread (easier debugging)
cargo test -- --test-threads=1
```

**Slow tests:**
```bash
# Run tests in release mode
cargo test --release
```

**Clippy warnings:**
```bash
# Show all warnings
cargo clippy --all-targets --all-features

# Fix auto-fixable issues
cargo clippy --fix --allow-dirty
```

### Getting Help

- Check `packages/rust-core/docs/` for detailed documentation
- Review commit history for similar changes
- Consult Rust documentation and community resources

## Project Status

### Current State (as of 2025-11-17)

- **Architecture**: Rust-only engine (TypeScript packages removed Nov 14, 2025)
- **Code**: 119,000+ lines of Rust
- **Tests**: 13 test files, all passing
- **Benchmarks**: 8 criterion benchmarks
- **Tools**: 45+ analysis and training utilities
- **Documentation**: 50+ markdown files

### Recently Removed (Nov 14, 2025)

- `packages/core/` - TypeScript game engine (DELETED)
- `packages/web/` - React web application (DELETED)
- `packages/server/` - Express server (DELETED)
- `packages/discord-bot/` - Discord bot (DELETED)
- `packages/desktop/` - Tauri desktop app (DELETED)
- `packages/types/` - Shared TypeScript types (DELETED)
- `crates/engine-wasm/` - WebAssembly bindings (DELETED)
- `crates/webrtc-p2p/` - WebRTC P2P (DELETED)

### Future Directions

Focus remains on:
- Improving NNUE evaluation accuracy
- Optimizing search performance
- Adding more analysis tools
- Enhancing USI protocol support
- Tournament testing and strength evaluation

---

**This is a professional Rust-based Shogi AI engine project. All development focuses on performance, correctness, and competitive strength.**
