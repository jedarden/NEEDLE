# Full Latency Profile: Median, P95, and P99

## Overview

NEEDLE provides a complete latency profile with three key metrics:

1. **Median (p50)**: The 50th percentile — half of all observations fall below this value
2. **P95**: The 95th percentile — 95% of observations fall below this value
3. **P99**: The 99th percentile — 99% of observations fall below this value

## Metrics Explained

### Median (p50)
- **What it measures**: The middle value of all latency measurements
- **Use case**: Understanding typical or average user experience
- **Calculation**: Sort all values, pick the middle one (or average of two middle values for even counts)

### P95 (95th Percentile)
- **What it measures**: The "tail" latency — 95% of requests complete within this time
- **Use case**: Understanding worst-case experience for most users
- **Calculation**: Linear interpolation between sorted values at rank 0.95*(n-1)

### P99 (99th Percentile)
- **What it measures**: Extreme tail latency — 99% of requests complete within this time
- **Use case**: Understanding outlier performance and system stability
- **Calculation**: Linear interpolation between sorted values at rank 0.99*(n-1)

## Implementation

### Core Functions

```rust
use needle::stats::{calculate_p95, calculate_p99};

// Helper function for median
fn calculate_median(latencies: &mut Vec<u128>) -> u128 {
    latencies.sort();
    let len = latencies.len();
    if len == 0 {
        return 0;
    }
    if len % 2 == 0 {
        (latencies[len / 2 - 1] + latencies[len / 2]) / 2
    } else {
        latencies[len / 2]
    }
}
```

### Example Output

When benchmarks run, they report all three metrics:

```
Median latency for 100KB trace: 2.45 ms (50 samples)
  Min: 1.80 ms
  Max: 5.20 ms
  P95: 3.80 ms
  P99: 4.90 ms
```

### Formatting and Display

All metrics are reported in milliseconds for readability:

```rust
let median_us = calculate_median(&mut latencies);
let p95_us = calculate_p95(&latencies);
let p99_us = calculate_p99(&latencies);

// Convert to milliseconds
let median_ms = median_us as f64 / 1000.0;
let p95_ms = p95_us as f64 / 1000.0;
let p99_ms = p99_us as f64 / 1000.0;

println!("Median: {:.2} ms", median_ms);
println!("P95: {:.2} ms", p95_ms);
println!("P99: {:.2} ms", p99_ms);
```

## Test Coverage

### Comprehensive Test Suite

The test file `tests/latency_profile_full.rs` provides comprehensive coverage:

1. **`test_full_latency_profile_known_values`**: Verifies all metrics with known test data
2. **`test_full_latency_profile_realistic_data`**: Tests with simulated latency data
3. **`test_full_latency_profile_edge_cases`**: Handles empty, single, and two-element cases
4. **`test_latency_output_format`**: Validates formatting and conversion
5. **`test_p95_p99_calculation`**: Tests percentile calculation correctness
6. **`test_latency_profile_benchmark_simulation`**: Simulates a real benchmark run
7. **`test_all_metrics_visible_and_labeled`**: Verifies all metrics are labeled clearly

### Running the Tests

```bash
# Run all latency profile tests
cargo test --test latency_profile_full

# Run specific test
cargo test --test latency_profile_full test_full_latency_profile_known_values
```

### Expected Test Output

```
running 7 tests
test test_all_metrics_visible_and_labeled ... ok
test test_full_latency_profile_edge_cases ... ok
test test_full_latency_profile_known_values ... ok
test test_full_latency_profile_realistic_data ... ok
test test_latency_output_format ... ok
test test_p95_p99_calculation ... ok
test test_latency_profile_benchmark_simulation ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Algorithm Details

### Linear Interpolation

Both p95 and p99 use linear interpolation for accuracy:

**Formula**: `rank = (p/100) * (n-1)`, then interpolate between `sorted[floor(rank)]` and `sorted[ceil(rank)]`

**Example for p95** with `[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]`:
- `rank = 0.95 * 9 = 8.55`
- `floor_index = 8`, `fraction = 0.55`
- `interpolated = 90 + (100 - 90) * 0.55 = 95.5 → 96`

**Example for p99** with same data:
- `rank = 0.99 * 9 = 8.91`
- `floor_index = 8`, `fraction = 0.91`
- `interpolated = 90 + (100 - 90) * 0.91 = 99.1 → 99`

### Edge Cases Handled

- **Empty data**: Returns 0 for all metrics
- **Single element**: Returns that element for all metrics
- **Two elements**: Median uses average, p95/p99 use interpolation
- **Unsorted input**: Functions sort internally

## Integration with Benchmarks

### Criterion.rs Integration

The benchmark suite (`benches/sanitize.rs`) integrates with Criterion.rs while also providing explicit metric output:

```rust
fn bench_median_latency(c: &mut Criterion) {
    // ... benchmark setup ...
    
    latencies.sort();
    let median_us = latencies[ASSERTION_SAMPLE_COUNT / 2];
    let median_ms = median_us as f64 / 1000.0;
    let p95_us = calculate_p95(&latencies);
    let p95_ms = p95_us as f64 / 1000.0;
    let p99_us = calculate_p99(&latencies);
    let p99_ms = p99_us as f64 / 1000.0;
    
    eprintln!("Median latency for 100KB trace: {:.2} ms ({} samples)", 
              median_ms, ASSERTION_SAMPLE_COUNT);
    eprintln!("  Min: {:.2} ms", *latencies.first().unwrap() as f64 / 1000.0);
    eprintln!("  Max: {:.2} ms", *latencies.last().unwrap() as f64 / 1000.0);
    eprintln!("  P95: {:.2} ms", p95_ms);
    eprintln!("  P99: {:.2} ms", p99_ms);
}
```

### P95Collector and P99Collector

For aggregating samples across multiple iterations:

```rust
use needle::stats::P95Collector;

let mut collector = P95Collector::new();
for _ in 0..50 {
    let start = Instant::now();
    // ... perform work ...
    collector.record(start.elapsed().as_micros());
}

let p95_us = collector.p95();
println!("P95 latency: {} μs", p95_us);
```

## Verification Summary

✅ **All three metrics implemented**: median, p95, and p99  
✅ **Comprehensive tests**: 7 test functions covering all scenarios  
✅ **Clear labeling**: All metrics are clearly labeled in output  
✅ **No regressions**: Existing tests continue to pass  
✅ **Documentation**: Complete documentation with examples  
✅ **Benchmark integration**: All metrics visible in benchmark output  

## References

- **Implementation**: `src/stats/mod.rs` — calculate_p95, calculate_p99
- **Tests**: `tests/latency_profile_full.rs` — Comprehensive test suite
- **Benchmarks**: `benches/sanitize.rs` — Benchmark suite with metric output
- **Algorithm docs**: `docs/p95-calculation-algorithms.md` — Algorithm details
