#!/usr/bin/env bash
set -euo pipefail

# Gauntlet sharded runner (Spec013 threads=1 per shard)
# Usage:
#   scripts/nnue/gauntlet-sharded.sh BASE CAND TOTAL_GAMES SHARDS TIME_STR OUT_DIR [BOOK]
# Example:
#   scripts/nnue/gauntlet-sharded.sh runs/ref.nnue runs/cand.nnue 2000 16 "0/40+0.4" runs/gauntlet_sharded/$(date +%Y%m%d_%H%M) runs/fixed/20251011/openings_ply1_20_v1.sfen

BASELINE=${1:?BASELINE}
CANDIDATE=${2:?CANDIDATE}
TOTAL=${3:?TOTAL_GAMES}
SHARDS=${4:?SHARDS}
TC=${5:?TIME}
OUT_ROOT=${6:?OUT_DIR}
BOOK=${7:-runs/fixed/20251011/openings_ply1_20_v1.sfen}

mkdir -p "$OUT_ROOT"
echo "[info] shards=$SHARDS total_games=$TOTAL tc=$TC out=$OUT_ROOT"

cargo build -p tools --release --bin gauntlet --features nnue_telemetry >/dev/null

# Distribute games as evenly as possible, enforcing even games per shard (gauntlet requires even)
# Example: TOTAL=2000, SHARDS=16 -> BASE=125, BASE_EVEN=124, REMAIN=2000-124*16=16, EXTRA_PAIRS=8
# Assign G=BASE_EVEN+2 for first EXTRA_PAIRS shards, otherwise BASE_EVEN. Sum = TOTAL and each G is even.
BASE=$(( TOTAL / SHARDS ))
BASE_EVEN=$(( (BASE/2)*2 ))
REMAIN=$(( TOTAL - BASE_EVEN*SHARDS ))
EXTRA_PAIRS=$(( REMAIN / 2 ))

declare -a PIDS=()
for ((i=0; i<SHARDS; i++)); do
  G=$BASE_EVEN; if [ $i -lt $EXTRA_PAIRS ]; then G=$((G+2)); fi
  [ $G -eq 0 ] && continue
  SDIR="$OUT_ROOT/shard_$i"; mkdir -p "$SDIR"
  SEED=$((12345 + i))
  echo "[shard $i] games=$G seed=$SEED"
  nohup target/release/gauntlet \
    --base "$BASELINE" \
    --cand "$CANDIDATE" \
    --time "$TC" \
    --games "$G" \
    --threads 1 \
    --hash-mb 1024 \
    --book "$BOOK" \
    --json "$SDIR/result.json" \
    --report "$SDIR/report.md" \
    --seed "$SEED" > "$SDIR/shard.log" 2>&1 &
  echo $! > "$SDIR/pid"
  PIDS+=($(cat "$SDIR/pid"))
done

echo "[info] launched ${#PIDS[@]} shards"
printf "%s\n" "${PIDS[@]}" > "$OUT_ROOT/pids"
echo "[hint] merge with: scripts/nnue/merge-gauntlet-json.sh $OUT_ROOT"
