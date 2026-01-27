#!/bin/bash
# perf_profile_debug.sh - debug buildでホットスポットの呼び出し元を特定
#
# 使い方:
#   ./scripts/perf_profile_debug.sh              # 1スレッド（デフォルト）
#   ./scripts/perf_profile_debug.sh --threads 8  # 8スレッドでプロファイル
#   ./scripts/perf_profile_debug.sh --movetime 5000  # 探索時間を5秒に設定

set -e

cd "$(dirname "$0")/.."

# デフォルト値（debug buildは遅いので短め）
MOVETIME=3000
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
            echo "  --movetime <ms>  探索時間 (default: 3000)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--threads <n>] [--movetime <ms>]"
            exit 1
            ;;
    esac
done

echo "=== Building debug binary ==="
cargo build -p tools --bin benchmark

echo ""
echo "=== Running perf record (${MOVETIME}ms × 4 positions, ${THREADS} threads, debug build is slower) ==="
sudo perf record -g --call-graph fp -o perf_debug.data \
    ./target/debug/benchmark --internal --threads "$THREADS" \
    --limit-type movetime --limit "$MOVETIME" --iterations 1

# 結果をファイルに保存
OUTPUT_DIR="./perf_results"
mkdir -p "$OUTPUT_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_FILE="$OUTPUT_DIR/${TIMESTAMP}_debug_${THREADS}t.txt"

{
    echo "=== Debug Build Perf Profile ==="
    echo "Timestamp: $(date -Iseconds)"
    echo "Build: debug (for symbol resolution)"
    echo "Threads: $THREADS"
    echo "Movetime: ${MOVETIME}ms"
    echo ""
    echo "=== memset/memmove callers ==="
    sudo perf report -i perf_debug.data --stdio --no-children -g caller --percent-limit 0.5 2>/dev/null | head -150
} | tee "$OUTPUT_FILE"

echo ""
echo "=== Report saved to: $OUTPUT_FILE ==="
echo "=== perf_debug.data saved for interactive analysis: sudo perf report -i perf_debug.data ==="
