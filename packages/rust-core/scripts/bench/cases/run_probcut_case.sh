#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/bench/cases/run_probcut_case.sh

# Run a single ProbCut case: mode in {off, default, strict}, threads in {1,8,...}
# Usage: CUT_AT=5a4b scripts/run_probcut_case.sh <mode> <threads>

MODE=${1:-default}
THREADS=${2:-8}

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

case "$MODE" in
  off)
    ENABLE=false; D5=250; D6P=300; TAG="probcut_off";;
  strict)
    ENABLE=true; D5=350; D6P=450; TAG="probcut_strict";;
  default)
    ENABLE=true; D5=250; D6P=300; TAG="probcut_default";;
  *) echo "unknown mode: $MODE" >&2; exit 1;;
esac

OUT="runs/repro/${TAG}_threads${THREADS}.log"
MULTIPV=${MULTIPV:-3}
mkdir -p runs/repro
echo "[probcut-case] mode=${MODE} threads=${THREADS} -> $OUT" >&2

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

{
  echo usi
  echo "setoption name USI_Hash value 1024"
  echo "setoption name Threads value ${THREADS}"
  echo "setoption name USI_Ponder value false"
  echo "setoption name MultiPV value ${MULTIPV}"
  echo "setoption name MinThinkMs value 200"
  echo "setoption name EngineType value Enhanced"
  echo "setoption name SearchParams.EnableProbCut value ${ENABLE}"
  echo "setoption name SearchParams.ProbCut_D5 value ${D5}"
  echo "setoption name SearchParams.ProbCut_D6P value ${D6P}"
  echo isready
  echo readyok
  echo "$POS_LINE"
  echo "go btime 0 wtime 0 byoyomi 10000"
} | timeout 30s stdbuf -oL -eL target/release/engine-usi | tee "$OUT" >/dev/null || true

rg -n "^bestmove|^info depth" "$OUT" | tail -n 20 || true
