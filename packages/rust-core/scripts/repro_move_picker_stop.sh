#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=${BIN:-target/release/engine-usi}
LOG_DIR=.repro
LOG="$LOG_DIR/move_picker_stop.log"
mkdir -p "$LOG_DIR"
: >"$LOG"

if command -v timeout >/dev/null 2>&1; then
    TIMEOUT_CMD=${TIMEOUT_CMD:-timeout}
elif command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_CMD=${TIMEOUT_CMD:-gtimeout}
else
    echo "[repro] ERROR: neither 'timeout' nor 'gtimeout' is available" >&2
    exit 1
fi

export RUST_MIN_STACK=${RUST_MIN_STACK:-8388608}
if command -v ulimit >/dev/null 2>&1; then
    ulimit -s 16384 || true
fi

if [[ ! -x "$BIN" ]]; then
    echo "[repro] building engine-usi (release)" >&2
    cargo build -p engine-usi --release >/dev/null
fi

cleanup() {
    [[ -n ${ENGIN:-} ]] && exec {ENGIN}>&-
    [[ -n ${ENGOUT:-} ]] && exec {ENGOUT}<&-
    if [[ -n ${ENG_PID:-} ]]; then
        kill "$ENG_PID" 2>/dev/null || true
        wait "$ENG_PID" 2>/dev/null || true
    fi
    if [[ -n ${READER_PID:-} ]]; then
        kill "$READER_PID" 2>/dev/null || true
        wait "$READER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

send() {
    printf '%s\n' "$1" >&"$ENGIN"
}

wait_for() {
    local pattern="$1"
    local timeout="${2:-5}"
    local start=$(date +%s)
    while true; do
        if grep -Fq "$pattern" "$LOG"; then
            return 0
        fi
        if (( $(date +%s) - start >= timeout )); then
            echo "[repro] timeout waiting for '$pattern'" >&2
            return 1
        fi
        sleep 0.05
    done
}

coproc ENG { "$TIMEOUT_CMD" 60s stdbuf -oL -eL "$BIN"; }
ENG_PID=${ENG_PID:-$COPROC_PID}
exec {ENGIN}>&"${ENG[1]}"
exec {ENGOUT}<&"${ENG[0]}"

{
    while IFS= read -r line <&"$ENGOUT"; do
        printf '%s\n' "$line" | tee -a "$LOG"
    done
} & READER_PID=$!

send "usi"
wait_for "usiok" 5

send "isready"
wait_for "readyok" 5

send "setoption name Threads value 1"
send "setoption name MultiPV value 1"

send "isready"
wait_for "readyok" 5

send "usinewgame"
send "position startpos"
send "go btime 0 wtime 0 byoyomi 2000"
wait_for "bestmove" 10 || echo "[repro] WARN: no bestmove after first go" >&2

send "position startpos moves 5i5h 5a6b"
send "go ponder btime 0 wtime 0 byoyomi 2000"
if ! wait_for "info depth" 5 && ! wait_for "info currmove" 5; then
    echo "[repro] WARN: no ponder info observed" >&2
fi
send "stop"
wait_for "bestmove" 10 || echo "[repro] WARN: bestmove missing after stop" >&2

send "quit"
wait "$ENG_PID" 2>/dev/null || true
wait "$READER_PID" 2>/dev/null || true

echo "[repro] session log captured at $LOG" >&2
