#!/bin/bash
# perf_profile.sh - ホットスポット特定用スクリプト
# 使い方: ./scripts/perf_profile.sh

set -e

cd "$(dirname "$0")/.."

echo "=== Building release binary (with frame pointer + debug symbols) ==="
# CARGO_PROFILE_RELEASE_* で Cargo.toml の設定を上書き
RUSTFLAGS="-C force-frame-pointers=yes" \
CARGO_PROFILE_RELEASE_STRIP=false \
CARGO_PROFILE_RELEASE_DEBUG=2 \
    cargo build --release -p engine-usi

echo ""
echo "=== Running perf record (5 seconds) ==="
echo -e "isready\nposition startpos\ngo movetime 5000\nquit" | \
    sudo perf record -g --call-graph fp -o perf_release.data ./target/release/engine-usi

# 結果をファイルに保存
OUTPUT_DIR="./perf_results"
mkdir -p "$OUTPUT_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_FILE="$OUTPUT_DIR/${TIMESTAMP}_release.txt"

{
    echo "=== Release Build Perf Profile ==="
    echo "Timestamp: $(date -Iseconds)"
    echo "Build: release + frame pointers + debug symbols"
    echo ""
    echo "=== Top hotspots ==="
    sudo perf report -i perf_release.data --stdio --no-children -g none --percent-limit 0.3 | head -120
} | tee "$OUTPUT_FILE"

echo ""
echo "=== Report saved to: $OUTPUT_FILE ==="
echo "=== perf_release.data saved for interactive analysis: sudo perf report -i perf_release.data ==="
