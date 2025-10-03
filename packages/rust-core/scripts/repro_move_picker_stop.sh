#!/usr/bin/env bash
set -euo pipefail

# Reproduce the 2025-10-03 stop hang after MovePicker refactor while keeping execution deterministic.
# Improvements over the initial draft:
#   * Synchronizes on engine output instead of fixed sleeps
#   * Wraps execution with timeout and ensures child process cleanup
#   * Increases Rust thread stack to avoid spurious stack-overflow during diagnosis

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=${BIN:-target/release/engine-usi}
LOG_DIR=.repro
LOG="$LOG_DIR/move_picker_stop.log"
mkdir -p "$LOG_DIR"

export RUST_MIN_STACK=${RUST_MIN_STACK:-8388608}
if command -v ulimit >/dev/null 2>&1; then
    ulimit -s 16384 || true
fi

if [[ ! -x "$BIN" ]]; then
    echo "[repro] building engine-usi (release)" >&2
    cargo build -p engine-usi --release >/dev/null
fi

cleanup() {
    if [[ -n ${ENGINE_IN:-} ]]; then
        exec {ENGINE_IN}>&-
    fi
    if [[ -n ${ENGINE_PID:-} ]]; then
        kill "$ENGINE_PID" 2>/dev/null || true
        wait "$ENGINE_PID" 2>/dev/null || true
    fi
    if [[ -n ${LOGGER_PID:-} ]]; then
        kill "$LOGGER_PID" 2>/dev/null || true
        wait "$LOGGER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

wait_for() {
    local pattern="$1"
    local timeout_seconds="${2:-5}"
    if [[ ! -p ${LOG}.fifo ]]; then
        mkfifo ${LOG}.fifo
    fi
    tail -F -n0 -- "$LOG" | stdbuf -oL grep -F --line-buffered -- "$pattern" >"${LOG}.fifo" &
    local grep_pid=$!
    if ! timeout "$timeout_seconds" head -n1 "${LOG}.fifo" >/dev/null 2>&1; then
        echo "[repro] ERROR: timeout while waiting for pattern '$pattern'" >&2
        kill "$grep_pid" 2>/dev/null || true
        wait "$grep_pid" 2>/dev/null || true
        rm -f "${LOG}.fifo"
        return 1
    fi
    kill "$grep_pid" 2>/dev/null || true
    wait "$grep_pid" 2>/dev/null || true
    rm -f "${LOG}.fifo"
    return 0
}

: >"$LOG"
coproc ENGINE { timeout 30s stdbuf -oL -eL "$BIN"; }
if [[ -n ${ENGINE_PID-} ]]; then
    ENGINE_MAIN_PID=$ENGINE_PID
elif [[ -n ${COPROC_PID-} ]]; then
    ENGINE_MAIN_PID=$COPROC_PID
else
    echo "[repro] ERROR: coproc PID was not assigned" >&2
    exit 1
fi
ENGINE_PID=$ENGINE_MAIN_PID
cat <&${ENGINE[0]} | tee "$LOG" &
LOGGER_PID=$!
exec {ENGINE_IN}>&${ENGINE[1]}

send() {
    printf '%s\n' "$1" >&${ENGINE_IN}
}

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
send "go btime 0 wtime 0 byoyomi 10000"
wait_for "info string search_started" 5
wait_for "bestmove" 10

send "position startpos moves 5i5h 5a6b"
send "go ponder btime 0 wtime 0 byoyomi 10000"
wait_for "info string search_started" 5
if ! wait_for "info string currmove" 5; then
    wait_for "info depth" 5
fi

send "stop"
if ! wait_for "bestmove" 10; then
    echo "[repro] WARN: bestmove not observed after stop binnen 10s" >&2
fi

send "quit"
wait "$ENGINE_PID" || true
kill "$LOGGER_PID" 2>/dev/null || true


if jobs -p &>/dev/null; then
    wait 2>/dev/null || true
fi

echo "[repro] session log captured at $LOG" >&2
