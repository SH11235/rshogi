#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/bench/run_ab_probcut.sh

# Toggle/adjust ProbCut and compare outcomes.
# Usage: CUT_AT=5a4b scripts/run_ab_probcut.sh

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

mkdir -p runs/repro
if [[ "${DIAG:-0}" != "0" ]]; then
  cargo build -q -p engine-usi --release --features diagnostics
else
  cargo build -q -p engine-usi --release
fi

CONF_CASES=(
  "EnableProbCut=false D5=250 D6P=300 tag=probcut_off"
  "EnableProbCut=true D5=250 D6P=300 tag=probcut_on_default"
  "EnableProbCut=true D5=350 D6P=450 tag=probcut_on_strict"
)

THREADS_SET=( 1 8 )

for case in "${CONF_CASES[@]}"; do
  eval $case
  for th in "${THREADS_SET[@]}"; do
    OUT="runs/repro/${tag}_threads${th}.log"
    echo "[ab-probcut] ${tag} threads=${th} -> $OUT" >&2
    {
      echo usi
      echo "setoption name USI_Hash value 1024"
      echo "setoption name Threads value ${th}"
      echo "setoption name USI_Ponder value false"
      echo "setoption name MultiPV value 3"
      echo "setoption name MinThinkMs value 200"
      echo "setoption name EngineType value Enhanced"
      echo "setoption name SearchParams.EnableProbCut value ${EnableProbCut}"
      echo "setoption name SearchParams.ProbCut_D5 value ${D5}"
      echo "setoption name SearchParams.ProbCut_D6P value ${D6P}"
      echo isready
      echo readyok
      POS_LINE_RAW=$(sed -n '1p' taikyoku_log_202510070935.md)
      CUT_AT=${CUT_AT:-}
      if [[ -n "$CUT_AT" && "$POS_LINE_RAW" == position\ startpos* ]]; then
        rest=${POS_LINE_RAW#position startpos }
        if [[ "$rest" == moves* ]]; then
          toks=($rest); acc=("position" "startpos" "moves"); found=0
          for ((i=1;i<${#toks[@]};i++)); do acc+=("${toks[$i]}"); [[ "${toks[$i]}" == "$CUT_AT" ]] && { found=1; break; }; done
          POS_LINE=$([[ $found -eq 1 ]] && echo "${acc[*]}" || echo "$POS_LINE_RAW")
        else POS_LINE="$POS_LINE_RAW"; fi
      else
        POS_LINE="$POS_LINE_RAW"
      fi
      echo "$POS_LINE"
      echo "go btime 0 wtime 0 byoyomi 10000"
    } | timeout 30s env RUST_LOG=${RUST_LOG:-warn} TRACE_PLY_MIN=${TRACE_PLY_MIN:-0} TRACE_PLY_MAX=${TRACE_PLY_MAX:-200} stdbuf -oL -eL target/release/engine-usi 2>&1 | tee "$OUT" >/dev/null
  done
done

echo "[ab-probcut] done. grep '^bestmove' runs/repro/probcut_*.log"
