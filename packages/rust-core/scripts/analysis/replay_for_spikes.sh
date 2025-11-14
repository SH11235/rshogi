#!/usr/bin/env bash
set -euo pipefail

# replay_for_spikes.sh
# - Extract spike prefixes from a USI log, then run replay_multipv.sh once
#   with those prefixes and the given USI options.
#
# Usage:
#   scripts/analysis/replay_for_spikes.sh <usi_log> \
#     --out runs/game-postmortem/$(date +%Y%m%d)-spikes \
#     [--threshold 300] [--back 3] [--forward 2] \
#     [--threads 8] [--multipv 1] [--byoyomi 10000] [--engine target/release/engine-usi] \
#     [--profile match|postmortem] [--inherit-setoptions|--no-inherit-setoptions] [--evalfile PATH] \
#     [--extra-setoption "<NAME ... value ...>"]

if [ $# -lt 1 ]; then
  echo "Usage: $0 <usi_log> [--out DIR] [--threshold N] [--back N] [--forward N] [--threads N] [--multipv N] [--byoyomi MS] [--engine BIN] [--profile match|postmortem] [--inherit-setoptions|--no-inherit-setoptions] [--evalfile PATH] [--extra-setoption '<NAME ... value ...>']" >&2
  exit 1
fi

LOG="$1"; shift
OUT="runs/game-postmortem/$(date +%Y%m%d)-spikes"
THRESH=300
BACK=3
FWD=2
THREADS=8
MULTIPV=1
BYO=10000
ENGINE="target/release/engine-usi"
PROFILE="match"
INHERIT_SETOPTIONS=1
EVALFILE=""
EXTRA_SETOPTS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2;;
    --threshold) THRESH="$2"; shift 2;;
    --back) BACK="$2"; shift 2;;
    --forward) FWD="$2"; shift 2;;
    --threads) THREADS="$2"; shift 2;;
    --multipv) MULTIPV="$2"; shift 2;;
    --byoyomi) BYO="$2"; shift 2;;
    --engine) ENGINE="$2"; shift 2;;
    --profile) PROFILE="$2"; shift 2;;
    --inherit-setoptions) INHERIT_SETOPTIONS=1; shift 1;;
    --no-inherit-setoptions) INHERIT_SETOPTIONS=0; shift 1;;
    --evalfile) EVALFILE="$2"; shift 2;;
    --extra-setoption) EXTRA_SETOPTS+=("$2"); shift 2;;
    *) echo "Unknown arg: $1" >&2; exit 1;;
  esac
done

python3 scripts/analysis/extract_eval_spikes.py "$LOG" --threshold "$THRESH" --back "$BACK" --forward "$FWD" --out "$OUT" >/dev/null
PREFIXES=$(cat "$OUT/prefixes.txt")

echo "[spikes] prefixes: $PREFIXES" >&2

scripts/analysis/replay_multipv.sh "$LOG" -p "$PREFIXES" -e "$ENGINE" -o "$OUT/replay" -m "$MULTIPV" -t "$THREADS" -b "$BYO" \
  --profile "$PROFILE" $( [ $INHERIT_SETOPTIONS -eq 1 ] && echo "--inherit-setoptions" || echo "--no-inherit-setoptions" ) \
  $( [ -n "$EVALFILE" ] && echo "--evalfile $EVALFILE" ) \
  $( for o in "${EXTRA_SETOPTS[@]:-}"; do echo --extra-setoption "$o"; done )

echo "Wrote: $OUT/replay/summary.txt" >&2
