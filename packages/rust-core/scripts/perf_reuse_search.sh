#!/bin/bash
# SearchWorker再利用の効果を確認するためのperfスクリプト

set -e

cd "$(dirname "$0")/.."

echo "=== Building release binary with debug symbols ==="
RUSTFLAGS="-C force-frame-pointers=yes -C debuginfo=2" cargo build --release -p tools

echo ""
echo "=== Running perf record with --reuse-search (iterations=4) ==="
sudo perf record -g --call-graph dwarf -F 999 -- \
    ./target/release/benchmark \
    --internal --reuse-search --iterations 4 --limit-type movetime --limit 5000

echo ""
echo "=== memset/memmove analysis ==="
sudo perf report --stdio --call-graph=graph --symbol-filter=__memset | head -100

echo ""
echo "=== Top hotspots ==="
sudo perf report --stdio | head -80

echo ""
echo "=== Full report saved to perf.data ==="
