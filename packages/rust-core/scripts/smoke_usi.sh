#!/usr/bin/env bash
set -euo pipefail

CARGO_BIN="cargo run --release -p engine-usi"
TIMEOUT=15

run_session() {
  local script="$1"
  echo "---- ${script} ----"
  eval "timeout ${TIMEOUT} ${CARGO_BIN}" <<'EOS' | tee /tmp/usi_smoke_output.txt
uci
EOS
}

# Session 1: Enhanced profile with Razor disabled
cat <<'EOUSI1' | timeout ${TIMEOUT} ${CARGO_BIN} > /tmp/usi_smoke_session1.txt
usi
isready
setoption name EngineType value Enhanced
setoption name SearchParams.EnableRazor value false
setoption name SearchParams.EnableIID value true
setoption name SearchParams.EnableProbCut value true
isready
position startpos
go depth 5
quit
EOUSI1

grep -q "pruning_note=IID" /tmp/usi_smoke_session1.txt || { echo "[smoke] expected IID pruning note"; exit 1; }

grep -q "pruning_note=ProbCut" /tmp/usi_smoke_session1.txt || echo "[smoke] ProbCut note not emitted (check profile)"

echo "[smoke] session1 OK"

# Session 2: Threads/Hash toggle + stop
cat <<'EOUSI2' | timeout ${TIMEOUT} ${CARGO_BIN} > /tmp/usi_smoke_session2.txt
usi
isready
setoption name EngineType value EnhancedNnue
setoption name Threads value 2
setoption name USI_Hash value 128
setoption name SearchParams.EnableProbCut value true
setoption name SearchParams.EnableIID value true
isready
position startpos
go movetime 1000
stop
quit
EOUSI2

grep -q "threads_note=ClassicBackend currently runs single-threaded" /tmp/usi_smoke_session2.txt || echo "[smoke] threads note missing"

grep -q "bestmove" /tmp/usi_smoke_session2.txt || { echo "[smoke] bestmove missing"; exit 1; }

if [[ $(grep -c "bestmove" /tmp/usi_smoke_session2.txt) -ne 1 ]]; then
  echo "[smoke] bestmove emitted multiple times"
  exit 1
fi

echo "[smoke] session2 OK"

echo "All USI smoke checks passed"
