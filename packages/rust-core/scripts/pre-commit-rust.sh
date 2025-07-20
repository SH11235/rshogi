#!/bin/bash

# Script to run Rust code quality checks for pre-commit hook
# This script should be run from the rust-core directory

set -e

echo "🦀 Running Rust pre-commit checks..."

# Change to rust-core directory if not already there
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
cd "$SCRIPT_DIR/.."

# 1. Check if cargo is available
if ! command -v cargo &> /dev/null; then
    echo "❌ cargo is not installed. Please install Rust."
    exit 1
fi

# 2. Run cargo fmt check (non-destructive check)
echo "📝 Checking Rust formatting..."

# Format each crate individually
CRATES=("crates/engine-core" "crates/engine-wasm" "crates/tools" "crates/webrtc-p2p")
for crate_dir in "${CRATES[@]}"; do
    if [ -d "$crate_dir" ]; then
        echo "  Formatting $crate_dir..."
        if ! cargo fmt --manifest-path "$crate_dir/Cargo.toml" -- --check; then
            echo "❌ Rust code in $crate_dir is not formatted. Run 'cargo fmt --manifest-path $crate_dir/Cargo.toml' to fix."
            exit 1
        fi
    fi
done

# 3. Run cargo clippy
echo "🔍 Running Clippy lints..."
for crate_dir in "${CRATES[@]}"; do
    if [ -d "$crate_dir" ]; then
        echo "  Clippy check for $crate_dir..."
        if ! cargo clippy --manifest-path "$crate_dir/Cargo.toml" -- -D warnings; then
            echo "❌ Clippy found issues in $crate_dir. Please fix them before committing."
            exit 1
        fi
    fi
done

# 4. Run cargo check (fast type checking)
echo "🔍 Running cargo check..."
for crate_dir in "${CRATES[@]}"; do
    if [ -d "$crate_dir" ]; then
        echo "  Type checking $crate_dir..."
        if ! cargo check --manifest-path "$crate_dir/Cargo.toml"; then
            echo "❌ Cargo check failed for $crate_dir. Please fix compilation errors."
            exit 1
        fi
    fi
done

# 5. Run tests (optional - can be commented out if too slow)
# echo "🧪 Running Rust tests..."
# if ! cargo test; then
#     echo "❌ Tests failed. Please fix them before committing."
#     exit 1
# fi

echo "✅ Rust pre-commit checks passed!"
