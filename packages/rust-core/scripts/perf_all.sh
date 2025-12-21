#!/bin/bash
# perf_all.sh - 全パフォーマンス計測をまとめて実行
#
# 使い方:
#   ./scripts/perf_all.sh
#   ./scripts/perf_all.sh --nnue-file /path/to/nn.bin
#
# 注意: 内部でsudoを使用するため、パスワード入力が必要です
#
# 実行される計測:
#   1. perf_profile_nnue.sh - NNUE有効時のホットスポット
#   2. perf_profile.sh      - Material評価時のホットスポット
#   3. benchmark (NNUE)     - NNUE有効時のNPS
#   4. benchmark (Material) - Material評価時のNPS

set -e

cd "$(dirname "$0")/.."

SCRIPT_DIR="$(dirname "$0")"
CONF_FILE="$SCRIPT_DIR/perf.conf"
CONF_EXAMPLE="$SCRIPT_DIR/perf.conf.example"

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
        --nnue-file)
            NNUE_FILE="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [--nnue-file <path>]"
            echo ""
            echo "Options:"
            echo "  --nnue-file <path>  NNUEファイルのパス (default: perf.confの設定値)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--nnue-file <path>]"
            exit 1
            ;;
    esac
done

echo "=============================================="
echo "  パフォーマンス計測スクリプト"
echo "=============================================="
echo ""
echo "注意: このスクリプトは内部でsudoを使用します"
echo "      パスワード入力が求められる場合があります"
echo ""
echo "実行される計測:"
echo "  1. perf (NNUE有効)    - ホットスポット分析"
echo "  2. perf (Material)    - ホットスポット分析"
echo "  3. benchmark (NNUE)   - NPS計測"
echo "  4. benchmark (Material) - NPS計測"
echo ""
read -p "続行しますか? [y/N] " -n 1 -r
echo ""

if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "キャンセルしました"
    exit 0
fi

# 結果ディレクトリを事前作成
mkdir -p ./perf_results
mkdir -p ./benchmark_results

# NNUEファイルの存在確認（読み取り権限もチェック）
if [ ! -f "$NNUE_FILE" ] || [ ! -r "$NNUE_FILE" ]; then
    echo "警告: NNUEファイルが見つからないか読み取れません: $NNUE_FILE"
    echo "NNUE関連の計測はスキップされます"
    SKIP_NNUE=true
else
    SKIP_NNUE=false
fi

echo ""
echo "=============================================="
echo "  1/4: perf (NNUE有効)"
echo "=============================================="
if [ "$SKIP_NNUE" = false ]; then
    ./scripts/perf_profile_nnue.sh --movetime 5000 --nnue-file "$NNUE_FILE"
else
    echo "スキップ: NNUEファイルがありません"
fi

echo ""
echo "=============================================="
echo "  2/4: perf (Material評価)"
echo "=============================================="
./scripts/perf_profile.sh

echo ""
echo "=============================================="
echo "  3/4: benchmark (NNUE有効)"
echo "=============================================="
if [ "$SKIP_NNUE" = false ]; then
    RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
        --internal --threads 1 --limit-type movetime --limit 20000 \
        --nnue-file "$NNUE_FILE" \
        --output-dir ./benchmark_results
else
    echo "スキップ: NNUEファイルがありません"
fi

echo ""
echo "=============================================="
echo "  4/4: benchmark (Material評価)"
echo "=============================================="
RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
    --internal --threads 1 --limit-type movetime --limit 20000 \
    --output-dir ./benchmark_results

echo ""
echo "=============================================="
echo "  計測完了"
echo "=============================================="
echo ""
echo "結果ファイル:"
echo "  perf結果:      ./perf_results/"
ls -1t ./perf_results/ | head -4 | sed 's/^/    /'
echo ""
echo "  benchmark結果: ./benchmark_results/"
ls -1t ./benchmark_results/ | head -4 | sed 's/^/    /'
echo ""

echo "perfレポートの生成中..."
sudo perf report -i perf_nnue.data --stdio --no-children --percent-limit 0.5 \
> perf_results/nnue_flat.txt
echo "  -> perf_results/nnue_flat.txt"

echo ""
echo "コールグラフレポートの生成中..."
sudo perf report -i perf_nnue.data --stdio -g caller --percent-limit 0.5 \
    > perf_results/nnue_callers.txt
echo "  -> perf_results/nnue_callers.txt"


echo "ドキュメント更新: Claude Codeで /update-rust-perf-docs を実行してください"
