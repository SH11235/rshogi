#!/bin/bash
# perf_all.sh - 全パフォーマンス計測をまとめて実行
#
# 使い方:
#   ./scripts/perf_all.sh
#   ./scripts/perf_all.sh --nnue-file /path/to/nn.bin
#   ./scripts/perf_all.sh --perf-stat
#   ./scripts/perf_all.sh --nnue-file /path/to/nn.bin --perf-stat
#   ./scripts/perf_all.sh --threads 1,8   # スレッド数を指定
#
# 注意: 内部でsudoを使用するため、パスワード入力が必要です
#
# 実行される計測:
#   1. perf_profile_nnue.sh - NNUE有効時のホットスポット
#   2. perf_profile.sh      - Material評価時のホットスポット
#   3. perf stat (NNUE)     - NNUE有効時のperf stat (--perf-stat指定時のみ)
#   4. perf stat (Material) - Material評価時のperf stat (--perf-stat指定時のみ)
#   5. benchmark (NNUE)     - NNUE有効時のNPS（1T/8T + 並列効率）
#   6. benchmark (Material) - Material評価時のNPS（1T/8T + 並列効率）

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

# 実行フラグ
RUN_PERF_STAT=false
THREADS="1,8"
BENCH_TIME=20000

# 引数解析（設定ファイルの値をオーバーライド可能）
while [[ $# -gt 0 ]]; do
    case $1 in
        --nnue-file)
            NNUE_FILE="$2"
            shift 2
            ;;
        --perf-stat)
            RUN_PERF_STAT=true
            shift
            ;;
        --threads)
            THREADS="$2"
            shift 2
            ;;
        --bench-time)
            BENCH_TIME="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [--nnue-file <path>] [--perf-stat] [--threads <list>]"
            echo ""
            echo "Options:"
            echo "  --nnue-file <path>  NNUEファイルのパス (default: perf.confの設定値)"
            echo "  --perf-stat         perf stat を実行する (default: off)"
            echo "  --threads <list>    ベンチマーク用スレッド数 (default: 1,8)"
            echo "  --bench-time <ms>   ベンチマーク探索時間 (default: 20000)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--nnue-file <path>] [--perf-stat] [--threads <list>]"
            exit 1
            ;;
    esac
done

# スレッドリストをパース
IFS=',' read -r -a THREAD_LIST <<< "$THREADS"

echo "=============================================="
echo "  パフォーマンス計測スクリプト"
echo "=============================================="
echo ""
echo "注意: このスクリプトは内部でsudoを使用します"
echo "      パスワード入力が求められる場合があります"
echo ""
echo "設定:"
echo "  スレッド: $THREADS"
echo "  探索時間: ${BENCH_TIME}ms"
echo ""
echo "実行される計測:"
echo "  1. perf (NNUE有効)    - ホットスポット分析"
echo "  2. perf (Material)    - ホットスポット分析"
if [ "$RUN_PERF_STAT" = true ]; then
    echo "  3. perf stat (NNUE)   - perf stat計測"
    echo "  4. perf stat (Material) - perf stat計測"
    echo "  5. benchmark (NNUE)   - NPS計測 (${THREADS}T + 並列効率)"
    echo "  6. benchmark (Material) - NPS計測 (${THREADS}T + 並列効率)"
else
    echo "  3. benchmark (NNUE)   - NPS計測 (${THREADS}T + 並列効率)"
    echo "  4. benchmark (Material) - NPS計測 (${THREADS}T + 並列効率)"
    echo "  * perf stat は --perf-stat 指定時のみ実行"
fi
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
TOTAL_STEPS=4
if [ "$RUN_PERF_STAT" = true ]; then
    TOTAL_STEPS=6
fi
STEP=1

echo "  ${STEP}/${TOTAL_STEPS}: perf (NNUE有効)"
echo "=============================================="
if [ "$SKIP_NNUE" = false ]; then
    ./scripts/perf_profile_nnue.sh --movetime 5000 --nnue-file "$NNUE_FILE"
else
    echo "スキップ: NNUEファイルがありません"
fi
STEP=$((STEP + 1))

echo ""
echo "=============================================="
echo "  ${STEP}/${TOTAL_STEPS}: perf (Material評価)"
echo "=============================================="
./scripts/perf_profile.sh
STEP=$((STEP + 1))

if [ "$RUN_PERF_STAT" = true ]; then
    echo ""
    echo "=============================================="
    echo "  ${STEP}/${TOTAL_STEPS}: perf stat (NNUE有効)"
    echo "=============================================="
    if [ "$SKIP_NNUE" = false ]; then
        echo "perf stat results will be saved under ./perf_results"
        RUSTFLAGS="-C target-cpu=native" cargo build -p tools --bin benchmark --release
        STAT_TIMESTAMP=$(date +%Y%m%d_%H%M%S)
        STAT_FILE="./perf_results/${STAT_TIMESTAMP}_perfstat_nnue.txt"
        sudo perf stat -e dTLB-load-misses,cache-misses,branch-misses \
            ./target/release/benchmark --internal --nnue-file "$NNUE_FILE" \
            --limit-type movetime --limit 5000 --iterations 1 2>&1 | tee "$STAT_FILE"
    else
        echo "スキップ: NNUEファイルがありません"
    fi
    STEP=$((STEP + 1))

    echo ""
    echo "=============================================="
    echo "  ${STEP}/${TOTAL_STEPS}: perf stat (Material評価)"
    echo "=============================================="
    echo "perf stat results will be saved under ./perf_results"
    RUSTFLAGS="-C target-cpu=native" cargo build -p tools --bin benchmark --release
    STAT_TIMESTAMP=$(date +%Y%m%d_%H%M%S)
    STAT_FILE="./perf_results/${STAT_TIMESTAMP}_perfstat_material.txt"
    sudo perf stat -e dTLB-load-misses,cache-misses,branch-misses \
        ./target/release/benchmark --internal \
        --limit-type movetime --limit 5000 --iterations 1 2>&1 | tee "$STAT_FILE"
    STEP=$((STEP + 1))
