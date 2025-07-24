.PHONY: build build-dev test test-all test-large-stack clean check-all format format-check lint check audit outdated unused-deps install-dev-tools

# Default WASM build (legacy support - builds src/)
build:
	wasm-pack build --target web --out-dir pkg
	mkdir -p ../web/src/wasm
	cp pkg/* ../web/src/wasm/

# Development WASM build (legacy support - builds src/)
build-dev:
	wasm-pack build --dev --target web --out-dir pkg
	mkdir -p ../web/src/wasm
	cp pkg/* ../web/src/wasm/

# Run all tests in workspace
test:
	@echo "Running all workspace tests..."
	cargo test --workspace

# Run all tests including large stack tests
test-all:
	@echo "Running all tests including large stack tests..."
	RUST_MIN_STACK=8388608 cargo test --workspace -- --include-ignored

# Run only large stack tests
test-large-stack:
	@echo "Running large stack tests..."
	RUST_MIN_STACK=8388608 cargo test --workspace -- --ignored

# Run tests in release mode (recommended for CI)
test-release:
	@echo "Running tests in release mode..."
	cargo test --workspace --release

# Format code
format:
	cargo fmt --all

# Check formatting
format-check:
	cargo fmt --all -- --check

# Run clippy linter
lint:
	cargo clippy --workspace --all-targets -- -D warnings

# Check code without building
check:
	cargo check --workspace --all-targets

# Run all quality checks
check-all: format-check lint check test-release
	@echo "✅ All quality checks passed!"

# Clean build artifacts
clean:
	cargo clean
	rm -rf pkg
	rm -rf ../web/src/wasm/*

# Security audit
audit:
	cargo audit

# Check for outdated dependencies
outdated:
	cargo outdated

# Find unused dependencies
unused-deps:
	cargo machete

# Install additional development tools
install-dev-tools:
	cargo install cargo-audit
	cargo install cargo-outdated
	cargo install cargo-machete
	cargo install wasm-pack
	@echo "✅ Development tools installed!"

# --- Engine/USI specific targets ---

# Build USI engine
build-engine:
	cargo build --release -p engine-cli

# Run USI engine
run-engine:
	cargo run --release -p engine-cli

# --- Tools specific targets ---

# Build all tools
build-tools:
	cargo build --release -p tools

# Convert opening book
convert-opening-book:
	cargo run --release -p tools --bin convert_opening_book -- $(ARGS)

# Verify opening book
verify-opening-book:
	cargo run --release -p tools --bin verify_opening_book -- $(ARGS)

# Run NNUE benchmark
nnue-benchmark:
	cargo run --release -p tools --bin nnue_benchmark

# Run shogi benchmark
shogi-benchmark:
	cargo run --release -p tools --bin shogi_benchmark

# --- Help ---
help:
	@echo "Available targets:"
	@echo "  build          - Build WASM (legacy src/)"
	@echo "  build-dev      - Build WASM in dev mode (legacy src/)"
	@echo "  test           - Run all workspace tests"
	@echo "  test-release   - Run tests in release mode (recommended)"
	@echo "  test-all       - Run all tests including ignored ones"
	@echo "  test-large-stack - Run only large stack tests"
	@echo "  format         - Format code"
	@echo "  format-check   - Check code formatting"
	@echo "  lint           - Run clippy linter"
	@echo "  check          - Check code without building"
	@echo "  check-all      - Run all quality checks"
	@echo "  clean          - Clean build artifacts"
	@echo "  audit          - Run security audit"
	@echo "  outdated       - Check for outdated dependencies"
	@echo "  unused-deps    - Find unused dependencies"
	@echo "  install-dev-tools - Install development tools"
	@echo ""
	@echo "Engine targets:"
	@echo "  build-engine   - Build USI engine"
	@echo "  run-engine     - Run USI engine"
	@echo ""
	@echo "Tools targets:"
	@echo "  build-tools    - Build all tools"
	@echo "  convert-opening-book ARGS=<args> - Convert opening book"
	@echo "  verify-opening-book ARGS=<args>  - Verify opening book"
	@echo "  nnue-benchmark - Run NNUE benchmark"
	@echo "  shogi-benchmark - Run shogi benchmark"