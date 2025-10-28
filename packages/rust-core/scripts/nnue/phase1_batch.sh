#!/usr/bin/env bash
set -euo pipefail

# Phase-1 batch pipeline (Fast -> training Single)
# - Extract O/M/E from Floodgate CSA
# - Unique + sampling to target unique positions
# - Annotate (JSONL, WDL, MultiPV=2, time=200ms)
# - Build cache (gzip, v1)
# - Train Single (1 epoch, stream-cache)
#
# Usage:
#   scripts/nnue/phase1_batch.sh [OUT_DIR] [ROOT_CSA]
#     OUT_DIR   : default runs/phase1_YYYYMMDD_HHMM
#     ROOT_CSA  : default data/floodgate/raw/x
#
# Tunables (env):
#   TARGET_UNIQ (default 1000000)
#   O_RATE/M_RATE/E_RATE (default 0.40/0.50/0.10)
#   O_CAP/M_CAP/E_CAP (default 40/60/20)
#   TIME_MS (default 200), JOBS (default nproc), HASH_MB (default 256)
#   BATCH_SIZE (default 16384), EPOCHS (default 1)
#   DO_CLASSIC (0/1; default 0) to run Classic distillation after Single
#   MULTIPV (default 2)
#   EXTRA_ANNOTATE_ARGS (extra CLI flags for generate_nnue_training_data)
#   KD_ALPHA (optional), KD_TEMPERATURE (optional), TEACHER_SCALE_FIT (default linear)

timestamp() { date +%Y%m%d_%H%M%S; }

OUT_DIR=${1:-"runs/phase1_$(timestamp)"}
ROOT_CSA=${2:-"data/floodgate/raw/x"}
TARGET_UNIQ=${TARGET_UNIQ:-1000000}
O_RATE=${O_RATE:-0.40}
M_RATE=${M_RATE:-0.50}
E_RATE=${E_RATE:-0.10}
O_CAP=${O_CAP:-40}
M_CAP=${M_CAP:-60}
E_CAP=${E_CAP:-20}
TIME_MS=${TIME_MS:-200}
# Optional: endgame-only extra time (ms). If set, end sample is annotated separately with TIME_MS_END.
TIME_MS_END=${TIME_MS_END:-}
JOBS=${JOBS:-$(nproc)}
HASH_MB=${HASH_MB:-256}
BATCH_SIZE=${BATCH_SIZE:-16384}
EPOCHS=${EPOCHS:-1}
DO_CLASSIC=${DO_CLASSIC:-0}
MULTIPV=${MULTIPV:-2}
EXTRA_ANNOTATE_ARGS=${EXTRA_ANNOTATE_ARGS:-}
TEACHER_SCALE_FIT=${TEACHER_SCALE_FIT:-linear}
KD_ALPHA=${KD_ALPHA:-}
KD_TEMPERATURE=${KD_TEMPERATURE:-}

mkdir -p "$OUT_DIR"
echo "[info] OUT_DIR=$OUT_DIR ROOT_CSA=$ROOT_CSA TARGET_UNIQ=$TARGET_UNIQ"
echo "[info] rates O=$O_RATE M=$M_RATE E=$E_RATE caps O=$O_CAP M=$M_CAP E=$E_CAP"
echo "[info] annotate: time_ms=$TIME_MS jobs=$JOBS hash_mb=$HASH_MB"

### 1) Extract O/M/E
echo "[1/6] extract O/M/E from CSA"
cargo run -p tools --release --bin floodgate_pipeline -- \
  extract --root "$ROOT_CSA" --out "$OUT_DIR/open.sfens" \
  --mode all --min-ply 1   --max-ply 20  --per-game-cap "$O_CAP"

cargo run -p tools --release --bin floodgate_pipeline -- \
  extract --root "$ROOT_CSA" --out "$OUT_DIR/mid.sfens" \
  --mode all --min-ply 21  --max-ply 120 --per-game-cap "$M_CAP"

cargo run -p tools --release --bin floodgate_pipeline -- \
  extract --root "$ROOT_CSA" --out "$OUT_DIR/end.sfens" \
  --mode all --min-ply 121 --max-ply 400 --per-game-cap "$E_CAP"

