#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/bench/cases/run_lmr_case.sh

# Run a single LMR_K_x100 case
# Usage: CUT_AT=5a4b scripts/run_lmr_case.sh <lmr_k_x100> <threads>

K=${1:-170}
THREADS=${2:-8}

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

OUT="runs/repro/lmr${K}_threads${THREADS}.log"
mkdir -p runs/repro
echo "[lmr-case] LMR_K_x100=${K} threads=${THREADS} -> $OUT" >&2

POS_LINE_RAW=$(sed -n '1p' taikyoku_log_202510070935.md)
POS_LINE="$POS_LINE_RAW"
if [[ -n "${CUT_AT:-}" && "$POS_LINE_RAW" == position\ startpos* ]]; then
  rest=${POS_LINE_RAW#position startpos }
  if [[ "$rest" == moves* ]]; then
    toks=($rest); acc=("position" "startpos" "moves"); found=0
    for ((i=1;i<${#toks[@]};i++)); do acc+=("${toks[$i]}"); [[ "${toks[$i]}" == "$CUT_AT" ]] && { found=1; break; }; done
    [[ $found -eq 1 ]] && POS_LINE="${acc[*]}"
  fi
fi

{
  echo usi
  echo "setoption name USI_Hash value 1024"
  echo "setoption name Threads value ${THREADS}"
  echo "setoption name USI_Ponder value false"
  echo "setoption name MultiPV value 3"
  echo "setoption name MinThinkMs value 200"
  echo "setoption name EngineType value Enhanced"
  echo "setoption name SearchParams.LMR_K_x100 value ${K}"
  echo isready
  echo readyok
  echo "$POS_LINE"
  echo "go btime 0 wtime 0 byoyomi 10000"
} | timeout 30s stdbuf -oL -eL target/release/engine-usi | tee "$OUT" >/dev/null || true

rg -n "^bestmove|^info depth" "$OUT" | tail -n 20 || true
