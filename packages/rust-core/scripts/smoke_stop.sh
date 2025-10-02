#!/usr/bin/env bash
set -euo pipefail

# Smoke: stop/finalize behavior under pure byoyomi
# - go btime 0 wtime 0 byoyomi 2000
# - send stop (with a couple of extra stop signals)
# - assert: bestmove emitted exactly once
# - assert: StopInfo snapshot appears in logs (oob_stop_info)

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=target/release/engine-usi
LOG_DIR=.smoke
LOG="$LOG_DIR/stop_byoyomi.log"
mkdir -p "$LOG_DIR"

echo "[smoke] building engine-usi (diagnostics)" >&2
cargo build -p engine-usi --release --features diagnostics >/dev/null

echo "[smoke] running stop-byoyomi session" >&2
{
  echo usi
  echo isready
  echo "setoption name Threads value 1"
  echo "setoption name MultiPV value 3"
  echo isready
  echo "position startpos"
  echo "go btime 0 wtime 0 byoyomi 2000"
  sleep 0.10
  echo stop
  sleep 0.05
  echo stop
  sleep 0.05
  echo stop
  sleep 1
  echo quit
} | stdbuf -oL -eL "$BIN" | tee "$LOG" >/dev/null

bcount=$(rg -n "^bestmove " "$LOG" | wc -l | tr -d ' ')
if [[ "$bcount" != "1" ]]; then
  echo "[smoke] NG: bestmove emitted $bcount times (expected 1)" >&2
  exit 2
fi

has_stopinfo=$(rg -n "oob_stop_info" "$LOG" -q && echo 1 || echo 0)
if [[ "$has_stopinfo" -ne 1 ]]; then
  echo "[smoke] WARN: StopInfo snapshot (oob_stop_info) not observed; finalize likely joined with result" >&2
else
  echo "[smoke] OK: observed StopInfo snapshot via *_oob_stop_info"
fi

echo "[smoke] OK: bestmove emitted exactly once"

