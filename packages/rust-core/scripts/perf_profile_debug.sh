#!/bin/bash
# perf_profile_debug.sh - debug buildでホットスポットの呼び出し元を特定
# 使い方: ./scripts/perf_profile_debug.sh

set -e

cd "$(dirname "$0")/.."

echo "=== Building debug binary ==="
cargo build -p engine-usi

echo ""
echo "=== Running perf record (3 seconds, debug build is slower) ==="
echo -e "isready\nposition startpos\ngo movetime 3000\nquit" | \
    sudo perf record -g --call-graph dwarf -o perf_debug.data ./target/debug/engine-usi

# 結果をファイルに保存
OUTPUT_DIR="./perf_results"
mkdir -p "$OUTPUT_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_FILE="$OUTPUT_DIR/${TIMESTAMP}_debug.txt"

{
    echo "=== Debug Build Perf Profile ==="
    echo "Timestamp: $(date -Iseconds)"
    echo "Build: debug (for symbol resolution)"
    echo ""
    echo "=== memset/memmove callers ==="
    sudo perf report -i perf_debug.data --stdio --no-children -g caller --percent-limit 0.5 2>/dev/null | head -150
} | tee "$OUTPUT_FILE"

echo ""
echo "=== Report saved to: $OUTPUT_FILE ==="
echo "=== perf_debug.data saved for interactive analysis: sudo perf report -i perf_debug.data ==="
