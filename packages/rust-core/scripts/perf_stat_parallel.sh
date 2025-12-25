#!/bin/bash
# perf_stat_parallel.sh - 並列探索のパフォーマンス計測スクリプト
#
# 使い方:
#   ./scripts/perf_stat_parallel.sh                    # 1,4,8スレッドでMaterial評価を計測
#   ./scripts/perf_stat_parallel.sh --threads 1,2,4,8  # スレッド数を指定
#   ./scripts/perf_stat_parallel.sh --nnue             # NNUE評価を使用（perf.confから読み込み）
#   ./scripts/perf_stat_parallel.sh --events-cache     # キャッシュ関連イベントを計測
#   ./scripts/perf_stat_parallel.sh --summary          # 結果サマリのみ表示
#
# 出力:
#   - perf_results/${TIMESTAMP}_*_perfstat.txt   : perf stat結果
#   - perf_results/${TIMESTAMP}_*_benchmark.txt  : ベンチマーク結果
#   - perf_results/${TIMESTAMP}_summary.txt      : 全スレッド数のサマリ

set -euo pipefail

cd "$(dirname "$0")/.."

# === デフォルト値 ===
THREADS="1,4,8"
TT_MB=256
LIMIT_MS=5000
REPEAT=3
NNUE_FILE=""
USE_NNUE=false
BENCH_PATH="./target/release/benchmark"
EVENTS=""
METRICS=""
USE_SUDO=true
NO_BUILD=false
MATERIAL_LEVEL=3
OUTPUT_DIR="./perf_results"
LIST_LLC=false
SUMMARY_ONLY=false
USE_INTERNAL=true

# AMD Zen3/4向けL3/DRAMイベント（プロセッサ依存）
EVENTS_CACHE="cache-misses,cache-references,L1-dcache-load-misses,dTLB-load-misses"
EVENTS_L3_DRAM="ls_any_fills_from_sys.ext_cache_local,ls_any_fills_from_sys.mem_io_local"

# 設定ファイルからNNUEパスを読み込み（存在すれば）
SCRIPT_DIR="$(dirname "$0")"
CONF_FILE="$SCRIPT_DIR/perf.conf"
if [ -f "$CONF_FILE" ]; then
    source "$CONF_FILE"
fi

print_usage() {
    cat <<'EOF'
Usage: ./scripts/perf_stat_parallel.sh [options]

基本オプション:
  --threads <list>       スレッド数リスト (例: 1,4,8) (default: 1,4,8)
  --tt-mb <mb>           TTサイズ (MB) (default: 256)
  --limit-ms <ms>        探索時間 (ms) (default: 5000)
  --repeat <n>           perf stat繰り返し回数 (default: 3)

評価関数:
  --nnue                 NNUE評価を使用 (perf.confのNNUE_FILEを使用)
  --nnue-file <path>     NNUEファイルを直接指定
  --material-level <lv>  Materialレベル (default: 3)

perf statイベント:
  --events <list>        カスタムイベント指定
  --events-cache         キャッシュ関連イベント (cache-misses,L1-dcache等)
  --events-l3dram        L3/DRAM関連イベント (AMD Zen3/4向け)
  -d, --detailed         詳細モード (perf stat -d)

出力制御:
  --output-dir <dir>     出力ディレクトリ (default: ./perf_results)
  --summary              結果サマリのみ表示（個別ファイル出力なし）
  --no-build             ビルドをスキップ
  --no-sudo              sudo無しで実行

ユーティリティ:
  --list-events          利用可能なキャッシュ/DRAM関連イベントを表示
  -h, --help             このヘルプを表示

例:
  # 並列効率を計測（最も一般的な使い方）
  ./scripts/perf_stat_parallel.sh --threads 1,8

  # NNUE評価でキャッシュ効率を計測
  ./scripts/perf_stat_parallel.sh --nnue --events-cache

  # 結果サマリだけを素早く確認
  ./scripts/perf_stat_parallel.sh --summary --repeat 1
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --threads)
            THREADS="$2"
            shift 2
            ;;
        --tt-mb)
            TT_MB="$2"
            shift 2
            ;;
        --limit-ms)
            LIMIT_MS="$2"
            shift 2
            ;;
        --repeat)
            REPEAT="$2"
            shift 2
            ;;
        --nnue)
            USE_NNUE=true
            shift
            ;;
        --nnue-file)
            NNUE_FILE="$2"
            USE_NNUE=true
            shift 2
            ;;
        --material-level)
            MATERIAL_LEVEL="$2"
            shift 2
            ;;
        --events)
            EVENTS="$2"
            shift 2
            ;;
        --events-cache)
            EVENTS="$EVENTS_CACHE"
            shift
            ;;
        --events-l3dram)
            EVENTS="$EVENTS_L3_DRAM"
            shift
            ;;
        -d|--detailed)
            EVENTS=""  # -d モードを使用
            shift
            ;;
        --output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --summary)
            SUMMARY_ONLY=true
            shift
            ;;
        --no-sudo)
            USE_SUDO=false
            shift
            ;;
        --list-events)
            LIST_LLC=true
            shift
            ;;
        --no-build)
            NO_BUILD=true
            shift
            ;;
        -h|--help)
            print_usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            print_usage
            exit 1
            ;;
    esac
