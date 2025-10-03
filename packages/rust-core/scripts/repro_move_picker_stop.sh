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
    kill 0 2>/dev/null || true
}
trap cleanup EXIT INT TERM

run_engine() {
    timeout 20s stdbuf -oL -eL "$BIN"
}

wait_for() {
    local pattern="$1"
    local timeout_seconds="${2:-5}"
    if ! timeout "$timeout_seconds" sh -c "tail -F -n0 -- '$LOG' | stdbuf -oL grep -m1 -- '$pattern'" >/dev/null 2>&1; then
        echo "[repro] ERROR: timeout while waiting for pattern '$pattern'" >&2
        return 1
    fi
    return 0
}

exec {ENGINE_IN}> >(run_engine | tee "$LOG")
ENGINE_PID=$!

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
wait_for "info string currmove" 5

send "stop"
if ! wait_for "bestmove" 10; then
    echo "[repro] WARN: bestmove not observed after stop binnen 10s" >&2
fi

send "quit"
wait "$ENGINE_PID" || true

echo "[repro] session log captured at $LOG" >&2
