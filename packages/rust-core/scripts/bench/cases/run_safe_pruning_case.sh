#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/bench/cases/run_safe_pruning_case.sh

# Compare SafePruning On/Off at the problematic position.
# Usage: CUT_AT=5a4b scripts/run_safe_pruning_case.sh <on|off> <threads>

MODE=${1:-on}
THREADS=${2:-8}

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

[[ "$MODE" == on || "$MODE" == off ]] || { echo "mode must be on|off" >&2; exit 1; }

ON=$([[ "$MODE" == on ]] && echo true || echo false)
TAG="safe_${MODE}_t${THREADS}"
OUT="runs/repro/${TAG}.log"

POS_LINE_RAW=$(sed -n '1p' taikyoku_log_202510070935.md)
if [[ "$POS_LINE_RAW" != position\ startpos* ]]; then
  echo "missing/invalid position line in taikyoku_log_202510070935.md" >&2
  exit 1
fi

POS_LINE="$POS_LINE_RAW"
if [[ -n "${CUT_AT:-}" ]]; then
  rest=${POS_LINE_RAW#position startpos }
  if [[ "$rest" == moves* ]]; then
    toks=($rest); acc=("position" "startpos" "moves"); found=0
    for ((i=1;i<${#toks[@]};i++)); do acc+=("${toks[$i]}"); [[ "${toks[$i]}" == "$CUT_AT" ]] && { found=1; break; }; done
    [[ $found -eq 1 ]] && POS_LINE="${acc[*]}"
  fi
fi

mkdir -p runs/repro
if [[ "${DIAG:-0}" != "0" ]]; then
  cargo build -q -p engine-usi --release --features diagnostics
else
  cargo build -q -p engine-usi --release
fi
echo "[safe-pruning] mode=${MODE} threads=${THREADS} -> $OUT" >&2

{
  echo usi
  echo "setoption name USI_Hash value 1024"
  echo "setoption name Threads value ${THREADS}"
  echo "setoption name USI_Ponder value false"
  echo "setoption name MultiPV value 3"
  echo "setoption name MinThinkMs value 200"
  echo "setoption name EngineType value Enhanced"
  echo "setoption name SearchParams.SafePruning value ${ON}"
  echo isready
  echo readyok
  echo "$POS_LINE"
  echo "go btime 0 wtime 0 byoyomi 10000"
} | timeout 30s env RUST_LOG=${RUST_LOG:-warn} TRACE_PLY_MIN=${TRACE_PLY_MIN:-0} TRACE_PLY_MAX=${TRACE_PLY_MAX:-200} stdbuf -oL -eL target/release/engine-usi 2>&1 | tee "$OUT" >/dev/null || true

rg -n "^bestmove|^info depth" "$OUT" | tail -n 20 || true
