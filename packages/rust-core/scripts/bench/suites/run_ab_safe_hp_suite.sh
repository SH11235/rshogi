#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/bench/suites/run_ab_safe_hp_suite.sh

# A/B suite for SafePruning and HP depth scale
# - 1 thread x3 runs, 8 threads x2 runs
# - safe on/off, and for safe=on test HP_DepthScale in {4361,3000}
# Usage: CUT_AT=5a4b DIAG=1 DIAG_ECHO_TAGS=1 scripts/run_ab_safe_hp_suite.sh

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

mkdir -p runs/repro
if [[ "${DIAG:-0}" != "0" ]]; then
  cargo build -q -p engine-usi --release --features diagnostics
else
  cargo build -q -p engine-usi --release
fi

run_case() {
  local SAFE=$1 SCALE=$2 TH=$3 RUN=$4
  local tag="suite_safe_${SAFE}_scale_${SCALE}_t${TH}_run${RUN}"
  local out="runs/repro/${tag}.log"
  echo "[suite] ${tag} -> ${out}" >&2
  {
    echo usi
    echo "setoption name USI_Hash value 1024"
    echo "setoption name Threads value ${TH}"
    echo "setoption name USI_Ponder value false"
    echo "setoption name MultiPV value 3"
    echo "setoption name MinThinkMs value 200"
    echo "setoption name EngineType value Enhanced"
    echo "setoption name SearchParams.SafePruning value ${SAFE}"
    if [[ "${SAFE}" == "true" ]]; then
      echo "setoption name SearchParams.HP_DepthScale value ${SCALE}"
    fi
    echo isready
    echo readyok
    POS_LINE_RAW=$(sed -n '1p' taikyoku_log_202510070935.md)
    if [[ -n "${CUT_AT:-}" && "$POS_LINE_RAW" == position\ startpos* ]]; then
      rest=${POS_LINE_RAW#position startpos }
      if [[ "$rest" == moves* ]]; then
        toks=($rest); acc=("position" "startpos" "moves"); found=0
        for ((i=1;i<${#toks[@]};i++)); do acc+=("${toks[$i]}"); [[ "${toks[$i]}" == "$CUT_AT" ]] && { found=1; break; }; done
        [[ $found -eq 1 ]] && POS_LINE="${acc[*]}" || POS_LINE="$POS_LINE_RAW"
      else
        POS_LINE="$POS_LINE_RAW"
      fi
    else
      POS_LINE="$POS_LINE_RAW"
    fi
    echo "$POS_LINE"
    echo "go btime 0 wtime 0 byoyomi 10000"
  } | timeout 30s env RUST_LOG=${RUST_LOG:-warn} TRACE_PLY_MIN=${TRACE_PLY_MIN:-0} TRACE_PLY_MAX=${TRACE_PLY_MAX:-200} stdbuf -oL -eL target/release/engine-usi 2>&1 | tee "$out" >/dev/null || true
}

for th in 1 8; do
  runs=$([[ $th -eq 1 ]] && echo 3 || echo 2)
  for ((i=1;i<=runs;i++)); do
    # safe=on, scale=4361 and 3000
    run_case true 4361 $th $i
    run_case true 3000 $th $i
    # safe=off (scale ignored)
    run_case false 0 $th $i
  done
done

echo "[suite] complete. Use scripts/summarize_diag_counters.sh runs/repro to count tags."
