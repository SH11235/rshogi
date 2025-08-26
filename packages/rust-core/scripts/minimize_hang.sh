#!/bin/bash
# MoveGenハング最小化スクリプト
# Phase 2: 最小再現USIシーケンスを特定

set -u

echo "=== MoveGen Hang Minimization ==="
echo "Finding minimal USI sequence that triggers hang..."
echo ""

# テスト用の関数
test_sequence() {
    local seq="$1"
    local desc="$2"
    
    echo -n "Testing $desc: "
    if echo -e "$seq" | SKIP_LEGAL_MOVES=0 timeout 3 ./target/release/engine-cli >/dev/null 2>&1; then
        echo "✅ NO HANG"
        return 0
    else
        local code=$?
        if [ $code -eq 124 ]; then
            echo "❌ HANG DETECTED (timeout)"
            return 1
        else
            echo "⚠️  Failed with code $code"
            return 2
        fi
    fi
}

# 1. 基本シーケンスのテスト
echo "=== Standard USI Sequences ==="
test_sequence "usi\nisready\nusinewgame\nposition startpos\ngo depth 1\nquit" "Full sequence (6 commands)"
test_sequence "usi\nisready\nposition startpos\ngo depth 1\nquit" "No usinewgame (5 commands)"
test_sequence "usi\nposition startpos\ngo depth 1\nquit" "No isready (4 commands)"
test_sequence "position startpos\ngo depth 1\nquit" "No usi (3 commands)"
test_sequence "position startpos\ngo depth 1" "No quit (2 commands)"
test_sequence "go depth 1" "Only go (1 command)"

echo ""
echo "=== Position Variations ==="
test_sequence "position startpos\ngo depth 1" "startpos"
test_sequence "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\ngo depth 1" "Initial SFEN"
test_sequence "position startpos moves 7g7f\ngo depth 1" "After one move"

echo ""
echo "=== Go Command Variations ==="
test_sequence "position startpos\ngo movetime 100" "go movetime"
test_sequence "position startpos\ngo nodes 1000" "go nodes"
test_sequence "position startpos\ngo infinite" "go infinite (with timeout)"

echo ""
echo "=== Buffer Control Tests ==="
# stdbufでバッファリング制御
echo -n "With stdbuf -o0 -e0: "
if echo -e "position startpos\ngo depth 1" | SKIP_LEGAL_MOVES=0 stdbuf -o0 -e0 timeout 3 ./target/release/engine-cli >/dev/null 2>&1; then
    echo "✅ NO HANG (buffering may be the issue)"
else
    echo "❌ STILL HANGS (not a buffering issue)"
fi

echo ""
echo "=== USE_ANY_LEGAL Comparison ==="
echo -n "USE_ANY_LEGAL=0: "
echo -e "position startpos\ngo depth 1" | SKIP_LEGAL_MOVES=0 USE_ANY_LEGAL=0 timeout 3 ./target/release/engine-cli >/dev/null 2>&1 && echo "✅ NO HANG" || echo "❌ HANG"

echo -n "USE_ANY_LEGAL=1: "
echo -e "position startpos\ngo depth 1" | SKIP_LEGAL_MOVES=0 USE_ANY_LEGAL=1 timeout 3 ./target/release/engine-cli >/dev/null 2>&1 && echo "✅ NO HANG" || echo "❌ HANG"

echo ""
echo "=== Direct vs Subprocess ==="
# 直接実行のテスト
echo -n "Direct execution test: "
(
    export SKIP_LEGAL_MOVES=0
    echo -e "position startpos\ngo depth 1\nquit" | timeout 3 ./target/release/engine-cli 2>&1 | grep -q "bestmove"
) && echo "✅ Works directly" || echo "❌ Also hangs directly"

echo ""
echo "=== Summary ==="
echo "Based on the tests above, identify:"
echo "1. Minimal command sequence that triggers hang"
echo "2. Whether buffering affects the hang"
echo "3. Whether USE_ANY_LEGAL makes a difference"
echo "4. Whether it's subprocess-specific"