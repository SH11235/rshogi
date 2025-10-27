#!/usr/bin/env bash
set -euo pipefail

# Quantize (per-channel, 120k) and evaluate with sharded gauntlet
# Usage: scripts/nnue/quant_and_sharded_eval.sh OUT_DIR [SHORT_SHARDS=16] [LONG_SHARDS=16]

DIR=${1:?OUT_DIR}
SHORT_SHARDS=${2:-16}
LONG_SHARDS=${3:-16}
VALC=runs/fixed/20251011/val.cache
BOOK=runs/fixed/20251011/openings_ply1_20_v1.sfen
BASE=runs/ref.nnue

CAND_FP32="$DIR/classic_v1/nn_best.fp32.bin"
while [ ! -f "$CAND_FP32" ]; do sleep 60; done

# 1) Quantize (per-channel fixed, 120k)
cargo run -p tools --release --bin train_nnue -- \
  --input "$DIR/train.cache" --arch classic \
  --distill-from-classic "$CAND_FP32" --distill-only \
  --export-format classic-v1 \
  --quant-calibration "$VALC" "$DIR/train.cache" \
  --quant-calibration-limit 120000 \
  --quant-ft per-tensor --quant-h1 per-channel --quant-h2 per-channel --quant-out per-tensor \
  --out "$DIR/classic_v1_q_pc_120k"

CAND_INT="$DIR/classic_v1_q_pc_120k/nn.classic.nnue"

# 2) ShortTC 800 (sharded)
S800_OUT="$DIR/gauntlet_shorttc_q_pc_120k_sharded_800"
mkdir -p "$S800_OUT"
scripts/nnue/gauntlet-sharded.sh "$BASE" "$CAND_INT" 800 "$SHORT_SHARDS" "0/10+0.1" "$S800_OUT" "$BOOK"
scripts/nnue/merge-gauntlet-json.sh "$S800_OUT"

S800_JSON="$S800_OUT/merged.result.json"
WIN=$(jq -r '.winrate' "$S800_JSON" 2>/dev/null || echo 0)
if awk "BEGIN{exit !($WIN>=0.55)}"; then
  # 3) ShortTC 2000 (sharded)
  S2K_OUT="$DIR/gauntlet_shorttc_q_pc_120k_sharded_2000"
  mkdir -p "$S2K_OUT"
  scripts/nnue/gauntlet-sharded.sh "$BASE" "$CAND_INT" 2000 "$SHORT_SHARDS" "0/10+0.1" "$S2K_OUT" "$BOOK"
  scripts/nnue/merge-gauntlet-json.sh "$S2K_OUT"

  # 4) LongTC 800 (sharded)
  L800_OUT="$DIR/gauntlet_longtc_q_pc_120k_sharded_800"
  mkdir -p "$L800_OUT"
  scripts/nnue/gauntlet-sharded.sh "$BASE" "$CAND_INT" 800 "$LONG_SHARDS" "0/40+0.4" "$L800_OUT" "$BOOK"
  scripts/nnue/merge-gauntlet-json.sh "$L800_OUT"
fi

# 5) PV probe (補助)
cargo build -p tools --release --bin pv_probe >/dev/null
target/release/pv_probe --cand "$CAND_INT" --book "$BOOK" \
  --depth 8 --threads 1 --hash-mb 512 --samples 200 --seed 42 \
  --json "$DIR/classic_v1_q_pc_120k/pv_probe_d8_s200.json"

echo "[done] quant+sharded eval: $DIR"

