#!/bin/bash
# MoveGenハング局所化スクリプト
# Phase 3: MoveGen内の特定フェーズを二分探索で特定

set -u

echo "=== MoveGen Hang Localization ==="
echo "Binary search to identify the hanging phase..."
echo ""

# まずトレース機能をビルド
echo "Building with trace support..."
cargo build --release

# テスト用の関数
test_with_disabled() {
    local phase="$1"
    local desc="$2"
    
    echo -n "Testing with $desc disabled: "
    if echo -e "position startpos\ngo depth 1" | \
        env SKIP_LEGAL_MOVES=0 MOVEGEN_DISABLE_${phase}=1 \
        timeout 3 ./target/release/engine-cli >/dev/null 2>&1; then
        echo "✅ NO HANG when $desc is disabled!"
        return 0
    else
        echo "❌ Still hangs"
        return 1
    fi
}

# 全フェーズのテスト
echo "=== Phase Disabling Tests ==="
PHASES=(
    "CHECKERS_PINS:Checkers and pins calculation"
    "KING:King moves"
    "ROOK:Rook moves"
    "BISHOP:Bishop moves"
    "GOLD:Gold moves"
    "SILVER:Silver moves"
    "KNIGHT:Knight moves"
    "LANCE:Lance moves"
    "PAWN:Pawn moves"
    "DROPS:Drop moves"
)

WORKING_PHASES=()

for phase_desc in "${PHASES[@]}"; do
    IFS=':' read -r phase desc <<< "$phase_desc"
    if test_with_disabled "$phase" "$desc"; then
        WORKING_PHASES+=("$phase:$desc")
    fi
done

echo ""
echo "=== Combination Tests ==="
# 効果があったフェーズの組み合わせテスト
if [ ${#WORKING_PHASES[@]} -gt 1 ]; then
    echo "Testing combinations of working phases..."
    for ((i=0; i<${#WORKING_PHASES[@]}; i++)); do
        for ((j=i+1; j<${#WORKING_PHASES[@]}; j++)); do
            IFS=':' read -r phase1 desc1 <<< "${WORKING_PHASES[$i]}"
            IFS=':' read -r phase2 desc2 <<< "${WORKING_PHASES[$j]}"
            
            echo -n "Disabling both $desc1 and $desc2: "
            if echo -e "position startpos\ngo depth 1" | \
                env SKIP_LEGAL_MOVES=0 MOVEGEN_DISABLE_${phase1}=1 MOVEGEN_DISABLE_${phase2}=1 \
                timeout 3 ./target/release/engine-cli >/dev/null 2>&1; then
                echo "✅ NO HANG"
            else
                echo "❌ Still hangs"
            fi
        done
    done
fi

echo ""
echo "=== Trace Output (Last Phase Before Hang) ==="
# トレースを有効にして最後のフェーズを特定
echo "Running with full trace (5 second timeout)..."
echo -e "position startpos\ngo depth 1" | \
    env SKIP_LEGAL_MOVES=0 MOVEGEN_TRACE=pre,checkers_pins,king,pieces,rook,bishop,gold,silver,knight,lance,pawn,drops,post \
    timeout 5 ./target/release/engine-cli 2>&1 | \
    grep "phase=" > trace_output.txt || true

if [ -s trace_output.txt ]; then
    echo "Trace output (last 10 phases):"
    tail -10 trace_output.txt
    
    echo ""
    echo "Phase counts:"
    cut -d= -f2 trace_output.txt | cut -f2 | sort | uniq -c | sort -nr
    
    LAST_PHASE=$(tail -1 trace_output.txt | cut -d= -f2 | cut -f2)
    echo ""
    echo "⚠️  Last phase before hang: $LAST_PHASE"
else
    echo "No trace output captured"
fi

echo ""
echo "=== Localization Summary ==="
if [ ${#WORKING_PHASES[@]} -gt 0 ]; then
    echo "Phases that prevent hang when disabled:"
    for phase_desc in "${WORKING_PHASES[@]}"; do
        IFS=':' read -r phase desc <<< "$phase_desc"
        echo "  - $desc (MOVEGEN_DISABLE_$phase=1)"
    done
    echo ""
    echo "Next steps:"
    echo "1. Focus debugging on the phase(s) that prevent hang when disabled"
    echo "2. Add more detailed tracing within those specific phases"
    echo "3. Check for:"
    echo "   - Infinite loops in the phase implementation"
    echo "   - Lock acquisition issues"
    echo "   - Uninitialized data access"
else
    echo "No single phase prevents the hang when disabled."
    echo "The issue may be in:"
    echo "  - Common initialization code (checkers/pins)"
    echo "  - Interaction between multiple phases"
    echo "  - Code outside the main phase loop"
fi