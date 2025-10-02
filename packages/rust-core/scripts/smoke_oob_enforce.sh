#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=target/release/engine-usi
LOG_DIR=.smoke
LOG="$LOG_DIR/oob_enforce.log"
mkdir -p "$LOG_DIR"

echo "[oob] build" >&2
cargo build -p engine-usi --release >/dev/null

echo "[oob] run byoyomi=2000" >&2
{
  echo usi
  echo isready
  echo "setoption name Threads value 1"
  echo "setoption name MultiPV value 1"
  echo isready
  echo "position startpos"
  echo "go btime 0 wtime 0 byoyomi 2000"
  sleep 4
  echo quit
} | stdbuf -oL -eL "$BIN" | tee "$LOG" >/dev/null

# enforce_deadline が発火して bestmove 1回のみ
rg -n "oob_finalize_request reason=Hard" "$LOG" >/dev/null || { echo "[oob] NG: Hard finalize 未検出" >&2; exit 1; }
bcount=$(rg -n "^bestmove " "$LOG" | wc -l | tr -d ' ')
[[ "$bcount" == 1 ]] || { echo "[oob] NG: bestmove=$bcount" >&2; exit 1; }

echo "[oob] OK"