done

if ! command -v perf >/dev/null 2>&1; then
    echo "Error: perf command not found"
    exit 1
fi

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd -P)"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

# --list-events: 利用可能なキャッシュ関連イベントを表示
if [ "$LIST_LLC" = true ]; then
    LIST_FILE="${OUTPUT_DIR}/${TIMESTAMP}_perf_list_cache.txt"
    perf_list_cmd=(perf list)
    if [ "$USE_SUDO" = true ] && [ "$EUID" -ne 0 ]; then
        perf_list_cmd=(sudo "${perf_list_cmd[@]}")
    fi
    echo "=== 利用可能なキャッシュ/DRAM関連イベント ==="
    if command -v rg >/dev/null 2>&1; then
        "${perf_list_cmd[@]}" | rg -i "llc|l3|imc|dram|cache" | tee "$LIST_FILE"
    else
        "${perf_list_cmd[@]}" | grep -i -E "llc|l3|imc|dram|cache" | tee "$LIST_FILE"
    fi
    echo ""
    echo "Saved: $LIST_FILE"
    exit 0
fi

# NNUE設定の検証
if [ "$USE_NNUE" = true ] && [ -z "$NNUE_FILE" ]; then
    echo "Error: --nnue を指定しましたが、NNUE_FILEが設定されていません"
    echo "       scripts/perf.conf を編集するか、--nnue-file で直接指定してください"
    exit 1
fi

if [ -n "$NNUE_FILE" ] && [ ! -f "$NNUE_FILE" ]; then
    echo "Error: NNUE file not found: $NNUE_FILE"
    exit 1
fi

# ビルド
if [ "$NO_BUILD" = false ]; then
    echo "=== Building release binaries ==="
    RUSTFLAGS="-C target-cpu=native" \
        cargo build --release -p tools --bin benchmark
fi

if [ ! -x "$BENCH_PATH" ]; then
    echo "Error: benchmark binary not found: $BENCH_PATH"
    exit 1
fi

# モード決定
MODE="material"
if [ "$USE_NNUE" = true ]; then
    MODE="nnue"
fi

IFS=',' read -r -a THREAD_LIST <<< "$THREADS"
if [ "${#THREAD_LIST[@]}" -eq 0 ]; then
    echo "Error: threads list is empty"
    exit 1
fi

RUN_TAG="${TIMESTAMP}_${MODE}_tt${TT_MB}"
SUMMARY_FILE="${OUTPUT_DIR}/${RUN_TAG}_summary.txt"

# サマリ用配列
declare -a NPS_RESULTS=()
FIRST_NPS=""

echo ""
echo "=== 並列探索パフォーマンス計測 ==="
echo "モード: $MODE"
echo "スレッド: $THREADS"
echo "TT: ${TT_MB}MB"
echo "探索時間: ${LIMIT_MS}ms × 4局面"
echo "繰り返し: ${REPEAT}回"
echo ""

