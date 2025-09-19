#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}" )/../../../.." && pwd)"
OUT_DIR=${1:-"${ROOT_DIR}/target/classic_roundtrip_smoke"}
PROFILE=${CARGO_PROFILE:-release}
PROFILE_FLAG=""
if [[ "${PROFILE}" == "release" ]]; then
  PROFILE_FLAG="--release"
fi

SINGLE_OUT="${OUT_DIR}/single_teacher"
TRAIN_OUT="${OUT_DIR}/export"
mkdir -p "${SINGLE_OUT}" "${TRAIN_OUT}"

DATA_TRAIN="${ROOT_DIR}/docs/reports/fixtures/classic_roundtrip/train.jsonl"
DATA_VAL="${ROOT_DIR}/docs/reports/fixtures/classic_roundtrip/val.jsonl"
POSITIONS_FILE="${ROOT_DIR}/docs/reports/fixtures/classic_roundtrip/positions.sfen"

TEACHER_PATH="${SINGLE_OUT}/nn.fp32.bin"
if [[ ! -f "${TEACHER_PATH}" || -n "${FORCE_REBUILD_TEACHER:-}" ]]; then
  echo "[classic-roundtrip] training Single teacher -> ${SINGLE_OUT}" >&2
  cargo run ${PROFILE_FLAG} -p tools --bin train_nnue -- \
    --input "${DATA_TRAIN}" \
    --validation "${DATA_VAL}" \
    --arch single \
    --label cp \
    --epochs 1 \
    --batch-size 32 \
    --opt sgd \
    --rng-seed 1337 \
    --export-format fp32 \
    --metrics \
    --out "${SINGLE_OUT}" ${TEACHER_EXTRA_ARGS:-}
fi

if [[ ! -f "${TEACHER_PATH}" ]]; then
  echo "[classic-roundtrip] teacher network missing at ${TEACHER_PATH}" >&2
  exit 1
fi

echo "[classic-roundtrip] training Classic network -> ${TRAIN_OUT}" >&2
cargo run ${PROFILE_FLAG} -p tools --bin train_nnue -- \
  --input "${DATA_TRAIN}" \
  --validation "${DATA_VAL}" \
  --arch classic \
  --label cp \
  --epochs 1 \
  --batch-size 32 \
  --opt sgd \
  --rng-seed 42 \
  --export-format classic-v1 \
  --emit-fp32-also \
  --distill-from-single "${TEACHER_PATH}" \
  --teacher-domain cp \
  --metrics \
  --out "${TRAIN_OUT}" ${TRAIN_EXTRA_ARGS:-}

FP32_PATH="${TRAIN_OUT}/nn.fp32.bin"
INT_PATH="${TRAIN_OUT}/nn.classic.nnue"
SCALES_PATH="${TRAIN_OUT}/nn.classic.scales.json"

if [[ ! -f "${FP32_PATH}" || ! -f "${INT_PATH}" || ! -f "${SCALES_PATH}" ]]; then
  echo "[classic-roundtrip] expected export artifacts not found" >&2
  exit 1
fi

REPORT_JSON="${OUT_DIR}/roundtrip.json"
WORST_JSONL="${OUT_DIR}/worst.jsonl"

echo "[classic-roundtrip] verifying round-trip -> ${REPORT_JSON}" >&2
cargo run ${PROFILE_FLAG} -p tools --bin verify_classic_roundtrip -- \
  --fp32 "${FP32_PATH}" \
  --int "${INT_PATH}" \
  --scales "${SCALES_PATH}" \
  --positions "${POSITIONS_FILE}" \
  --metric cp \
  --max-abs 400.0 \
  --mean-abs 150.0 \
  --p95-abs 250.0 \
  --worst-count 20 \
  --out "${REPORT_JSON}" \
  --worst-jsonl "${WORST_JSONL}" ${VERIFY_EXTRA_ARGS:-}

echo "[classic-roundtrip] completed. Report: ${REPORT_JSON}" >&2
