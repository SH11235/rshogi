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
if ! cargo fmt -- --check; then
    echo "❌ Rust code is not formatted. Run 'cargo fmt' to fix."
    exit 1
fi

# 3. Run cargo clippy
echo "🔍 Running Clippy lints..."
if ! cargo clippy -- -D warnings; then
    echo "❌ Clippy found issues. Please fix them before committing."
    exit 1
fi

# 4. Run cargo check (fast type checking)
echo "🔍 Running cargo check..."
if ! cargo check; then
    echo "❌ Cargo check failed. Please fix compilation errors."
    exit 1
fi

# 5. Run tests (optional - can be commented out if too slow)
# echo "🧪 Running Rust tests..."
# if ! cargo test; then
#     echo "❌ Tests failed. Please fix them before committing."
#     exit 1
# fi

echo "✅ Rust pre-commit checks passed!"