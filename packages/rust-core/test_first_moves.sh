#!/bin/bash
# 3手分の簡易対局テスト

set -e

ENGINE="./target/release/engine-usi"
LOG_FILE="test_first_moves_$(date +%Y%m%d_%H%M%S).log"

echo "=== USI対局テスト開始 ===" | tee -a "$LOG_FILE"
echo "ログファイル: $LOG_FILE" >&2

# エンジンを起動し、標準出力とエラー出力の両方をキャプチャ
{
    # USIプロトコルでのやりとり
    echo "usi"
    sleep 1

    echo "setoption name USI_Hash value 1024"
    echo "setoption name Threads value 10"
    echo "setoption name EngineType value EnhancedNnue"
    echo "setoption name StopWaitMs value 800"
    sleep 0.5

    echo "isready"
    sleep 2

    echo "=== 1手目: 先手（本エンジン） ===" >&2
    echo "position startpos"
    echo "go btime 0 wtime 0 byoyomi 10000"
    sleep 12

    echo "=== 2手目: 後手（固定で9c9d） ===" >&2
    echo "position startpos moves 8g8f 9c9d"
    sleep 0.5

    echo "=== 3手目: 先手（本エンジン） ===" >&2
    echo "go btime 0 wtime 0 byoyomi 10000"
    sleep 12

    echo "quit"
    sleep 1
} | "$ENGINE" 2>&1 | tee -a "$LOG_FILE"

echo ""
echo "=== テスト完了 ===" | tee -a "$LOG_FILE"
echo "ログファイル: $LOG_FILE"
echo ""
echo "=== 重要なログの抽出 ===" | tee -a "$LOG_FILE"
grep -E "(controller_|searcher_|bestmove|info depth)" "$LOG_FILE" || true