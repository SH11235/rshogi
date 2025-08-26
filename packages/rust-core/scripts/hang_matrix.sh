#!/bin/bash
# MoveGen hang investigation matrix runner
# This script runs a comprehensive set of tests to isolate the hang condition

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Test setup
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_BIN="$PROJECT_ROOT/target/release/engine-cli"
ENGINE_DEBUG_BIN="$PROJECT_ROOT/target/debug/engine-cli"
RESULTS_DIR="$PROJECT_ROOT/hang_matrix_results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULTS_FILE="$RESULTS_DIR/matrix_results_$TIMESTAMP.tsv"

# Ensure binaries exist
echo "Building engine binaries..."
cd "$PROJECT_ROOT"
cargo build --release --bin engine-cli
cargo build --bin engine-cli

# Create results directory
mkdir -p "$RESULTS_DIR"

# Initialize TSV header
echo -e "timestamp\ttest_name\texecution_mode\tskip_legal_moves\tuse_any_legal\tbuild_type\tusi_dry_run\tforce_flush_stderr\tresult\tduration_ms\thang_detected\texit_code\tnotes" > "$RESULTS_FILE"

# Test configuration matrix
declare -a EXECUTION_MODES=("subprocess" "direct")
declare -a SKIP_LEGAL_MOVES_VALUES=("0" "1")
declare -a USE_ANY_LEGAL_VALUES=("0" "1")
declare -a BUILD_TYPES=("debug" "release")
declare -a USI_DRY_RUN_VALUES=("0" "1")
declare -a FORCE_FLUSH_STDERR_VALUES=("0" "1")

# Test function
run_test() {
    local execution_mode=$1
    local skip_legal_moves=$2
    local use_any_legal=$3
    local build_type=$4
    local usi_dry_run=$5
    local force_flush_stderr=$6
    
    local test_name="${execution_mode}_skip${skip_legal_moves}_any${use_any_legal}_${build_type}_dry${usi_dry_run}_flush${force_flush_stderr}"
    local engine_path="$ENGINE_BIN"
    if [[ "$build_type" == "debug" ]]; then
        engine_path="$ENGINE_DEBUG_BIN"
    fi
    
    echo -ne "Testing $test_name... "
    
    local start_time=$(date +%s%3N)
    local result="success"
    local hang_detected="false"
    local exit_code=0
    local notes=""
    
    # Set up environment
    export SKIP_LEGAL_MOVES="$skip_legal_moves"
    export USE_ANY_LEGAL="$use_any_legal"
    export USI_DRY_RUN="$usi_dry_run"
    export FORCE_FLUSH_STDERR="$force_flush_stderr"
    export RUST_LOG="warn"  # Reduce noise
    
    # Create test input
    local test_input=$(mktemp)
    cat > "$test_input" << EOF
usi
isready
usinewgame
position startpos
go depth 1
quit
EOF
    
    # Run test based on execution mode
    if [[ "$execution_mode" == "subprocess" ]]; then
        # Run as subprocess with timeout
        timeout 10s "$engine_path" < "$test_input" > /dev/null 2>&1
        exit_code=$?
        
        if [[ $exit_code -eq 124 ]]; then
            result="timeout"
            hang_detected="true"
            notes="Process killed by timeout"
        elif [[ $exit_code -ne 0 ]]; then
            result="error"
            notes="Exit code: $exit_code"
        fi
    else
        # Direct execution (simulated - just check if binary runs)
        timeout 2s "$engine_path" --version > /dev/null 2>&1
        exit_code=$?
        
        if [[ $exit_code -eq 0 ]]; then
            result="success"
            notes="Direct execution test (version check only)"
        else
            result="error"
            notes="Failed to run version check"
        fi
    fi
    
    local end_time=$(date +%s%3N)
    local duration=$((end_time - start_time))
    
    # Clean up
    rm -f "$test_input"
    
    # Record result
    local timestamp=$(date +%s)
    echo -e "${timestamp}\t${test_name}\t${execution_mode}\t${skip_legal_moves}\t${use_any_legal}\t${build_type}\t${usi_dry_run}\t${force_flush_stderr}\t${result}\t${duration}\t${hang_detected}\t${exit_code}\t${notes}" >> "$RESULTS_FILE"
    
    # Display result
    if [[ "$result" == "success" ]]; then
        echo -e "${GREEN}OK${NC} (${duration}ms)"
    elif [[ "$result" == "timeout" ]]; then
        echo -e "${RED}HANG${NC} (timeout after 10s)"
    else
        echo -e "${YELLOW}ERROR${NC} (${notes})"
    fi
    
    # Unset environment
    unset SKIP_LEGAL_MOVES USE_ANY_LEGAL USI_DRY_RUN FORCE_FLUSH_STDERR RUST_LOG
}

