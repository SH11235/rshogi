#!/bin/bash
# perf_profile.sh - ホットスポット特定用スクリプト
# 使い方: ./scripts/perf_profile.sh

set -e

cd "$(dirname "$0")/.."

echo "=== Building release binary (with frame pointer + debug symbols) ==="
RUSTFLAGS="-C force-frame-pointers=yes -C debuginfo=2" cargo build --release -p engine-usi

echo ""
echo "=== Running perf record (5 seconds) ==="
echo -e "isready\nposition startpos\ngo movetime 5000\nquit" | \
    sudo perf record -g --call-graph fp -o perf.data ./target/release/engine-usi

echo ""
echo "=== Top 50 hotspots ==="
sudo perf report --stdio --no-children -g none --percent-limit 0.3 | head -120

echo ""
echo "=== Full report saved to perf.data ==="
echo "Run 'sudo perf report' for interactive view"
