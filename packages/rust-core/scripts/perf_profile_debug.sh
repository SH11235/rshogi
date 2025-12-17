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
    sudo perf record -g --call-graph dwarf -o perf.data ./target/debug/engine-usi

echo ""
echo "=== memset/memmove callers ==="
sudo perf report --stdio --no-children -g caller --percent-limit 0.5 2>/dev/null | head -150

echo ""
echo "=== Full report saved to perf.data ==="
