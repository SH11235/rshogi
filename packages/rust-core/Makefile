.PHONY: build build-dev test clean

# Default target
build:
	wasm-pack build --target web --out-dir pkg
	cp pkg/* ../web/src/wasm/

build-dev:
	wasm-pack build --dev --target web --out-dir pkg
	cp pkg/* ../web/src/wasm/

test:
	cargo test

test-wasm:
	wasm-pack test --chrome --headless

test-wasm-firefox:
	wasm-pack test --firefox --headless

format:
	cargo fmt

lint:
	cargo clippy

clean:
	cargo clean
	rm -rf pkg
	rm -rf ../web/src/wasm/*