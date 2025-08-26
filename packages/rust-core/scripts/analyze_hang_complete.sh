#!/bin/bash
# MoveGenハング完全分析スクリプト
# 分類 → 最小化 → 局所化を一気に実行

set -euo pipefail

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
REPORT_DIR="hang_analysis_${TIMESTAMP}"
mkdir -p "$REPORT_DIR"

echo "=== MoveGen Hang Complete Analysis ==="
echo "Report directory: $REPORT_DIR"
echo ""

# Phase 1: Classification
echo "=========================================="
echo "Phase 1: Classification (CPU/IO/Lock)"
echo "=========================================="

./scripts/collect_hang_evidence.sh > "$REPORT_DIR/phase1_classification.log" 2>&1

# Extract classification from the log
CLASSIFICATION=$(grep "Type:" "$REPORT_DIR/phase1_classification.log" | tail -1 | cut -d: -f2- | xargs || echo "UNKNOWN")
echo "Hang type detected: $CLASSIFICATION"
echo ""

# Phase 2: Minimization
echo "=========================================="
echo "Phase 2: USI Sequence Minimization"
echo "=========================================="

./scripts/minimize_hang.sh > "$REPORT_DIR/phase2_minimization.log" 2>&1

# Extract minimal sequence
MINIMAL_SEQ=$(grep -A1 "Minimal command sequence" "$REPORT_DIR/phase2_minimization.log" 2>/dev/null | tail -1 || echo "Not determined")
echo "Minimal sequence: $MINIMAL_SEQ"
echo ""

# Phase 3: Localization
echo "=========================================="
echo "Phase 3: MoveGen Phase Localization"
echo "=========================================="

./scripts/localize_hang.sh > "$REPORT_DIR/phase3_localization.log" 2>&1

# Extract problematic phases
echo "Phases that prevent hang when disabled:"
grep "✅ NO HANG when" "$REPORT_DIR/phase3_localization.log" 2>/dev/null || echo "None found"

# Extract last phase from trace
LAST_PHASE=$(grep "Last phase before hang:" "$REPORT_DIR/phase3_localization.log" 2>/dev/null | cut -d: -f2- | xargs || echo "Unknown")
echo "Last phase in trace: $LAST_PHASE"
echo ""

# Additional Analysis based on classification
echo "=========================================="
echo "Additional Analysis"
echo "=========================================="

case "$CLASSIFICATION" in
    *"CPU LOOP"*)
        echo "CPU Loop detected - checking for hot functions..."
        # Run perf if available
        if command -v perf &> /dev/null; then
            echo -e "position startpos\ngo depth 1" | \
                SKIP_LEGAL_MOVES=0 timeout 5 perf record -F 99 ./target/release/engine-cli 2>&1 || true
            perf report --stdio --no-header | head -20 > "$REPORT_DIR/perf_hotspots.txt"
            echo "Top hot functions saved to $REPORT_DIR/perf_hotspots.txt"
        fi
        ;;
        
    *"LOCK"*|*"MUTEX"*)
        echo "Lock/Mutex issue detected - checking for static initializers..."
        # Look for lazy_static or Once usage
        grep -r "lazy_static\|Once::new\|ONCE_INIT" crates/engine-core/src/movegen/ > "$REPORT_DIR/static_init_usage.txt" 2>/dev/null || true
        echo "Static initializer usage saved to $REPORT_DIR/static_init_usage.txt"
        ;;
        
    *"I/O BLOCKING"*)
        echo "I/O blocking detected - checking buffer sizes..."
        # Test with different buffer configurations
        for buf_size in 0 1 1024 65536; do
            echo -n "Testing with buffer size $buf_size: "
            if echo -e "position startpos\ngo depth 1" | \
                SKIP_LEGAL_MOVES=0 stdbuf -o${buf_size} -e${buf_size} \
                timeout 3 ./target/release/engine-cli >/dev/null 2>&1; then
                echo "✅ Works"
            else
                echo "❌ Hangs"
            fi
        done > "$REPORT_DIR/buffer_tests.txt"
        ;;
esac

# Generate final report
echo ""
echo "=========================================="
echo "Final Report"
echo "=========================================="

cat > "$REPORT_DIR/SUMMARY.md" << EOF
# MoveGen Hang Analysis Report
Generated: $(date)

## Classification
- **Hang Type**: $CLASSIFICATION
- **Evidence Directory**: hang_evidence_*

## Minimization
- **Minimal USI Sequence**: $MINIMAL_SEQ
- **Subprocess-specific**: $(grep -q "Works directly" "$REPORT_DIR/phase2_minimization.log" 2>/dev/null && echo "Yes" || echo "No")

## Localization
- **Last Phase Before Hang**: $LAST_PHASE
- **Phases That Prevent Hang**:
$(grep "✅ NO HANG when" "$REPORT_DIR/phase3_localization.log" 2>/dev/null | sed 's/^/  - /' || echo "  - None found")

## Recommendations

Based on the analysis:

EOF

# Add specific recommendations based on findings
if [[ "$LAST_PHASE" == *"checkers_pins"* ]]; then
    cat >> "$REPORT_DIR/SUMMARY.md" << EOF
1. **Focus on checkers_pins calculation**
   - Check for infinite loops in attack detection
   - Verify bitboard operations are correct
   - Look for uninitialized data access

EOF
fi

if grep -q "NO HANG when .* is disabled" "$REPORT_DIR/phase3_localization.log" 2>/dev/null; then
    cat >> "$REPORT_DIR/SUMMARY.md" << EOF
2. **Isolated to specific phase(s)**
   - Add detailed logging within the problematic phase
   - Check for phase-specific static initialization
   - Verify move generation logic for edge cases

EOF
fi

cat >> "$REPORT_DIR/SUMMARY.md" << EOF
## Next Steps

1. Review the detailed logs in $REPORT_DIR/
2. If CPU loop: Use debugger to break into the hanging process
3. If Lock issue: Check for re-entrant calls or initialization order
4. If I/O issue: Verify subprocess stderr handling

## File Locations
- Classification: $REPORT_DIR/phase1_classification.log
- Minimization: $REPORT_DIR/phase2_minimization.log
- Localization: $REPORT_DIR/phase3_localization.log
EOF

echo "Analysis complete!"
echo ""
echo "📊 Summary saved to: $REPORT_DIR/SUMMARY.md"
echo "📁 All logs saved to: $REPORT_DIR/"
echo ""
echo "Next steps:"
echo "1. Review $REPORT_DIR/SUMMARY.md for findings"
echo "2. Check trace output for the last executing phase"
echo "3. Focus debugging on the identified problematic area"