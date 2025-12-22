#!/bin/bash
# SearchWorker再利用の効果を確認するためのperfスクリプト

set -e

cd "$(dirname "$0")/.."

echo "=== Building release binary with debug symbols ==="
# CARGO_PROFILE_RELEASE_* で Cargo.toml の設定を上書き
RUSTFLAGS="-C force-frame-pointers=yes" \
CARGO_PROFILE_RELEASE_STRIP=false \
CARGO_PROFILE_RELEASE_DEBUG=2 \
    cargo build --release -p tools

echo ""
echo "=== Running perf record with --reuse-search (iterations=4) ==="
sudo perf record -g --call-graph fp -F 999 -o perf_reuse.data -- \
    ./target/release/benchmark \
    --internal --reuse-search --iterations 4 --limit-type movetime --limit 5000

# 結果をファイルに保存
OUTPUT_DIR="./perf_results"
mkdir -p "$OUTPUT_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_FILE="$OUTPUT_DIR/${TIMESTAMP}_reuse_search.txt"

{
    echo "=== Reuse Search Perf Profile ==="
    echo "Timestamp: $(date -Iseconds)"
    echo "Build: release + debug symbols"
    echo "Options: --reuse-search --iterations 4"
    echo ""
    echo "=== memset/memmove analysis ==="
    sudo perf report -i perf_reuse.data --stdio --call-graph=graph --symbol-filter=__memset | head -100
    echo ""
    echo "=== Top hotspots ==="
    sudo perf report -i perf_reuse.data --stdio | head -80
} | tee "$OUTPUT_FILE"

echo ""
echo "=== Report saved to: $OUTPUT_FILE ==="
echo "=== perf_reuse.data saved for interactive analysis: sudo perf report -i perf_reuse.data ==="
