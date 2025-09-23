#!/bin/bash
# NNUE評価用ローカルスクリプト

set -euo pipefail

# デフォルト値
BASELINE="${1:-runs/nnue_local/baseline.nnue}"
CANDIDATE="${2:-runs/nnue_local/candidate.nnue}"
GAMES="${3:-1000}"
THREADS="${4:-8}"

echo "NNUE Performance Evaluation"
echo "=========================="
echo "Baseline:  $BASELINE"
echo "Candidate: $CANDIDATE"
echo "Games:     $GAMES"
echo "Threads:   $THREADS"
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
  --threads "$THREADS" \
  --hash-mb 1024 \
  --book docs/reports/fixtures/opening/representative_100.epd \
  --json runs/gauntlet_local/result.json \
  --report runs/gauntlet_local/report.md \
  --seed 12345 \
  --pv-ms 500

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