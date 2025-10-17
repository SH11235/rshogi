#!/bin/bash
# NNUE評価用ローカルスクリプト
# moved: scripts/nnue/evaluate-nnue.sh

set -euo pipefail

# デフォルト値（Spec 013: threads=1 を強制）
BASELINE="${1:-runs/nnue_local/baseline.nnue}"
CANDIDATE="${2:-runs/nnue_local/candidate.nnue}"
GAMES="${3:-1000}"
REQ_THREADS="${4:-1}"
# 追加オプション: 第5引数で PV 計測時間（ms）を上書き
PV_MS="${5:-500}"

# Opening book の既定
# 優先: runs/fixed/20251011/openings_ply1_20_v1.sfen（固定スイート）
# 環境変数 EVAL_BOOK があればそれを使う
DEFAULT_BOOK="runs/fixed/20251011/openings_ply1_20_v1.sfen"
LEGACY_BOOK="docs/reports/fixtures/opening/representative_100.epd"
BOOK_PATH="${EVAL_BOOK:-$DEFAULT_BOOK}"
if [ ! -f "$BOOK_PATH" ]; then
  BOOK_PATH="$LEGACY_BOOK"
fi

echo "NNUE Performance Evaluation"
echo "=========================="
echo "Baseline:  $BASELINE"
echo "Candidate: $CANDIDATE"
echo "Games:     $GAMES"
echo "Threads:   $REQ_THREADS (Spec013 requires 1)"
echo "Book:      $BOOK_PATH"
echo "PV-ms:     $PV_MS"
echo ""

# ビルド
echo "Building gauntlet tool..."
cargo build -p tools --release --bin gauntlet --features nnue_telemetry

# 実行
echo "Running evaluation..."
mkdir -p runs/gauntlet_local

target/release/gauntlet \
  --base "$BASELINE" \
  --cand "$CANDIDATE" \
  --time "0/10+0.1" \
  --games "$GAMES" \
  --threads 1 \
  --hash-mb 1024 \
  --book "$BOOK_PATH" \
  --json runs/gauntlet_local/result.json \
  --report runs/gauntlet_local/report.md \
  --seed 12345 \
  --pv-ms "$PV_MS"

# 結果表示
echo ""
echo "Results:"
echo "--------"
cat runs/gauntlet_local/report.md

# Gate判定
GATE=$(jq -r '.summary.gate' runs/gauntlet_local/result.json)
echo ""
echo "Gate Decision: $GATE"

if [ "$GATE" = "pass" ]; then
  echo "✅ Candidate NNUE passed all criteria!"
elif [ "$GATE" = "provisional" ]; then
  echo "⚠️  Candidate NNUE is comparable but not clearly better"
else
  echo "❌ Candidate NNUE failed to meet criteria"
fi
