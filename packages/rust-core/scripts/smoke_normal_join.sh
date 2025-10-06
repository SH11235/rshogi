#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=target/release/engine-usi
LOG_DIR=.smoke
LOG="$LOG_DIR/normal_join.log"
mkdir -p "$LOG_DIR"

echo "[join] build" >&2
cargo build -p engine-usi --release >/dev/null

echo "[join] run movetime=1000 (ample time before hard deadline)" >&2
{
  echo usi
  echo isready
  echo "setoption name Threads value 1"
  echo "setoption name MultiPV value 1"
  echo isready
  echo "position startpos"
  echo "go movetime 1000"
  # go movetime 1000 でも NearHard/OOB finalize が発火する設計なので、
  # エンジンが結果を確定するまで十分待機してから quit を送る。
  sleep 5
  echo quit
} | stdbuf -oL -eL "$BIN" | tee "$LOG" >/dev/null

rg -n "^bestmove " "$LOG" >/dev/null || { echo "[join] NG: bestmove 未出力" >&2; exit 1; }

# 最終行 Exact
last_info=$(rg -n "^info .* pv " "$LOG" -r '$0' | tail -n1 || true)
[[ -n "${last_info:-}" ]] || { echo "[join] NG: 最終PV行が見つかりません" >&2; exit 1; }
echo "$last_info" | rg -q "\b(lowerbound|upperbound)\b" && { echo "[join] NG: 最終行が Exact ではありません" >&2; exit 1; }

echo "[join] OK"
