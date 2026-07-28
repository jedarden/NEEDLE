# bf-1sam: Implement p95 aggregation across benchmark iterations

## Task

Implement aggregation logic for p95 values across multiple benchmark iterations and ensure aggregation follows correct statistical approach.

## Implementation Status

**ALREADY COMPLETE** - The implementation already exists in `src/stats/mod.rs` and has been verified.

## Implementation Details

### P95Collector (lines 746-815)

The `P95Collector` struct provides statistically sound aggregation across multiple benchmark iterations:

```rust
pub struct P95Collector {
    samples: Vec<u128>,
}
```

Key methods:
- `record(latency_us: u128)` - Record a single latency sample
- `record_all(latencies: impl IntoIterator<Item = u128>)` - Record multiple samples
- `p95() -> u128` - Calculate p95 across all recorded samples
- `count() -> usize` - Return number of samples collected
- `clear()` - Clear all recorded samples
- `samples() -> &[u128]` - Get reference to underlying samples
- `stats() -> Option<(u128, u128, f64)>` - Calculate additional statistics

### Statistical Approach

The implementation follows the **statistically correct approach**:

1. **Pool all samples** from all iterations into a single dataset
2. **Calculate one p95** on the pooled data using linear interpolation

**DO NOT average p95 values** from individual iterations — this is statistically invalid because percentiles are non-linear statistics.

### Algorithm

Uses **linear interpolation** (same method as Criterion.rs):

```rust
rank = 0.95 * (n - 1)
floor_index = rank.floor()
fraction = rank - floor_index
interpolated = floor_value + (ceiling_value - floor_value) * fraction
```

## Verification

All tests pass (6/6):

```bash
$ cargo test --test p95_aggregation
running 6 tests
test test_p95_collector_clear_and_reuse ... ok
test test_p95_collector_aggregates_across_iterations ... ok
test test_p95_collector_preserves_statistical_validity ... ok
test test_p95_collector_with_outliers ... ok
test test_p95_collector_with_realistic_benchmark_pattern ... ok
test test_p95_collector_large_dataset ... ok

test result: ok. 6 passed; 0 failed; 0 ignored
```

### Test Coverage

1. **Aggregation correctness**: Verifies that P95Collector produces the same result as manually pooling all samples
2. **Statistical validity**: Confirms aggregation preserves statistical properties
3. **Outlier handling**: Tests proper handling of extreme outliers
4. **Reusability**: Verifies clear() and reuse functionality
5. **Scalability**: Tests with large datasets (10,000 samples)
6. **Realistic patterns**: Tests with realistic benchmark warm-up + measurement patterns

## Acceptance Criteria

✅ **p95 values are properly aggregated across iterations**
   - P95Collector pools all samples and calculates p95 on the aggregated dataset

✅ **Aggregation preserves statistical validity**
   - Uses linear interpolation (Criterion.rs methodology)
   - Does not average individual p95 values (statistically invalid)
   - Verified by `test_p95_collector_preserves_statistical_validity`

✅ **Code compiles without errors**
   - All tests pass
   - No compilation warnings

## Example Usage

```rust
use needle::stats::P95Collector;
use std::time::Instant;

let mut collector = P95Collector::new();

// Run benchmark for 50 iterations
for _ in 0..50 {
    let start = Instant::now();
    // ... perform work ...
    collector.record(start.elapsed().as_micros());
}

// Calculate p95 across all iterations
let p95_us = collector.p95();
println!("p95 latency: {} μs", p95_us);
```

## Related Beads

- **bf-90va**: Basic p95 percentile calculation implementation (CLOSED)
- **bf-5u57**: Verify p95 implementation compiles and runs (CLOSED)
- **bf-1sam**: p95 aggregation across iterations (THIS BEAD)

The P95Collector provides the aggregation layer on top of the basic `calculate_p95()` function implemented in bf-90va.
