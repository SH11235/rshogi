# SEE Integration Testing Framework

## Overview

The SEE integration testing framework provides automated benchmarking and testing for Static Exchange Evaluation optimizations. It measures performance impact and correctness across various aspects of the search engine.

## Components

### 1. Tactical Position Database (`tests/tactical_positions.yaml`)

A curated collection of test positions including:
- Complex middle game positions
- Long tactical sequences (LTAC)
- Bait and trap positions
- Pin exploitation scenarios
- X-ray attack positions
- Positions requiring quiet moves

Each position includes:
- SFEN notation
- Description
- Expected results (best move, moves to avoid, minimum search depth)

### 2. Integration Tests (`tests/test_search_integration.rs`)

Comprehensive tests for SEE effectiveness:
- **Quiescence Search Comparison**: Compares search with/without SEE
- **Move Ordering Consistency**: Verifies PV stability and cutoff rates
- **Tactical Position Benchmarks**: Tests against known positions
- **SEE Pruning Effectiveness**: Measures pruning rates in main search
- **Performance Regression Tests**: Ensures SEE stays within performance bounds

### 3. Benchmark Suite (`benches/see_integration_bench.rs`)

Performance benchmarks measuring:
- Basic SEE calculation time
- SEE with pin detection overhead
- Search performance impact
- Move ordering efficiency

## Usage

### Running Tests

```bash
# Run all integration tests
cargo test --test test_search_integration

# Run specific test
cargo test test_see_in_quiescence_search_comparison

# Run with output
cargo test --test test_search_integration -- --nocapture
```

### Running Benchmarks

```bash
# Run all benchmarks
cargo bench --bench see_integration_bench

# Run specific benchmark group
cargo bench --bench see_integration_bench -- see_basic

# Save baseline for comparison
cargo bench --bench see_integration_bench -- --save-baseline before_optimization

# Compare against baseline
cargo bench --bench see_integration_bench -- --baseline before_optimization
```

### Interpreting Results

#### Test Metrics
- **Node Count**: Total positions searched
- **Quiescence Nodes**: Positions in quiescence search
- **Beta Cutoffs**: Number of beta cutoffs achieved
- **First Move Cutoff Rate**: Percentage of cutoffs from first move (>30% is good)
- **SEE Prune Rate**: Percentage of moves pruned by SEE

#### Performance Targets
- Basic SEE: < 200ns
- SEE with pins: < 250ns
- Quiescence cutoff rate: > 65%
- First move cutoff rate: > 35%

## Adding New Test Positions

1. Add position to `tactical_positions.yaml`:
```yaml
- name: "Descriptive Name"
  sfen: "position in SFEN format"
  description: "What makes this position interesting"
  expected:
    best_move: "7g7f"  # Optional
    avoid_move: "8h2b" # Optional
    min_depth: 6
```

2. The position will automatically be included in benchmark tests

## Performance Tracking

The framework supports automated performance tracking:

1. **Baseline Establishment**:
   ```bash
   cargo bench --bench see_integration_bench -- --save-baseline main
   ```

2. **Optimization Implementation**:
   - Implement optimization
   - Run benchmarks to measure impact

3. **Comparison**:
   ```bash
   cargo bench --bench see_integration_bench -- --baseline main
   ```

4. **Report Generation**:
   - Criterion generates HTML reports in `target/criterion/`
   - Shows performance changes with statistical significance

## Best Practices

1. **Before Optimization**:
   - Establish baseline performance
   - Run all tests to ensure correctness
   - Document current metrics

2. **During Development**:
   - Run targeted benchmarks frequently
   - Use `--profile-time 30` for more accurate measurements
   - Check both performance and correctness

3. **After Optimization**:
   - Run full benchmark suite
   - Verify no regression in test results
   - Document performance improvements

## Continuous Integration

The framework is designed for CI integration:

```yaml
# Example GitHub Actions workflow
- name: Run SEE benchmarks
  run: |
    cargo bench --bench see_integration_bench -- --save-baseline ${{ github.sha }}
    
- name: Compare performance
  if: github.event_name == 'pull_request'
  run: |
    cargo bench --bench see_integration_bench -- --baseline main
```

## Troubleshooting

### Common Issues

1. **Inconsistent benchmark results**:
   - Increase measurement time: `--measurement-time 20`
   - Close other applications
   - Use release mode: `cargo bench --release`

2. **Test failures after optimization**:
   - Check for off-by-one errors in SEE calculation
   - Verify pin detection correctness
   - Compare with reference implementation

3. **Performance regression**:
   - Profile with `cargo flamegraph`
   - Check for unnecessary allocations
   - Verify algorithmic complexity hasn't increased

## Future Enhancements

1. **Automated regression detection**
2. **Integration with perf/flamegraph**
3. **Multi-threaded search benchmarks**
4. **Endgame position test suite**