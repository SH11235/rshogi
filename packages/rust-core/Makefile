.PHONY: build build-dev test clean check-all

# Default target
build:
	wasm-pack build --target web --out-dir pkg
	mkdir -p ../web/src/wasm
	cp pkg/* ../web/src/wasm/

build-dev:
	wasm-pack build --dev --target web --out-dir pkg
	mkdir -p ../web/src/wasm
	cp pkg/* ../web/src/wasm/

test:
	cargo test

test-wasm:
	wasm-pack test --chrome --headless

test-wasm-firefox:
	wasm-pack test --firefox --headless

format:
	cargo fmt

format-check:
	cargo fmt -- --check

lint:
	cargo clippy -- -D warnings

check:
	cargo check

# Run all quality checks
check-all: format-check lint check test
	@echo "✅ All quality checks passed!"

# Install additional development tools
install-dev-tools:
	cargo install cargo-audit
	cargo install cargo-outdated
	cargo install cargo-machete
	@echo "✅ Development tools installed!"

# Security audit
audit:
	cargo audit

# Check for outdated dependencies
outdated:
	cargo outdated

# Find unused dependencies
unused-deps:
	cargo machete

clean:
	cargo clean
	rm -rf pkg
	rm -rf ../web/src/wasm/*

# Generate opening book files
generate-openings:
	./scripts/generate-and-copy-openings.sh

# Convert opening book from JSON to binary format
convert-opening-book:
	cargo run --bin convert_opening_book -- --help

# Verify opening book binary files
verify-opening-book:
	cargo run --bin verify_opening_book -- --help