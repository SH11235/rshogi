#!/bin/bash
# SearchWorker再利用の効果を確認するためのperfスクリプト
#
# 使い方:
#   ./scripts/perf_reuse_search.sh              # 1スレッド（デフォルト）
#   ./scripts/perf_reuse_search.sh --threads 8  # 8スレッドでプロファイル
#   ./scripts/perf_reuse_search.sh --movetime 10000  # 探索時間を10秒に設定

set -e

cd "$(dirname "$0")/.."

# デフォルト値
MOVETIME=5000
THREADS=1
ITERATIONS=4

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
        --iterations)
            ITERATIONS="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [--threads <n>] [--movetime <ms>] [--iterations <n>]"
            echo ""
            echo "Options:"
            echo "  --threads <n>     スレッド数 (default: 1)"
            echo "  --movetime <ms>   探索時間 (default: 5000)"
            echo "  --iterations <n>  繰り返し回数 (default: 4)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--threads <n>] [--movetime <ms>] [--iterations <n>]"
            exit 1
            ;;
    esac
done

echo "=== Building release binary with debug symbols ==="
# CARGO_PROFILE_RELEASE_* で Cargo.toml の設定を上書き
RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes" \
CARGO_PROFILE_RELEASE_STRIP=false \
CARGO_PROFILE_RELEASE_DEBUG=2 \
    cargo build --release -p tools --bin benchmark

echo ""
echo "=== Running perf record with --reuse-search (${MOVETIME}ms × 4 positions × ${ITERATIONS} iterations, ${THREADS} threads) ==="
sudo perf record -g --call-graph fp -F 999 -o perf_reuse.data -- \
    ./target/release/benchmark \
    --internal --reuse-search --iterations "$ITERATIONS" \
    --limit-type movetime --limit "$MOVETIME" --threads "$THREADS"

# 結果をファイルに保存
OUTPUT_DIR="./perf_results"
mkdir -p "$OUTPUT_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_FILE="$OUTPUT_DIR/${TIMESTAMP}_reuse_search_${THREADS}t.txt"

{
    echo "=== Reuse Search Perf Profile ==="
    echo "Timestamp: $(date -Iseconds)"
    echo "Build: release + debug symbols"
    echo "Threads: $THREADS"
    echo "Movetime: ${MOVETIME}ms"
    echo "Options: --reuse-search --iterations $ITERATIONS"
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
