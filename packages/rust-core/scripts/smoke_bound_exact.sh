#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=target/release/engine-usi
LOG_DIR=.smoke
LOG="$LOG_DIR/bound_exact.log"
mkdir -p "$LOG_DIR"

echo "[bound] build" >&2
cargo build -p engine-usi --release >/dev/null

echo "[bound] run movetime=1000 (MultiPV=1)" >&2
{
  echo usi
  echo isready
  echo "setoption name Threads value 1"
  echo "setoption name MultiPV value 1"
  echo isready
  echo "position startpos"
  echo "go movetime 1000"
  sleep 2
  echo quit
} | stdbuf -oL -eL "$BIN" | tee "$LOG" >/dev/null

last_info=$(rg -n "^info .* pv " "$LOG" -r '$0' | tail -n1 || true)
[[ -n "${last_info:-}" ]] || { echo "[bound] NG: 最終PV行が見つかりません" >&2; exit 1; }
echo "$last_info" | rg -q "\b(lowerbound|upperbound)\b" && { echo "[bound] NG: 最終行が Exact ではありません" >&2; exit 1; }

echo "[bound] OK: 最終情報行は Exact"
