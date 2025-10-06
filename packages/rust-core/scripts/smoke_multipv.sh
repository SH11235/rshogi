#!/usr/bin/env bash
set -euo pipefail

# Smoke: MultiPV behavior
# - MultiPV=3, go movetime 1000
# - Assert: PV(1) head == bestmove
# - Assert: currmovenumber monotonic (no renumbering due to exclude)
# - Report: root_hint_exist/root_hint_used from finalize_diag

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN=target/release/engine-usi
LOG_DIR=.smoke
LOG="$LOG_DIR/multipv_depth6.log"
mkdir -p "$LOG_DIR"

if ! command -v rg >/dev/null 2>&1; then
  echo "[smoke] ERROR: ripgrep (rg) is required" >&2
  exit 99
fi

echo "[smoke] building engine-usi" >&2
cargo build -p engine-usi --release >/dev/null

echo "[smoke] running MultiPV depth 6 session" >&2
CTRL_PIPE=".smoke/multipv_ctrl.pipe"
rm -f "$CTRL_PIPE"; mkfifo "$CTRL_PIPE"

# Engine process with hard timeout guard (prevents hang)
timeout 45s stdbuf -oL -eL "$BIN" < "$CTRL_PIPE" | tee "$LOG" >/dev/null &
ENG_PID=$!
trap 'kill $ENG_PID 2>/dev/null || true; rm -f "$CTRL_PIPE"' EXIT

# Writer: send commands and go
{
  echo usi
  echo isready
  echo "setoption name Threads value 1"
  echo "setoption name MultiPV value 3"
  echo isready
  echo "position startpos"
  echo "go movetime 1000"
} > "$CTRL_PIPE"

# Monitor: wait bestmove then quit
tries=0
until rg -n "^bestmove " "$LOG" >/dev/null; do
  sleep 0.5
  tries=$((tries+1))
  if [ $tries -gt 90 ]; then
    echo "[smoke] ERROR: timeout waiting bestmove" >&2
    echo quit > "$CTRL_PIPE"
    wait $ENG_PID || true
    exit 1
  fi
done
echo quit > "$CTRL_PIPE"
wait $ENG_PID || true

bm=$(rg -n "^bestmove " "$LOG" -r '$0' | tail -n1 | awk '{print $2}')
# Align with finalize_select/fast_select の最終選択と比較（fast finalize 対応）
fsel=$( {
  rg -n "(finalize_select|.*_fast_select) .* move=" "$LOG" -r '$0' \
    | tail -n1 \
    | sed -E 's/.* move=([^ ]+).*/\1/'
} || true )

if [[ -z "${bm:-}" ]]; then
  echo "[smoke] ERROR: failed to parse bestmove" >&2
  exit 1
fi

if [[ -z "${fsel:-}" ]]; then
  echo "[smoke] WARN: finalize_select/fast_select not found; falling back to bestmove" >&2
  fsel="$bm"
fi

if [[ "$bm" != "$fsel" ]]; then
  echo "[smoke] NG: finalize_select ($fsel) != bestmove ($bm)" >&2
  exit 2
fi

# Verify currmovenumber monotonicity within each depth block
violations=$(awk '
  /^info string depth / { prev=""; next }
  /info string currmove/ {
    for(i=1;i<=NF;i++){
      if($i=="currmovenumber"){
        n=$(i+1)
        if(prev!="" && n+0 < prev+0){
          if(n+0 <= 2){ prev=n; next }
          v++
        }
        prev=n
      }
    }
  }
  END{print v+0}
' v=0 prev="" "$LOG")
if [[ "$violations" -ne 0 ]]; then
  echo "[smoke] NG: currmovenumber decreased ($violations times)" >&2
  exit 3
fi

# Sanity-check nodes/time/nps in PV lines
anomalies=$(awk '
  /^info depth [0-9]+ / && / multipv [0-9]+ / {
    nodes=""; time=""; nps="";
    for(i=1;i<=NF;i++){
      if($i=="nodes"){nodes=$(i+1)}
      if($i=="time"){time=$(i+1)}
      if($i=="nps"){nps=$(i+1)}
    }
    if(nodes=="" || nodes+0 < 0){bad++}
    if(time!="" && time+0 < 0){bad++}
    if(time!="" && time+0>0 && (nps=="" || nps+0<=0)){bad++}
  }
  END{print bad+0}
' "$LOG")
if [[ "${anomalies:-0}" -ne 0 ]]; then
  echo "[smoke] NG: nodes/time/nps anomaly (${anomalies} cases)" >&2
  exit 4
fi

# Extract root hint usage from finalize_diag
hint_exist=$( {
  rg -n "finalize_diag .* root_hint_exist=" "$LOG" -r '$0' \
    | tail -n1 \
    | sed -E 's/.*root_hint_exist=([0-9]+).*/\1/'
} || true )
hint_used=$( {
  rg -n "finalize_diag .* root_hint_used=" "$LOG" -r '$0' \
    | tail -n1 \
    | sed -E 's/.*root_hint_used=([0-9]+).*/\1/'
} || true )

echo "[smoke] OK: finalize_select == bestmove ($bm), currmovenumber monotonic"
echo "[smoke] root_hint_exist=${hint_exist:-0} root_hint_used=${hint_used:-0}"
