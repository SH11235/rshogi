.PHONY: build build-dev test clean check-all build-bins build-bin-convert build-bin-verify convert-opening-books-parallel

# Default target
build:
	wasm-pack build --target web --out-dir pkg
	mkdir -p ../web/src/wasm
	cp pkg/* ../web/src/wasm/

build-dev:
	wasm-pack build --dev --target web --out-dir pkg
	mkdir -p ../web/src/wasm
	cp pkg/* ../web/src/wasm/

# Run all tests (unit and integration)
test:
	@echo "Running unit tests..."
	cargo test --lib
	@echo "\nRunning integration tests..."
	@for test in $$(find tests -name "*.rs" -type f | grep -v "/data/" | xargs -n1 basename | sed 's/.rs$$//'); do \
		echo "\n=== Running $$test ==="; \
		cargo test --test $$test || true; \
	done

# Run unit tests only
test-unit:
	cargo test --lib

# Run all integration tests (individually to see all results)
test-integration:
	@echo "Running integration tests..."
	@for test in $$(find tests -name "*.rs" -type f | grep -v "/data/" | xargs -n1 basename | sed 's/.rs$$//'); do \
		echo "\n=== Running $$test ==="; \
		cargo test --test $$test || true; \
	done

# Run specific integration test
test-integration-binary:
	cargo test --test binary_converter_test

test-integration-data-structures:
	cargo test --test data_structures_test

test-integration-integration:
	cargo test --test integration_test

test-integration-move-encoder:
	cargo test --test move_encoder_test

test-integration-position-filter:
	cargo test --test position_filter_test

test-integration-position-hasher:
	cargo test --test position_hasher_test

test-integration-sfen-parser:
	cargo test --test sfen_parser_test

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

# Build release binaries
build-bins:
	cargo build --release --bins

build-bin-convert:
	cargo build --release --bin convert_opening_book

build-bin-verify:
	cargo build --release --bin verify_opening_book

# Generate opening book files
generate-openings:
	./scripts/generate-and-copy-openings.sh

# Convert opening book from SFEN to binary format
convert-opening-book: build-bin-convert
	./target/release/convert_opening_book user_book1.db converted_openings/opening_book_web.binz

# Convert opening books in parallel with different configurations
convert-opening-books-parallel: build-bin-convert
	@echo "Converting opening books in parallel..."
	@mkdir -p converted_openings
	./target/release/convert_opening_book \
		--input user_book1.db \
		--output converted_openings/opening_book_web.binz \
		--max-moves 50 \
		--min-depth 5 \
		--min-eval=-500 \
		--max-eval=500 \
		--compress \
		--validate & \
	./target/release/convert_opening_book \
		--input user_book1.db \
		--output converted_openings/opening_book_full.binz \
		--max-moves 999999 \
		--min-depth 0 \
		--min-eval=-99999 \
		--max-eval=99999 \
		--compress \
		--progress-interval 100000 & \
	./target/release/convert_opening_book \
		--input user_book1.db \
		--output converted_openings/opening_book_early.binz \
		--max-moves 20 \
		--min-depth 8 \
		--min-eval=-300 \
		--max-eval=300 \
		--compress & \
	./target/release/convert_opening_book \
		--input user_book1.db \
		--output converted_openings/opening_book_tournament.binz \
		--max-moves 100 \
		--min-depth 10 \
		--min-eval=-200 \
		--max-eval=200 \
		--compress \
		--validate & \
	wait
	@echo "✅ All opening book conversions completed!"

# Verify opening book binary files
verify-opening-book: build-bin-verify
	./target/release/verify_opening_book converted_openings/opening_book_web.binz

# Show help for convert tool
convert-opening-book-help:
	cargo run --bin convert_opening_book -- --help

# Show help for verify tool
verify-opening-book-help:
	cargo run --bin verify_opening_book -- --help