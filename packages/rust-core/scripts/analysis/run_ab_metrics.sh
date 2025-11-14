#!/usr/bin/env bash
set -euo pipefail

# run_ab_metrics.sh
# - Evaluate one or more param presets against an existing dataset (targets.json present),
#   then compute both overall metrics and first_bad/avoidance metrics.
#
# Usage:
#   scripts/analysis/run_ab_metrics.sh \
#     --dataset runs/20251112-2014-tuning \
#     --out-root runs/$(date +%Y%m%d-%H%M)-ab \
#     scripts/analysis/param_presets/foo.json [bar.json ...]
#
# ENV:
#   ENGINE_BIN: path to engine-usi (default: target/release/engine-usi)

DATASET=""
OUT_ROOT="runs/$(date +%Y%m%d-%H%M)-ab"

print_usage(){
  cat << USAGE >&2
Usage: $0 --dataset <dir> --out-root <dir> <preset.json>...
USAGE
}

if [ $# -lt 3 ]; then
  print_usage; exit 1
fi

while [ $# -gt 0 ]; do
  case "$1" in
    --dataset) DATASET="$2"; shift 2;;
    --out-root) OUT_ROOT="$2"; shift 2;;
    -h|--help) print_usage; exit 0;;
    *) break;;
  esac
done

if [ -z "$DATASET" ] || [ ! -f "$DATASET/targets.json" ]; then
  echo "Error: --dataset must point to a directory containing targets.json" >&2
  exit 1
fi

PRESETS=("$@")
if [ ${#PRESETS[@]} -eq 0 ]; then
  echo "Error: no preset json given" >&2
  exit 1
fi

mkdir -p "$OUT_ROOT" || true

echo "[run_ab_metrics] dataset=$DATASET out_root=$OUT_ROOT presets=${#PRESETS[@]}" >&2

: "${ENGINE_BIN:=target/release/engine-usi}"

for PRESET in "${PRESETS[@]}"; do
  if [ ! -f "$PRESET" ]; then
    echo "skip (not found): $PRESET" >&2; continue
  fi
  NAME=$(jq -r '.name // "exp"' "$PRESET")
  OUT_DIR="${OUT_ROOT}-${NAME}"
  mkdir -p "$OUT_DIR"
  cp "$DATASET/targets.json" "$OUT_DIR/"
  echo "[eval] $NAME -> $OUT_DIR" >&2
  ENGINE_BIN="$ENGINE_BIN" python3 scripts/analysis/run_eval_targets_params.py "$OUT_DIR" \
    --threads 8 --byoyomi 10000 --minthink 100 --warmupms 200 --params-json "$PRESET"
  # Overall metrics (bad_th=-600)
  python3 scripts/analysis/summarize_drop_metrics.py "$OUT_DIR" --bad-th -600 > "$OUT_DIR/metrics.json"
  # Ensure first_bad csv
  python3 scripts/analysis/summarize_true_blunders.py "$OUT_DIR" >/dev/null || true
  # First-bad-only and avoidance
  python3 scripts/analysis/summarize_first_bad_metrics.py "$OUT_DIR" --profile "$NAME" > "$OUT_DIR/metrics_first_bad.json" || true
  python3 scripts/analysis/summarize_avoidance.py "$OUT_DIR" --profile "$NAME" > "$OUT_DIR/avoidance.json" || true
  # Brief summary
  ALL_RATE=$(jq -r '.spike_rate_percent' "$OUT_DIR/metrics.json" 2>/dev/null || echo "-")
  FB_RATE=$(jq -r '.spike_rate_percent' "$OUT_DIR/metrics_first_bad.json" 2>/dev/null || echo "-")
  AV_RATE=$(jq -r '.avoid_rate_percent' "$OUT_DIR/avoidance.json" 2>/dev/null || echo "-")
  echo "[summary] name=$NAME all_spike=${ALL_RATE}% firstbad_spike=${FB_RATE}% avoid=${AV_RATE}% -> $OUT_DIR" >&2
done

echo "[done] Outputs under $OUT_ROOT-*" >&2

