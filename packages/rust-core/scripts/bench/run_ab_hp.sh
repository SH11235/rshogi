#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/bench/run_ab_hp.sh

# Sweep History Pruning threshold and compare outcomes.
# Usage: CUT_AT=5a4b scripts/run_ab_hp.sh

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

mkdir -p runs/repro

HP_VALUES=( -2000 -1000 -500 0 )
THREADS_SET=( 1 8 )

for hp in "${HP_VALUES[@]}"; do
  for th in "${THREADS_SET[@]}"; do
    OUT="runs/repro/hp${hp}_threads${th}.log"
    echo "[ab-hp] hp=${hp} threads=${th} -> $OUT" >&2
    {
      echo usi
      echo "setoption name USI_Hash value 1024"
      echo "setoption name Threads value ${th}"
      echo "setoption name USI_Ponder value false"
      echo "setoption name MultiPV value 3"
      echo "setoption name MinThinkMs value 200"
      echo "setoption name EngineType value Enhanced"
      echo "setoption name SearchParams.HP_Threshold value ${hp}"
      echo isready
      echo readyok
      # position from first line, optionally trimmed at CUT_AT
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
    } | timeout 30s stdbuf -oL -eL target/release/engine-usi | tee "$OUT" >/dev/null
  done
done

echo "[ab-hp] done. grep '^bestmove' runs/repro/hp*.log"
