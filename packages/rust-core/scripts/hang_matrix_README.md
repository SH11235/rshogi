# MoveGen Hang Investigation Matrix Script

## Overview
`hang_matrix.sh` is a comprehensive test script designed to isolate and identify the conditions that cause MoveGen hangs in subprocess execution.

## TSV Output Format

The script outputs results in TSV (Tab-Separated Values) format with the following columns:

| Column | Description | Example Values |
|--------|-------------|----------------|
| timestamp | Unix timestamp when test was run | 1735500000 |
| test_name | Unique test configuration identifier | subprocess_skip0_any0_debug_dry0_flush1 |
| execution_mode | How the engine was executed | subprocess, direct |
| skip_legal_moves | SKIP_LEGAL_MOVES environment variable | 0, 1 |
| use_any_legal | USE_ANY_LEGAL environment variable | 0, 1 |
| build_type | Build configuration | debug, release |
| usi_dry_run | USI_DRY_RUN environment variable | 0, 1 |
| force_flush_stderr | FORCE_FLUSH_STDERR environment variable | 0, 1 |
| result | Test outcome | success, timeout, error |
| duration_ms | Test execution time in milliseconds | 1234 |
| hang_detected | Whether a hang was detected | true, false |
| exit_code | Process exit code | 0, 124, etc. |
| notes | Additional information about the test | Process killed by timeout |

## Usage

### Basic Usage
```bash
./scripts/hang_matrix.sh
```

### Output Location
Results are saved to:
- TSV file: `hang_matrix_results/matrix_results_<timestamp>.tsv`
- Python analyzer: `hang_matrix_results/analyze_matrix.py`

### Analysis
After running the matrix, analyze results with:
```bash
python hang_matrix_results/analyze_matrix.py hang_matrix_results/matrix_results_<timestamp>.tsv
```

## Environment Variables

The script tests various combinations of:
- `SKIP_LEGAL_MOVES`: Controls has_legal_moves check (0=enabled, 1=disabled)
- `USE_ANY_LEGAL`: Selects optimization method (0=generate_all, 1=early_exit)
- `USI_DRY_RUN`: Disables USI output (0=normal, 1=dry_run)
- `FORCE_FLUSH_STDERR`: Forces stderr flushing (0=normal, 1=force)

## Test Matrix

The script runs all valid combinations of:
- Execution modes: subprocess, direct
- Skip legal moves: 0, 1
- Use any legal: 0, 1 (only when skip=0)
- Build types: debug, release
- USI dry run: 0, 1
- Force flush stderr: 0, 1

Invalid combinations (e.g., skip=1 with any_legal=1) are automatically skipped.

## Interpreting Results

- **timeout**: Indicates a hang was detected (10s timeout)
- **success**: Test completed normally
- **error**: Test failed with non-zero exit code

Focus on configurations that result in timeouts to identify hang conditions.