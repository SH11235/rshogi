#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<USAGE >&2
Usage: $0 [--log taikyoku-log/xxx.md] [--out runs/regressions/...]
Options (env vars override defaults):
  --log PATH              USIログ (default: taikyoku-log/taikyoku_log_enhanced-parallel-202511101854.md)
  --out DIR               出力ルート (default: runs/regressions/dropguard-<timestamp>)
  --prefixes "PREFS"      replay対象prefix（空白区切り, default: "50 51 52 53 54"）
  --engine PATH           engine-usi バイナリ (default: target/release/engine-usi)
  --mp-threads N          replay時 Threads (default: 8)
  --mp-multipv N          replay時 MultiPV (default: 3)
  --mp-byoyomi MS         replay時 byoyomi (default: 10000)
  --eval-threads N        再評価スレッド数 (default: 8)
  --eval-byoyomi MS       再評価 byoyomi (default: 500)
  --eval-minthink MS      再評価 MinThinkMs (default: 50)
  --eval-warmup MS        再評価 Warmup.Ms (default: 0)
USAGE
}

LOG="taikyoku-log/taikyoku_log_enhanced-parallel-202511101854.md"
OUT_ROOT=""
PREFIXES="43 44 45 46 47 48 49 50 51 52 53 54"
ENGINE_BIN="target/release/engine-usi"
MP_THREADS=8
MP_MULTIPV=3
MP_BYO=10000
EVAL_THREADS=8
EVAL_BYO=500
EVAL_MINTHINK=50
EVAL_WARMUP=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --log)
      LOG="$2"; shift 2;;
    --out)
      OUT_ROOT="$2"; shift 2;;
    --prefixes)
      PREFIXES="$2"; shift 2;;
    --engine)
      ENGINE_BIN="$2"; shift 2;;
    --mp-threads)
      MP_THREADS="$2"; shift 2;;
    --mp-multipv)
      MP_MULTIPV="$2"; shift 2;;
    --mp-byoyomi)
      MP_BYO="$2"; shift 2;;
    --eval-threads)
      EVAL_THREADS="$2"; shift 2;;
    --eval-byoyomi)
      EVAL_BYO="$2"; shift 2;;
    --eval-minthink)
      EVAL_MINTHINK="$2"; shift 2;;
    --eval-warmup)
      EVAL_WARMUP="$2"; shift 2;;
    -h|--help)
      usage; exit 0;;
    *)
      echo "Unknown option: $1" >&2; usage; exit 1;;
  esac
done

if [[ -z "$OUT_ROOT" ]]; then
  OUT_ROOT="runs/regressions/dropguard-$(date +%Y%m%d-%H%M%S)"
fi

if [[ ! -f "$LOG" ]]; then
  echo "Log not found: $LOG" >&2; exit 1
fi

TARGETS_SRC="scripts/analysis/regression_targets/drop_guard_targets.json"
if [[ ! -f "$TARGETS_SRC" ]]; then
  echo "Target definition not found: $TARGETS_SRC" >&2; exit 1
fi

mkdir -p "$OUT_ROOT"
MP_DIR="$OUT_ROOT/multipv"
DIAG_DIR="$OUT_ROOT/diag"
mkdir -p "$MP_DIR" "$DIAG_DIR"

# 1) replay(10s)で pre-50..54 を MultiPV=3 で掘り直し
bash scripts/analysis/replay_multipv.sh "$LOG" -p "$PREFIXES" -o "$MP_DIR" -t "$MP_THREADS" -m "$MP_MULTIPV" -b "$MP_BYO" --profile match

# 2) 再評価ターゲット（静止落下セグメント）
cp "$TARGETS_SRC" "$DIAG_DIR/targets.json"
ENGINE_BIN="$ENGINE_BIN" python3 scripts/analysis/run_eval_targets.py "$DIAG_DIR" \
  --threads "$EVAL_THREADS" --byoyomi "$EVAL_BYO" --minthink "$EVAL_MINTHINK" --warmupms "$EVAL_WARMUP"

cat <<MSG
Done.
Multipv summary : $MP_DIR/summary.txt
Eval summary    : $DIAG_DIR/summary.json
MSG
