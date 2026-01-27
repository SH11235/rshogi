#!/bin/bash
# perf_profile.sh - ホットスポット特定用スクリプト
#
# 使い方:
#   ./scripts/perf_profile.sh              # 1スレッド（デフォルト）
#   ./scripts/perf_profile.sh --threads 8  # 8スレッドでプロファイル
#   ./scripts/perf_profile.sh --movetime 10000  # 探索時間を10秒に設定

set -e

cd "$(dirname "$0")/.."

# デフォルト値
MOVETIME=5000
THREADS=1

# 引数解析
while [[ $# -gt 0 ]]; do
    case $1 in
        --threads|-t)
            THREADS="$2"
            shift 2
            ;;
        --movetime)
            MOVETIME="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [--threads <n>] [--movetime <ms>]"
            echo ""
            echo "Options:"
            echo "  --threads <n>    スレッド数 (default: 1)"
            echo "  --movetime <ms>  探索時間 (default: 5000)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--threads <n>] [--movetime <ms>]"
            exit 1
            ;;
    esac
done

echo "=== Building release binary (with frame pointer + debug symbols) ==="
# CARGO_PROFILE_RELEASE_* で Cargo.toml の設定を上書き
RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes" \
CARGO_PROFILE_RELEASE_STRIP=false \
CARGO_PROFILE_RELEASE_DEBUG=2 \
    cargo build --release -p tools --bin benchmark

echo ""
echo "=== Running perf record (${MOVETIME}ms × 4 positions, ${THREADS} threads) ==="
sudo perf record -g --call-graph fp -o perf_release.data \
    ./target/release/benchmark --internal --threads "$THREADS" \
    --limit-type movetime --limit "$MOVETIME" --iterations 1

# 結果をファイルに保存
OUTPUT_DIR="./perf_results"
mkdir -p "$OUTPUT_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_FILE="$OUTPUT_DIR/${TIMESTAMP}_release_${THREADS}t.txt"

{
    echo "=== Release Build Perf Profile ==="
    echo "Timestamp: $(date -Iseconds)"
    echo "Build: release + frame pointers + debug symbols"
    echo "Threads: $THREADS"
    echo "Movetime: ${MOVETIME}ms"
    echo ""
    echo "=== Top hotspots ==="
    sudo perf report -i perf_release.data --stdio --no-children -g none --percent-limit 0.3 | head -120
} | tee "$OUTPUT_FILE"

echo ""
echo "=== Report saved to: $OUTPUT_FILE ==="
echo "=== perf_release.data saved for interactive analysis: sudo perf report -i perf_release.data ==="