# Main test loop
echo "Running MoveGen hang investigation matrix..."
echo "Results will be saved to: $RESULTS_FILE"
echo ""

total_tests=0
hang_count=0
error_count=0
success_count=0

for execution_mode in "${EXECUTION_MODES[@]}"; do
    for skip_legal_moves in "${SKIP_LEGAL_MOVES_VALUES[@]}"; do
        for use_any_legal in "${USE_ANY_LEGAL_VALUES[@]}"; do
            for build_type in "${BUILD_TYPES[@]}"; do
                for usi_dry_run in "${USI_DRY_RUN_VALUES[@]}"; do
                    for force_flush_stderr in "${FORCE_FLUSH_STDERR_VALUES[@]}"; do
                        # Skip invalid combinations
                        if [[ "$skip_legal_moves" == "1" && "$use_any_legal" == "1" ]]; then
                            continue  # USE_ANY_LEGAL only matters when SKIP_LEGAL_MOVES=0
                        fi
                        
                        run_test "$execution_mode" "$skip_legal_moves" "$use_any_legal" "$build_type" "$usi_dry_run" "$force_flush_stderr"
                        
                        # Count results
                        total_tests=$((total_tests + 1))
                        last_result=$(tail -n1 "$RESULTS_FILE" | cut -f9)
                        case "$last_result" in
                            success) success_count=$((success_count + 1)) ;;
                            timeout) hang_count=$((hang_count + 1)) ;;
                            error) error_count=$((error_count + 1)) ;;
                        esac
                    done
                done
            done
        done
    done
done

# Summary
echo ""
echo "=== Test Matrix Summary ==="
echo "Total tests: $total_tests"
echo -e "Successful: ${GREEN}$success_count${NC}"
echo -e "Hangs detected: ${RED}$hang_count${NC}"
echo -e "Errors: ${YELLOW}$error_count${NC}"
echo ""

# Analysis
echo "=== Analysis ==="
echo "Configurations that hang:"
awk -F'\t' 'NR>1 && ($9=="timeout" || $11=="true") { print }' "$RESULTS_FILE" 2>/dev/null \
  | cut -f2-8 | column -t || echo "No hangs detected"

echo ""
echo "Results saved to: $RESULTS_FILE"

# Additional analysis script
cat > "$RESULTS_DIR/analyze_matrix.py" << 'EOF'
#!/usr/bin/env python3
import sys
import pandas as pd

if len(sys.argv) != 2:
    print("Usage: python analyze_matrix.py <results.tsv>")
    sys.exit(1)

df = pd.read_csv(sys.argv[1], sep='\t')

# Group by configuration and show hang rate
config_cols = ['execution_mode', 'skip_legal_moves', 'use_any_legal', 'build_type']
hang_analysis = df[df['hang_detected'] == 'true'].groupby(config_cols).size()

print("\nConfigurations with hangs:")
print(hang_analysis)

# Show average duration by configuration
print("\nAverage duration by configuration (successful runs only):")
success_df = df[df['result'] == 'success']
if not success_df.empty:
    duration_analysis = success_df.groupby(config_cols)['duration_ms'].mean()
    print(duration_analysis.sort_values(ascending=False))
EOF

chmod +x "$RESULTS_DIR/analyze_matrix.py"

echo ""
echo "To analyze results in detail, run:"
echo "  python $RESULTS_DIR/analyze_matrix.py $RESULTS_FILE"