### 2) Unique
echo "[2/6] unique"
LC_ALL=C sort -u "$OUT_DIR/open.sfens" -o "$OUT_DIR/open.unique.sfens" || true
LC_ALL=C sort -u "$OUT_DIR/mid.sfens"  -o "$OUT_DIR/mid.unique.sfens"  || true
LC_ALL=C sort -u "$OUT_DIR/end.sfens"  -o "$OUT_DIR/end.unique.sfens"  || true

open_n=$(wc -l < "$OUT_DIR/open.unique.sfens" || echo 0)
mid_n=$(wc -l < "$OUT_DIR/mid.unique.sfens"  || echo 0)
end_n=$(wc -l < "$OUT_DIR/end.unique.sfens"  || echo 0)
echo "[counts] open=$open_n mid=$mid_n end=$end_n"

### 3) Sampling to target
echo "[3/6] sampling to target=$TARGET_UNIQ"
shuf_seed() { yes 42 | head -c 1048576; }

o_target=$(python3 - <<PY
import math,os
print(int(round(float(os.environ.get('TARGET_UNIQ','1000000'))*float(os.environ.get('O_RATE','0.4')))))
PY
)
m_target=$(python3 - <<PY
import math,os
print(int(round(float(os.environ.get('TARGET_UNIQ','1000000'))*float(os.environ.get('M_RATE','0.5')))))
PY
)
e_target=$(python3 - <<PY
import math,os
print(int(round(float(os.environ.get('TARGET_UNIQ','1000000'))*float(os.environ.get('E_RATE','0.1')))))
PY
)

o_take=$(( open_n<o_target ? open_n : o_target ))
m_take=$(( mid_n<m_target ? mid_n : m_target ))
e_take=$(( end_n<e_target ? end_n : e_target ))

[ "$o_take" -gt 0 ] && shuf --random-source=<(shuf_seed) -n "$o_take" "$OUT_DIR/open.unique.sfens" > "$OUT_DIR/open.sample.sfens" || :
[ "$m_take" -gt 0 ] && shuf --random-source=<(shuf_seed) -n "$m_take" "$OUT_DIR/mid.unique.sfens"  > "$OUT_DIR/mid.sample.sfens"  || :
[ "$e_take" -gt 0 ] && shuf --random-source=<(shuf_seed) -n "$e_take" "$OUT_DIR/end.unique.sfens"  > "$OUT_DIR/end.sample.sfens"  || :

