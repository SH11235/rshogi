#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/bench/bench_aspiration_diff.sh

# Run fixed-position thinking sessions for given log (first-line USI) and
# compare aspiration failure frequency, depth, and PV stability between
# single-thread and SMP (8 threads).
#
# Usage:
#   scripts/bench_aspiration_diff.sh taikyoku_log_YYYYMMDDHHMM.md [runs] [cut_at]
#

LOG_FILE=${1:-taikyoku_log_202510090836.md}
RUNS=${2:-3}
CUT_AT_ARG=${3:-}

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

echo "[bench] building engine-usi (release)" >&2
cargo build -q -p engine-usi --release

mkdir -p runs/repro

for t in 1 8; do
  for r in $(seq 1 ${RUNS}); do
    echo "[bench] run t=${t} r=${r}" >&2
    LOG_FILE="$LOG_FILE" THREADS="$t" RUN_IDX="$r" CUT_AT="$CUT_AT_ARG" \
      bash scripts/repro_baseline.sh "$t" "$r" >/dev/null 2>&1 || true
  done
done

echo "[bench] analyzing logs in runs/repro (baseline_*.log only)" >&2
targets=(runs/repro/baseline_threads1_run*.log runs/repro/baseline_threads8_run*.log)
bash scripts/analyze_usi_logs.sh "${targets[@]}" > runs/repro/aspiration_summary.csv

echo "[bench] summary written to runs/repro/aspiration_summary.csv" >&2
cat runs/repro/aspiration_summary.csv
