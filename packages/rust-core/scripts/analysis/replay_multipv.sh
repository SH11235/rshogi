#!/usr/bin/env bash
set -euo pipefail

# replay_multipv.sh
# - Extracts the final "position startpos moves ..." from a USI log
# - Replays specified prefixes with engine-usi under MultiPV/Byoyomi settings
# - Emits raw logs per prefix and a short summary
#
# Usage:
#   scripts/analysis/replay_multipv.sh <usi_log> \
#     [-p "28 30 32" ] [-e target/release/engine-usi] \
#     [-o runs/game-postmortem/$(date +%Y%m%d)-5s] \
#     [-m 1] [-t 8] [-b 5000] \
#     [--profile match|postmortem] [--inherit-setoptions|--no-inherit-setoptions] [--evalfile PATH] \
#     [--extra-setoption "<NAME ... value ...>"]
#
# Notes:
# - If nn.classic_v8.nnue exists in CWD, it's passed via EvalFile option.
# - The engine is run with a small set of mate/sanity options that are useful for post-mortem.

usage() {
  echo "Usage: $0 <usi_log> [-p \"28 30 32\"] [-e engine] [-o out_dir] [-m multipv] [-t threads] [-b byoyomi_ms] [--profile match|postmortem] [--inherit-setoptions|--no-inherit-setoptions] [--evalfile PATH] [--extra-setoption '<NAME ... value ...>']" >&2
}

if [ ${#} -lt 1 ]; then
  usage; exit 1
fi

LOG="$1"; shift || true
PREFIX_LIST="28 30 32 36 38"
ENGINE="target/release/engine-usi"
OUT_DIR="runs/game-postmortem/$(date +%Y%m%d)-10s"
MULTIPV=1
THREADS=8
BYO_MS=10000
PROFILE="match"        # match | postmortem
INHERIT_SETOPTIONS=1    # 1: apply setoptions from USI log; 0: do not
EVALFILE=""
EXTRA_SETOPTS=()

while [ ${#} -gt 0 ]; do
  case "$1" in
    -p|--prefix)
      PREFIX_LIST="$2"; shift 2;;
    -e|--engine)
      ENGINE="$2"; shift 2;;
    -o|--out)
      OUT_DIR="$2"; shift 2;;
    -m|--multipv)
      MULTIPV="$2"; shift 2;;
    -t|--threads)
      THREADS="$2"; shift 2;;
    -b|--byoyomi)
      BYO_MS="$2"; shift 2;;
    --profile)
      PROFILE="$2"; shift 2;;
    --inherit-setoptions)
      INHERIT_SETOPTIONS=1; shift 1;;
    --no-inherit-setoptions)
      INHERIT_SETOPTIONS=0; shift 1;;
    --evalfile)
      EVALFILE="$2"; shift 2;;
    --extra-setoption)
      EXTRA_SETOPTS+=("$2"); shift 2;;
    -h|--help)
      usage; exit 0;;
    *)
      echo "Unknown arg: $1" >&2; usage; exit 1;;
  esac
done

mkdir -p "$OUT_DIR"

if ! command -v rg >/dev/null 2>&1; then
  echo "ripgrep (rg) is required" >&2; exit 1
fi

MOVES_LINE=$(rg -n "position startpos moves" "$LOG" | tail -n 1 | sed 's/^.*position startpos moves //')
if [ -z "$MOVES_LINE" ]; then
  echo "No 'position startpos moves' found in $LOG" >&2; exit 1
fi

parse_moves() {
  local upto=$1
  awk -v TOKENS="$MOVES_LINE" -v UPTO="$upto" 'BEGIN{split(TOKENS,a," "); for(i=1;i<=UPTO;i++){printf("%s%s", a[i], (i<UPTO?" ":""));}}'
}

SUMMARY="$OUT_DIR/summary.txt"
echo "# replay_multipv summary" > "$SUMMARY"
echo "engine: $ENGINE" >> "$SUMMARY"
echo "threads=$THREADS multipv=$MULTIPV byoyomi=${BYO_MS}ms" >> "$SUMMARY"
echo >> "$SUMMARY"

for P in $PREFIX_LIST; do
  MOVES=$(parse_moves "$P")
  TAG="pre-${P}"
  RAW="$OUT_DIR/${TAG}.log"
  echo "[run] $TAG -> $RAW" >&2
  (
    echo usi
    echo isready
    if [ "$INHERIT_SETOPTIONS" -eq 1 ]; then
      # Reproduce the same engine profile as the match by inheriting setoptions from the log
      rg -n "setoption name" "$LOG" | sed 's/^.*> //'
    fi
    # Apply explicit overrides after inheritance
    echo "setoption name Threads value ${THREADS}"
    echo "setoption name MultiPV value ${MULTIPV}"
    if [ -n "$EVALFILE" ]; then
      echo "setoption name EvalFile value ${EVALFILE}"
    fi
    for OPT in "${EXTRA_SETOPTS[@]:-}"; do
      [ -n "$OPT" ] && echo "setoption name ${OPT}"
    done
    if [ "$PROFILE" = "postmortem" ]; then
      # Optional helpers useful for debugging only. Do not enable for strict match.
      echo "setoption name InstantMateMove.MaxDistance value 2"
      echo "setoption name InstantMateMove.VerifyMode value QSearch"
      echo "setoption name FinalizeSanity.MateProbe.Enabled value true"
      echo "setoption name FinalizeSanity.MateProbe.Depth value 6"
      echo "setoption name FinalizeSanity.MateProbe.TimeMs value 15"
    fi
    echo isready
    echo "position startpos moves ${MOVES}"
    echo "go byoyomi ${BYO_MS}"
  ) | stdbuf -oL -eL timeout 20s "$ENGINE" 2>&1 | tee "$RAW" >/dev/null || true

  BEST=$(rg -n "^bestmove" "$RAW" | tail -n 1 | sed 's/^.*bestmove //') || BEST="(none)"
  LAST_INFO=$(rg -n "^info depth" "$RAW" | tail -n 1 | sed 's/^.*info //') || LAST_INFO="(none)"
  echo "${TAG}: bestmove=${BEST}" >> "$SUMMARY"
  echo "${TAG}: last_info=${LAST_INFO}" >> "$SUMMARY"
  echo >> "$SUMMARY"
done

echo "Wrote: $SUMMARY" >&2