cat "$OUT_DIR"/*.sample.sfens 2>/dev/null | LC_ALL=C sort -u > "$OUT_DIR/train.sfens" || true
wc -l "$OUT_DIR/train.sfens" || true

awk '{print "sfen "$0}' "$OUT_DIR/train.sfens" > "$OUT_DIR/train.sfenl"

# If TIME_MS_END is specified, split end positions into its own file for separate annotation
if [[ -n "${TIME_MS_END}" ]]; then
  awk '{print "sfen "$0}' "$OUT_DIR/end.sample.sfens" > "$OUT_DIR/end.sfenl"
  awk '{print "sfen "$0}' "$OUT_DIR/open.sample.sfens" > "$OUT_DIR/open.sfenl" || true
  awk '{print "sfen "$0}' "$OUT_DIR/mid.sample.sfens"  > "$OUT_DIR/mid.sfenl"  || true
  cat "$OUT_DIR/open.sfenl" "$OUT_DIR/mid.sfenl" 2>/dev/null > "$OUT_DIR/openmid.sfenl" || true
fi

### 4) Annotate (JSONL)
echo "[4/6] annotate JSONL (WDL, MultiPV=${MULTIPV}, time=${TIME_MS}ms${TIME_MS_END:+, end=${TIME_MS_END}ms})"
if [[ -n "${TIME_MS_END}" ]]; then
  # Open+Mid at TIME_MS
  if [[ -s "$OUT_DIR/openmid.sfenl" ]]; then
    cargo run -p tools --release --bin generate_nnue_training_data -- \
      "$OUT_DIR/openmid.sfenl" "$OUT_DIR/train.openmid.jsonl" \
      3 256 0 --engine enhanced \
      --label wdl --wdl-scale 600 --multipv "$MULTIPV" \
      --time-limit-ms "$TIME_MS" \
      --jobs "$JOBS" --split 200000 \
      --reuse-tt --hash-mb "$HASH_MB" \
      --output-format jsonl $EXTRA_ANNOTATE_ARGS \
      --structured-log "$OUT_DIR/train.openmid.manifest.json"
  fi
  # End at TIME_MS_END
  if [[ -s "$OUT_DIR/end.sfenl" ]]; then
    cargo run -p tools --release --bin generate_nnue_training_data -- \
      "$OUT_DIR/end.sfenl" "$OUT_DIR/train.end.jsonl" \
      3 256 0 --engine enhanced \
      --label wdl --wdl-scale 600 --multipv "$MULTIPV" \
      --time-limit-ms "$TIME_MS_END" \
      --jobs "$JOBS" --split 200000 \
      --reuse-tt --hash-mb "$HASH_MB" \
      --output-format jsonl $EXTRA_ANNOTATE_ARGS \
      --structured-log "$OUT_DIR/train.end.manifest.json"
  fi
  # Merge
  echo "[merge] openmid/end -> train.jsonl"
  cat "$OUT_DIR"/train.openmid.jsonl "$OUT_DIR"/train.end.jsonl 2>/dev/null > "$OUT_DIR/train.jsonl" || true
else
  cargo run -p tools --release --bin generate_nnue_training_data -- \
    "$OUT_DIR/train.sfenl" "$OUT_DIR/train.jsonl" \
    3 256 0 --engine enhanced \
    --label wdl --wdl-scale 600 --multipv "$MULTIPV" \
    --time-limit-ms "$TIME_MS" \
    --jobs "$JOBS" --split 200000 \
    --reuse-tt --hash-mb "$HASH_MB" \
    --output-format jsonl $EXTRA_ANNOTATE_ARGS \
    --structured-log "$OUT_DIR/train.manifest.json"
fi

if ls "$OUT_DIR"/train.part-*.jsonl >/dev/null 2>&1; then
  echo "[merge] train.part-*.jsonl -> train.jsonl"
  cat "$OUT_DIR"/train.part-*.jsonl > "$OUT_DIR/train.jsonl"
fi

### 5) Build cache (gzip)
echo "[5/6] build_feature_cache (gzip)"
cargo run -p tools --release --bin build_feature_cache -- \
  -i "$OUT_DIR/train.jsonl" -o "$OUT_DIR/train.cache" \
  -l wdl --compress --compressor gz --compress-level 6 \
  --io-buf-mb 8 --metrics-interval 20000 --report-rss

### 6) Train Single (1 epoch, stream-cache)
echo "[6/6] train Single (stream-cache)"
# Detect validation cache (latest fixed)
VALCACHE=${VALCACHE:-$(ls -1dt runs/fixed/*/val.cache 2>/dev/null | head -n 1 || true)}
if [[ -z "${VALCACHE}" || ! -f "$VALCACHE" ]]; then
  echo "[warn] validation cache not found; training without --validation"
  VALID_ARGS=()
else
  echo "[info] using validation: $VALCACHE"
  VALID_ARGS=(--validation "$VALCACHE")
fi

cargo run -p tools --release --bin train_nnue -- \
  --input "$OUT_DIR/train.cache" "${VALID_ARGS[@]}" \
  --arch single --epochs "$EPOCHS" --batch-size "$BATCH_SIZE" \
  --lr 1e-3 --metrics --seed 42 \
  --stream-cache --prefetch-batches 4 --throughput-interval 2.0 \
  --out "$OUT_DIR/single_v1"

if [[ "$DO_CLASSIC" == "1" ]]; then
  echo "[opt] classic distillation"
  KD_ARGS=()
  if [[ -n "$KD_ALPHA" ]]; then KD_ARGS+=(--kd-alpha "$KD_ALPHA"); fi
  if [[ -n "$KD_TEMPERATURE" ]]; then KD_ARGS+=(--kd-temperature "$KD_TEMPERATURE"); fi
  cargo run -p tools --release --bin train_nnue -- \
    --input "$OUT_DIR/train.cache" "${VALID_ARGS[@]}" \
    --arch classic --distill-from-single "$OUT_DIR/single_v1/nn.fp32.bin" \
    --teacher-domain wdl-logit --teacher-scale-fit "$TEACHER_SCALE_FIT" "${KD_ARGS[@]}" \
    --epochs 1 --batch-size "$BATCH_SIZE" --lr 8e-4 \
    --export-format classic-v1 --metrics --seed 42 \
    --out "$OUT_DIR/classic_v1"
fi

echo "[done] phase1 batch completed: $OUT_DIR"
