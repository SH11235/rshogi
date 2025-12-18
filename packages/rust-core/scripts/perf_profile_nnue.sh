#!/bin/bash
# perf_profile_nnue.sh - NNUE有効時のホットスポット特定用スクリプト
#
# 使い方:
#   ./scripts/perf_profile_nnue.sh          # release build（推奨）
#   ./scripts/perf_profile_nnue.sh --debug  # debug build（シンボル詳細）
#   ./scripts/perf_profile_nnue.sh --movetime 10000  # movetimeを10秒に設定
#
# release buildでもシンボル情報を保持するため、frame-pointersを有効化

set -e

cd "$(dirname "$0")/.."

NNUE_FILE="./memo/YaneuraOu/eval/nn.bin"
BUILD_MODE="release"
MOVETIME=5000

# 引数解析
while [[ $# -gt 0 ]]; do
    case $1 in
        --debug)
            BUILD_MODE="debug"
            shift
            ;;
        --movetime)
            MOVETIME="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--debug] [--movetime <ms>]"
            exit 1
            ;;
    esac
done

if [ ! -f "$NNUE_FILE" ]; then
    echo "Error: NNUE file not found: $NNUE_FILE"
    exit 1
fi

if [ "$BUILD_MODE" = "release" ]; then
    echo "=== Building release binary (with symbols) ==="
    # release buildでもシンボル情報を保持
    # - target-cpu=native: SIMD最適化を有効化
    # - force-frame-pointers=yes: perfでコールスタックを取得可能に
    # - CARGO_PROFILE_RELEASE_STRIP=false: .cargo/config.tomlのstrip設定を上書き
    # - CARGO_PROFILE_RELEASE_DEBUG=2: デバッグシンボルを含める
    RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes" \
    CARGO_PROFILE_RELEASE_STRIP=false \
    CARGO_PROFILE_RELEASE_DEBUG=2 \
        cargo build -p tools --bin benchmark --release
    BINARY="./target/release/benchmark"
else
    echo "=== Building debug binary ==="
    cargo build -p tools --bin benchmark
    BINARY="./target/debug/benchmark"
fi

echo ""
echo "=== Running perf record with NNUE (${MOVETIME}ms × 4 positions) ==="
echo "Build mode: $BUILD_MODE"
echo "Binary: $BINARY"

# benchmarkツールをperf経由で実行
sudo perf record -g --call-graph dwarf -o perf.data \
    "$BINARY" --internal --nnue-file "$NNUE_FILE" \
    --limit-type movetime --limit "$MOVETIME" --iterations 1

echo ""
echo "=== Top hotspots (NNUE enabled, $BUILD_MODE build) ==="
sudo perf report --stdio --no-children -g caller --percent-limit 0.5 2>/dev/null | head -200

echo ""
echo "=== Full report saved to perf.data ==="
echo ""
echo "Interactive analysis: sudo perf report"
