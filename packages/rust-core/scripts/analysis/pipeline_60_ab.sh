#!/usr/bin/env bash
set -euo pipefail

# pipeline_60_ab.sh
# - 対局ログ群からスパイク抽出→back数手遡り→60件選別→10秒/8T/3プロファイル評価→
#   “真の悪手”CSVと落下率メトリクスを保存するワンコマンドパイプライン。
# - 実行時間が長いのは評価フェーズのみ。
#
# 使い方例:
#   scripts/analysis/pipeline_60_ab.sh \
#     --logs 'taikyoku-log/taikyoku_log_enhanced-parallel-202511*.md' \
#     --out runs/\$(date +%Y%m%d-%H%M)-tuning \
#     --threads 8 --byoyomi 10000
#
# 環境変数:
#   ENGINE_BIN: USIエンジンパス（既定: target/release/engine-usi）

LOGS_GLOB=""
OUT_DIR="runs/$(date +%Y%m%d-%H%M)-tuning"
THREADS=8
BYO=10000
THRESH=250
TOPK=12
BACK_MIN=3
BACK_MAX=6
MINTHINK=100
WARMUPMS=200

print_usage(){
  cat << USAGE >&2
Usage: $0 --logs 'taikyoku-log/taikyoku_log_enhanced-parallel-202511*.md' [--out DIR] \
          [--threads N] [--byoyomi MS] [--threshold N] [--topk K] [--back-min N] [--back-max N]
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --logs) LOGS_GLOB="$2"; shift 2;;
    --out) OUT_DIR="$2"; shift 2;;
    --threads) THREADS="$2"; shift 2;;
    --byoyomi) BYO="$2"; shift 2;;
    --threshold) THRESH="$2"; shift 2;;
    --topk) TOPK="$2"; shift 2;;
    --back-min) BACK_MIN="$2"; shift 2;;
    --back-max) BACK_MAX="$2"; shift 2;;
    -h|--help) print_usage; exit 0;;
    *) echo "Unknown arg: $1" >&2; print_usage; exit 1;;
  esac
done

if [ -z "$LOGS_GLOB" ]; then
  echo "Error: --logs is required" >&2
  print_usage
  exit 1
fi

mkdir -p "$OUT_DIR"

echo "[1/5] Extracting targets from logs -> $OUT_DIR" >&2
python3 scripts/analysis/make_targets_from_logs.py \
  --threshold "$THRESH" --topk "$TOPK" --back-min "$BACK_MIN" --back-max "$BACK_MAX" \
  --out "$OUT_DIR" $LOGS_GLOB

echo "[2/5] Selecting top 60 (back>=$BACK_MIN, |delta| desc)" >&2
jq '{targets: (.targets | map(select(.back_plies >= '"$BACK_MIN"')) | sort_by((.origin_delta | (if .>=0 then . else - . end))) | reverse | .[:60])}' \
  "$OUT_DIR/targets.json" > "$OUT_DIR/targets.60.json"
cp "$OUT_DIR/targets.60.json" "$OUT_DIR/targets.json"

echo "[3/5] Running eval (10s, ${THREADS}T, 3 profiles)" >&2
: "${ENGINE_BIN:=target/release/engine-usi}"
ENGINE_BIN="$ENGINE_BIN" python3 scripts/analysis/run_eval_targets.py "$OUT_DIR" \
  --threads "$THREADS" --byoyomi "$BYO" --minthink "$MINTHINK" --warmupms "$WARMUPMS"

echo "[4/5] Summarizing true blunders and drop metrics" >&2
python3 scripts/analysis/summarize_true_blunders.py "$OUT_DIR"
python3 scripts/analysis/summarize_drop_metrics.py "$OUT_DIR" --bad-th -600 > "$OUT_DIR/metrics_all.json"
python3 scripts/analysis/summarize_drop_metrics.py "$OUT_DIR" --bad-th -600 --profile base > "$OUT_DIR/metrics_base.json"
python3 scripts/analysis/summarize_drop_metrics.py "$OUT_DIR" --bad-th -600 --profile gates > "$OUT_DIR/metrics_gates.json"
python3 scripts/analysis/summarize_drop_metrics.py "$OUT_DIR" --bad-th -600 --profile rootfull > "$OUT_DIR/metrics_rootfull.json"

echo "[5/5] A/B quick view (base vs gates/rootfull) -> CSV" >&2
# base<-300 and gates>0 を救済候補として CSV 出力
jq -s '[.[1][]] as $r | [.[0].targets[]] as $t | [ $t[] as $m | {origin: ($m.origin_log+":"+($m.origin_ply|tostring)), tag: $m.tag, back: $m.back_plies, base: ($r[]|select(.tag==$m.tag and .profile=="base")|.eval_cp), gates: ($r[]|select(.tag==$m.tag and .profile=="gates")|.eval_cp), rootfull: ($r[]|select(.tag==$m.tag and .profile=="rootfull")|.eval_cp) } | select(.base!=null and .gates!=null) | select(.base<-300 and .gates>0) ] | sort_by(.origin,.back)' \
  "$OUT_DIR/targets.json" "$OUT_DIR/summary.json" \
  | jq -r '("origin,tag,back,base,gates,rootfull"), (.[] | "\(.origin),\(.tag),\(.back),\(.base),\(.gates),\(.rootfull)")' \
  > "$OUT_DIR/ab_rescue_candidates.csv"

echo "Done. Outputs in: $OUT_DIR" >&2
echo "- targets:        $OUT_DIR/targets.json" >&2
echo "- eval summary:   $OUT_DIR/summary.json" >&2
echo "- true blunders:  $OUT_DIR/true_blunders_first_bad.csv, $OUT_DIR/true_blunders_rescue_candidates.csv" >&2
echo "- drop metrics:   $OUT_DIR/metrics_all.json (and *_base/gates/rootfull.json)" >&2
echo "- A/B candidates: $OUT_DIR/ab_rescue_candidates.csv" >&2