fi

# NPS結果を保存する連想配列
declare -A NNUE_NPS
declare -A MATERIAL_NPS

# ビルド（ベンチマーク用）
echo ""
echo "=============================================="
echo "  ベンチマーク用バイナリのビルド"
echo "=============================================="
RUSTFLAGS="-C target-cpu=native" cargo build -p tools --bin benchmark --release

echo ""
echo "=============================================="
echo "  ${STEP}/${TOTAL_STEPS}: benchmark (NNUE有効)"
echo "=============================================="
if [ "$SKIP_NNUE" = false ]; then
    for t in "${THREAD_LIST[@]}"; do
        echo "--- スレッド: $t ---"
        OUTPUT=$(./target/release/benchmark \
            --internal --threads "$t" --limit-type movetime --limit "$BENCH_TIME" \
            --nnue-file "$NNUE_FILE" \
            --output-dir ./benchmark_results 2>&1)
        echo "$OUTPUT"
        # NPSを抽出（"1    4    609551    ..." 形式から4列目を取得）
        NPS=$(echo "$OUTPUT" | grep -E "^$t\s+" | awk '{print $4}' | tr -d ',')
        if [ -n "$NPS" ]; then
            NNUE_NPS[$t]="$NPS"
        fi
    done
else
    echo "スキップ: NNUEファイルがありません"
fi
STEP=$((STEP + 1))

echo ""
echo "=============================================="
echo "  ${STEP}/${TOTAL_STEPS}: benchmark (Material評価)"
echo "=============================================="
for t in "${THREAD_LIST[@]}"; do
    echo "--- スレッド: $t ---"
    OUTPUT=$(./target/release/benchmark \
        --internal --threads "$t" --limit-type movetime --limit "$BENCH_TIME" \
        --output-dir ./benchmark_results 2>&1)
    echo "$OUTPUT"
    # NPSを抽出
    NPS=$(echo "$OUTPUT" | grep -E "^$t\s+" | awk '{print $4}' | tr -d ',')
    if [ -n "$NPS" ]; then
        MATERIAL_NPS[$t]="$NPS"
    fi
done

echo ""
echo "=============================================="
echo "  計測完了"
echo "=============================================="
echo ""

# 並列効率サマリを出力
SUMMARY_TIMESTAMP=$(date +%Y%m%d_%H%M%S)
SUMMARY_FILE="./benchmark_results/${SUMMARY_TIMESTAMP}_parallel_summary.txt"

{
    echo "=============================================="
    echo "  並列探索パフォーマンスサマリ"
    echo "=============================================="
    echo "日時: $(date -Iseconds)"
    echo "探索時間: ${BENCH_TIME}ms × 4局面"
    echo ""

    # Material評価の結果
    if [ ${#MATERIAL_NPS[@]} -gt 0 ]; then
        echo "=== Material評価 ==="
        printf "%-10s %15s %10s %10s\n" "Threads" "NPS" "Scale" "Efficiency"
        printf "%-10s %15s %10s %10s\n" "-------" "---" "-----" "----------"
        FIRST_NPS=""
        for t in "${THREAD_LIST[@]}"; do
            nps="${MATERIAL_NPS[$t]:-}"
            if [ -n "$nps" ]; then
                if [ -z "$FIRST_NPS" ]; then
                    FIRST_NPS="$nps"
                fi
                if [ -n "$FIRST_NPS" ] && [ "$FIRST_NPS" -gt 0 ]; then
                    scale=$(echo "scale=2; $nps / $FIRST_NPS" | bc)
                    eff=$(echo "scale=1; 100 * $nps / ($FIRST_NPS * $t)" | bc)
                    printf "%-10s %15s %10sx %9s%%\n" "$t" "$nps" "$scale" "$eff"
                else
                    printf "%-10s %15s %10s %10s\n" "$t" "$nps" "-" "-"
                fi
            fi
        done
        echo ""
    fi

    # NNUE評価の結果
    if [ ${#NNUE_NPS[@]} -gt 0 ]; then
        echo "=== NNUE評価 ==="
        printf "%-10s %15s %10s %10s\n" "Threads" "NPS" "Scale" "Efficiency"
        printf "%-10s %15s %10s %10s\n" "-------" "---" "-----" "----------"
        FIRST_NPS=""
        for t in "${THREAD_LIST[@]}"; do
            nps="${NNUE_NPS[$t]:-}"
            if [ -n "$nps" ]; then
                if [ -z "$FIRST_NPS" ]; then
                    FIRST_NPS="$nps"
                fi
                if [ -n "$FIRST_NPS" ] && [ "$FIRST_NPS" -gt 0 ]; then
                    scale=$(echo "scale=2; $nps / $FIRST_NPS" | bc)
                    eff=$(echo "scale=1; 100 * $nps / ($FIRST_NPS * $t)" | bc)
                    printf "%-10s %15s %10sx %9s%%\n" "$t" "$nps" "$scale" "$eff"
                else
                    printf "%-10s %15s %10s %10s\n" "$t" "$nps" "-" "-"
                fi
            fi
        done
    fi
} | tee "$SUMMARY_FILE"

echo ""
echo "=== サマリ保存: $SUMMARY_FILE ==="
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


echo "ドキュメント更新:"
echo "  Rust native: Claude Codeで /update-rust-perf-docs を実行してください"
echo "  WASM:        Claude Codeで /update-wasm-perf-docs を実行してください"
