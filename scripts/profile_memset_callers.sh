#!/bin/bash
# profile_memset_callers.sh - memsetの呼び出し元を特定するプロファイル
#
# 使い方:
#   sudo ./scripts/profile_memset_callers.sh
#
# 出力:
#   memset_callers.txt - memsetを呼び出している関数のコールスタック

set -e

cd "$(dirname "$0")/.."

NNUE_FILE="${NNUE_FILE:-suisho5.bin}"
DEPTH="${DEPTH:-15}"
OUTPUT_FILE="memset_callers.txt"
PERF_DATA="perf_memset.data"

echo "=============================================="
echo "  memset呼び出し元プロファイル"
echo "=============================================="
echo ""
echo "設定:"
echo "  NNUE: $NNUE_FILE"
echo "  深さ: $DEPTH"
echo ""

# ビルド
echo "ビルド中..."
RUSTFLAGS="-C target-cpu=native" cargo build --release -p rshogi-usi 2>/dev/null

# プロファイル取得（コールグラフ付き）
echo "プロファイル取得中 (depth $DEPTH)..."
sudo perf record -g --call-graph dwarf -o "$PERF_DATA" \
  ./target/release/rshogi-usi --eval "$NNUE_FILE" <<EOF
isready
position startpos
go depth $DEPTH
quit
EOF

echo ""
echo "memset呼び出し元を抽出中..."

# memsetの呼び出し元を抽出
sudo perf report -i "$PERF_DATA" --stdio -g caller \
  --symbol-filter=__memset_avx2_unaligned_erms \
  --percent-limit 0.1 > "$OUTPUT_FILE"

# clear_page_ermsも追加
echo "" >> "$OUTPUT_FILE"
echo "========================================" >> "$OUTPUT_FILE"
echo "clear_page_erms (kernel page zeroing)" >> "$OUTPUT_FILE"
echo "========================================" >> "$OUTPUT_FILE"
sudo perf report -i "$PERF_DATA" --stdio -g caller \
  --symbol-filter=clear_page_erms \
  --percent-limit 0.1 >> "$OUTPUT_FILE"

# クリーンアップ
sudo rm -f "$PERF_DATA"

echo ""
echo "完了: $OUTPUT_FILE"
echo ""
echo "上位の呼び出し元:"
grep -A 5 "Children" "$OUTPUT_FILE" | head -30
