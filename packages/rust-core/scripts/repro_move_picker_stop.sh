#!/usr/bin/env bash
set -euo pipefail

# Reproduce the 2025-10-03 17:28:53 stop hang reported post MovePicker refactor.
# Sequence:
#   1. initial go with byoyomi only -> expect bestmove in <100ms
#   2. issue ponder search on the replied line
#   3. wait ~600ms then send stop
# Observation (current bug): engine keeps thinking, no stop info emitted, quit only once GUI forces timeout.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=${BIN:-target/release/engine-usi}
LOG_DIR=.repro
LOG="$LOG_DIR/move_picker_stop.log"
mkdir -p "$LOG_DIR"

FIRST_WAIT=${FIRST_WAIT:-0.80}
PONDER_WAIT=${PONDER_WAIT:-0.60}
AFTER_STOP_WAIT=${AFTER_STOP_WAIT:-3}

if [[ ! -x "$BIN" ]]; then
    echo "[repro] building engine-usi (release)" >&2
    cargo build -p engine-usi --release >/dev/null
fi

echo "[repro] running move-picker stop hang scenario" >&2
{
    echo usi
    echo isready
    echo "setoption name Threads value 1"
    echo "setoption name MultiPV value 1"
    echo isready
    echo usinewgame
    echo "position startpos"
    echo "go btime 0 wtime 0 byoyomi 10000"
    sleep "$FIRST_WAIT"
    echo "position startpos moves 5i5h 5a6b"
    echo "go ponder btime 0 wtime 0 byoyomi 10000"
    sleep "$PONDER_WAIT"
    echo stop
    sleep "$AFTER_STOP_WAIT"
    echo quit
} | stdbuf -oL -eL "$BIN" | tee "$LOG" >/dev/null

echo "[repro] session log captured at $LOG" >&2
