#!/bin/bash
# perf_profile_nnue.sh - NNUE有効時のホットスポット特定用スクリプト
# 使い方: ./scripts/perf_profile_nnue.sh

set -e

cd "$(dirname "$0")/.."

NNUE_FILE="./memo/YaneuraOu/eval/nn.bin"

if [ ! -f "$NNUE_FILE" ]; then
    echo "Error: NNUE file not found: $NNUE_FILE"
    exit 1
fi

echo "=== Building debug binary ==="
cargo build -p engine-usi

echo ""
echo "=== Running perf record with NNUE (3 seconds) ==="
# NNUEを使うにはengine-usiではなくbenchmarkツールを使う
# ただしperfで計測するにはengine-usiが必要なので、別の方法を検討

# benchmarkツールをperf経由で実行
echo "Using benchmark tool with NNUE..."
sudo perf record -g --call-graph dwarf -o perf.data \
    ./target/debug/benchmark --internal --nnue-file "$NNUE_FILE" \
    --limit-type movetime --limit 3000 --iterations 1

echo ""
echo "=== Top hotspots (NNUE enabled) ==="
sudo perf report --stdio --no-children -g caller --percent-limit 0.5 2>/dev/null | head -150

echo ""
echo "=== Full report saved to perf.data ==="