for t in "${THREAD_LIST[@]}"; do
    PERF_FILE="${OUTPUT_DIR}/${RUN_TAG}_t${t}_perfstat.txt"
    BENCH_FILE="${OUTPUT_DIR}/${RUN_TAG}_t${t}_bench.txt"

    # perf statコマンド構築
    perf_cmd=(perf stat -r "$REPEAT")
    if [ -n "$EVENTS" ]; then
        perf_cmd+=(-e "$EVENTS")
    else
        perf_cmd+=(-d)
    fi

    if [ "$USE_SUDO" = true ] && [ "$EUID" -ne 0 ]; then
        perf_cmd=(sudo "${perf_cmd[@]}")
    fi

    # benchmarkコマンド構築（--internalモード使用）
    bench_cmd=(
        "$BENCH_PATH"
        --internal
        --threads "$t"
        --tt-mb "$TT_MB"
        --limit-type movetime
        --limit "$LIMIT_MS"
        --iterations 1
        --material-level "$MATERIAL_LEVEL"
    )
    if [ "$USE_NNUE" = true ]; then
        bench_cmd+=(--nnue-file "$NNUE_FILE")
    fi

    echo "--- スレッド: $t ---"

    if [ "$SUMMARY_ONLY" = true ]; then
        # サマリモード: perf stat無しで実行
        OUTPUT=$("${bench_cmd[@]}" 2>&1)
        NPS=$(echo "$OUTPUT" | grep -E "^$t\s+" | awk '{print $4}' | tr -d ',')
    else
        # 通常モード: perf stat付きで実行
        {
            echo "=== perf stat: threads=$t ($MODE) ==="
            echo "timestamp: $TIMESTAMP"
            echo "threads: $t"
            echo "tt_mb: $TT_MB"
            echo "movetime_ms: $LIMIT_MS"
            echo "repeat: $REPEAT"
            if [ "$USE_NNUE" = true ]; then
                echo "nnue_file: $NNUE_FILE"
            else
                echo "material_level: $MATERIAL_LEVEL"
            fi
            echo ""
        } > "$BENCH_FILE"

        OUTPUT=$("${perf_cmd[@]}" -- "${bench_cmd[@]}" 2> "$PERF_FILE" | tee -a "$BENCH_FILE")
        NPS=$(echo "$OUTPUT" | grep -E "^$t\s+" | awk '{print $4}' | tr -d ',')

        echo "  perf stat: $PERF_FILE"
    fi

    # NPS抽出と保存
    if [ -n "$NPS" ]; then
        NPS_RESULTS+=("$t:$NPS")
        if [ -z "$FIRST_NPS" ]; then
            FIRST_NPS="$NPS"
        fi
        echo "  NPS: $NPS"
    else
        echo "  NPS: (取得失敗)"
    fi
    echo ""
done

# サマリ出力
echo "=== 結果サマリ ==="
{
    echo "=== 並列探索パフォーマンス計測結果 ==="
    echo "日時: $(date -Iseconds)"
    echo "モード: $MODE"
    echo "TT: ${TT_MB}MB"
    echo "探索時間: ${LIMIT_MS}ms × 4局面"
    echo ""
    printf "%-10s %15s %10s %10s\n" "Threads" "NPS" "Scale" "Efficiency"
    printf "%-10s %15s %10s %10s\n" "-------" "---" "-----" "----------"

    for result in "${NPS_RESULTS[@]}"; do
        t="${result%%:*}"
        nps="${result#*:}"
        if [ -n "$FIRST_NPS" ] && [ "$FIRST_NPS" -gt 0 ]; then
            scale=$(echo "scale=2; $nps / $FIRST_NPS" | bc)
            eff=$(echo "scale=1; 100 * $nps / ($FIRST_NPS * $t)" | bc)
            printf "%-10s %15s %10sx %9s%%\n" "$t" "$nps" "$scale" "$eff"
        else
            printf "%-10s %15s %10s %10s\n" "$t" "$nps" "-" "-"
        fi
    done
} | tee "$SUMMARY_FILE"

echo ""
echo "=== Saved: $SUMMARY_FILE ==="
