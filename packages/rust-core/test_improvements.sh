#!/bin/bash
# Test script to verify MoveGen improvements

set -euo pipefail

echo "=== Testing MoveGen Improvements ==="
echo ""

# Test 1: Case-insensitive MOVEGEN_TRACE
echo "Test 1: Case-insensitive MOVEGEN_TRACE"
echo -e "position startpos\ngo depth 1" | \
    SKIP_LEGAL_MOVES=0 MOVEGEN_TRACE=KING,Drops,checkers_PINS \
    timeout 2 ./target/release/engine-cli 2>&1 | grep "phase=" | head -5 || true

echo ""

# Test 2: MOVEGEN_TRACE=all
echo "Test 2: MOVEGEN_TRACE=all"
echo -e "position startpos\ngo depth 1" | \
    SKIP_LEGAL_MOVES=0 MOVEGEN_TRACE=all \
    timeout 2 ./target/release/engine-cli 2>&1 | grep "phase=" | wc -l || true

echo ""

# Test 3: Phase disabling
echo "Test 3: Phase disabling (MOVEGEN_DISABLE_KING=1)"
echo -e "position startpos\ngo depth 1" | \
    SKIP_LEGAL_MOVES=0 MOVEGEN_DISABLE_KING=1 MOVEGEN_TRACE=king \
    timeout 2 ./target/release/engine-cli 2>&1 | grep "phase=king" | wc -l || true

echo ""

# Test 4: Early exit trace
echo "Test 4: Early exit trace (USE_ANY_LEGAL=1)"
echo -e "position startpos\ngo depth 1" | \
    SKIP_LEGAL_MOVES=0 USE_ANY_LEGAL=1 MOVEGEN_TRACE=early_exit \
    timeout 2 ./target/release/engine-cli 2>&1 | grep "phase=early_exit" || echo "No early exit (might be normal)"

echo ""
echo "Tests complete!"