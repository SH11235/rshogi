#!/bin/bash
# perf_all.sh - 全パフォーマンス計測をまとめて実行
#
# 使い方:
#   ./scripts/perf_all.sh
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

NNUE_FILE="./memo/YaneuraOu/eval/nn.bin"

# NNUEファイルの存在確認
if [ ! -f "$NNUE_FILE" ]; then
    echo "警告: NNUEファイルが見つかりません: $NNUE_FILE"
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
    ./scripts/perf_profile_nnue.sh --movetime 5000
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
echo "ドキュメント更新: Claude Codeで /update-rust-perf-docs を実行してください"
