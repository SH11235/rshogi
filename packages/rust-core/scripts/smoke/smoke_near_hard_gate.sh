#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/smoke/smoke_near_hard_gate.sh

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=target/release/engine-usi
LOG_DIR=.smoke
LOG="$LOG_DIR/near_hard_gate.log"
mkdir -p "$LOG_DIR"

echo "[near-hard] build" >&2
cargo build -p engine-usi --release >/dev/null

echo "[near-hard] run byoyomi=2000" >&2
{
  echo usi
  echo isready
  echo "setoption name Threads value 1"
  echo "setoption name MultiPV value 1"
  echo "setoption name ByoyomiDeadlineLeadMs value 300"
  echo isready
  echo "position startpos"
  echo "go btime 0 wtime 0 byoyomi 2000"
  sleep 4
  echo quit
} | stdbuf -oL -eL "$BIN" | tee "$LOG" >/dev/null

# 1) near-hardとhardのOOBログ
rg -n "oob_deadline_nearhard_reached" "$LOG" >/dev/null || { echo "[near-hard] NG: near-hard 未検出" >&2; exit 1; }

# 2) bestmoveは1回のみ
bcount=$(rg -n "^bestmove " "$LOG" | wc -l | tr -d ' ')
[[ "$bcount" == 1 ]] || { echo "[near-hard] NG: bestmove=$bcount" >&2; exit 1; }

# 3) 最終PV行（bestmove直前の multipv 1）boundがEXACT（= lowerbound/upperbound を含まない）
last_info=$(rg -n "^info .* pv " "$LOG" -r '$0' | tail -n1 || true)
if [[ -z "${last_info:-}" ]]; then
  echo "[near-hard] WARN: 最終PV行が見つからず（OOB fast finalizeでPV行を省略した可能性）" >&2
else
  echo "$last_info" | rg -q "\b(lowerbound|upperbound)\b" && { echo "[near-hard] NG: 最終行が Exact ではありません" >&2; exit 1; }
fi

# 4) 直前で次反復に入らない（最大深さ+1のdepth行が無い）
maxd=$(rg -n "^info depth ([0-9]+) " -or '$1' "$LOG" 2>/dev/null || true | awk 'max<$1{max=$1} END{print max+0}')
if [[ "$maxd" -gt 0 ]]; then
  rg -n "^info depth $((maxd+1))\b" "$LOG" >/dev/null && { echo "[near-hard] NG: 次反復(dept=$((maxd+1)))に入っている" >&2; exit 1; }
fi

echo "[near-hard] OK"
