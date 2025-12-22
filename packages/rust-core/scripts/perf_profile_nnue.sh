#!/bin/bash
# perf_profile_nnue.sh - NNUE有効時のホットスポット特定用スクリプト
#
# 使い方:
#   ./scripts/perf_profile_nnue.sh          # release build（推奨）
#   ./scripts/perf_profile_nnue.sh --debug  # debug build（シンボル詳細）
#   ./scripts/perf_profile_nnue.sh --movetime 10000  # movetimeを10秒に設定
#   ./scripts/perf_profile_nnue.sh --nnue-file /path/to/nn.bin  # NNUEファイル指定
#
# release buildでもシンボル情報を保持するため、frame-pointersを有効化

set -e

cd "$(dirname "$0")/.."

SCRIPT_DIR="$(dirname "$0")"
CONF_FILE="$SCRIPT_DIR/perf.conf"
CONF_EXAMPLE="$SCRIPT_DIR/perf.conf.example"

# デフォルト値（NNUE_FILE以外）
BUILD_MODE="release"
MOVETIME=5000

# 設定ファイルの読み込み
if [ ! -f "$CONF_FILE" ]; then
    echo "設定ファイルが見つかりません。exampleからコピーします..."
    cp "$CONF_EXAMPLE" "$CONF_FILE"
    echo "作成しました: $CONF_FILE"
    echo ""
    echo "エラー: 設定ファイルを編集してください"
    echo "       vim scripts/perf.conf"
    echo "       NNUE_FILE のパスを環境に合わせて設定してください"
    exit 1
fi

source "$CONF_FILE"

# exampleのデフォルト値のままかチェック
if [ "$NNUE_FILE" = "./path/to/nn.bin" ]; then
    echo "エラー: NNUE_FILE が未設定です"
    echo "       scripts/perf.conf を編集して、正しいパスを設定してください"
    exit 1
fi

# 引数解析（設定ファイルの値をオーバーライド可能）
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
        --nnue-file)
            NNUE_FILE="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [--debug] [--movetime <ms>] [--nnue-file <path>]"
            echo ""
            echo "Options:"
            echo "  --debug            debug buildでプロファイリング"
            echo "  --movetime <ms>    探索時間 (default: 5000)"
            echo "  --nnue-file <path> NNUEファイルのパス (default: perf.confの設定値)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--debug] [--movetime <ms>] [--nnue-file <path>]"
            exit 1
            ;;
    esac
done

# NNUEファイルの存在確認（読み取り権限もチェック）
if [ ! -f "$NNUE_FILE" ] || [ ! -r "$NNUE_FILE" ]; then
    echo "Error: NNUE file not found or not readable: $NNUE_FILE"
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
sudo perf record -g --call-graph fp -o perf_nnue.data \
    "$BINARY" --internal --nnue-file "$NNUE_FILE" \
    --limit-type movetime --limit "$MOVETIME" --iterations 1

# 結果をファイルに保存
OUTPUT_DIR="./perf_results"
mkdir -p "$OUTPUT_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_FILE="$OUTPUT_DIR/${TIMESTAMP}_nnue_${BUILD_MODE}.txt"

{
    echo "=== NNUE Perf Profile ==="
    echo "Timestamp: $(date -Iseconds)"
    echo "Build mode: $BUILD_MODE"
    echo "Movetime: ${MOVETIME}ms"
    echo "NNUE file: $NNUE_FILE"
    echo ""
    echo "=== Top hotspots (NNUE enabled, $BUILD_MODE build) ==="
    sudo perf report -i perf_nnue.data --stdio --no-children -g caller --percent-limit 0.5 2>/dev/null | head -200
} | tee "$OUTPUT_FILE"

echo ""
echo "=== Report saved to: $OUTPUT_FILE ==="
echo "=== perf_nnue.data saved for interactive analysis: sudo perf report -i perf_nnue.data ==="
