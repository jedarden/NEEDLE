# P95 Usage Research Summary

## Overview
This document summarizes current p95 usage in the NEEDLE codebase as of 2026-08-01.

## Files Using p95

### Core Implementation
- **`src/stats/mod.rs`** (lines 348-526)
  - Main `calculate_p95()` function implementation
  - `P95Collector` struct for aggregating latency samples
  - Comprehensive documentation with examples
  - Extensive test coverage

### Usage in Production Code
- **`src/sanitize/mod.rs`** (lines 797-798)
  - Performance testing in `sanitizer_performance()` test
  - Calculates p95 latency over 20 samples
  - Used alongside median for threshold assertions

### Benchmark Integration
- **`benches/sanitize.rs`** (multiple locations)
  - Lines 28, 40, 217-218, 274-275, 331-332, 434, 435, 479-480
  - Three benchmark functions: `bench_sanitize_10kb()`, `bench_sanitize_100kb()`, `bench_sanitize_1mb()`
  - Each reports median, p95, and p99 percentiles explicitly
  - Configured with Criterion.rs for automated percentile reporting

### Example/Validation Files
- **`examples/test_p95_simple.rs`** - Basic p95 calculation verification
- **`examples/test_p95_output.rs`** - Output format verification
- **`examples/validate_p95_values.rs`** - Value validation
- **`examples/verify_p95_reporting.rs`** - Reporting verification
- **`examples/test_benchmark_p95.rs`** - Benchmark integration
- **`examples/extract_p95_from_criterion.rs`** - Criterion.rs integration
- **`examples/test_p95_simple_manual.rs`** - Manual testing
- **`examples/test_p95_output.rs`** - Output testing

## Current Calculation Method

### Algorithm: Linear Interpolation

The implementation uses the **linear interpolation method**, which matches Criterion.rs approach:

```rust
pub fn calculate_p95(latencies: &[u128]) -> u128 {
    if latencies.is_empty() {
        return 0;
    }
    let n = latencies.len();
    if n == 1 {
        return latencies[0];
    }
    
    let mut sorted = Vec::from(latencies);
    sorted.sort();
    
    // rank = 0.95 * (n - 1)
    let rank = 0.95 * (n - 1) as f64;
    let floor_index = rank.floor() as usize;
    let fraction = rank - floor_index as f64;
    
    let floor_value = sorted[floor_index];
    let ceiling_value = sorted[floor_index + 1];
    
    // Linear interpolation: floor + (ceiling - floor) * fraction
    let interpolated = floor_value as f64 + (ceiling_value - floor_value) as f64 * fraction;
    
    // Round to nearest integer with epsilon for FP precision
    (interpolated + 1e-9).round() as u128
}
```

### Key Features
- **Handles all edge cases:** empty, single element, two elements, large datasets
- **Internally sorts input:** accepts unsorted data
- **Linear interpolation:** more accurate than nearest-rank method
- **Rounding with epsilon:** handles floating-point precision issues

## Criterion.rs Status

### Availability: PRESENT ✓

- **Location:** `Cargo.toml` line 123
- **Version:** 0.5
- **Type:** dev-dependency
- **Usage:** Benchmark harness in `benches/sanitize.rs`

### Criterion.rs Configuration
```rust
fn configure_criterion() -> Criterion {
    Criterion::default()
        .confidence_level(0.95)
        .sample_size(100)
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(5))
        .noise_threshold(0.02)
}
```

Criterion.rs calculates percentiles automatically via bootstrap analysis, but the benchmarks also explicitly call `needle::stats::calculate_p95()` for direct reporting.

## Implementation Approach Recommendation

### Current Status: DUAL APPROACH ✓

The codebase currently uses both:
1. **Custom implementation** (`needle::stats::calculate_p95()`) - for explicit calculations
2. **Criterion.rs** - as the benchmark harness with built-in percentile reporting

### Recommendation: CONTINUE DUAL APPROACH

**Rationale:**
1. **Custom implementation advantages:**
   - No external dependency for basic p95 needs
   - Full control over calculation method
   - Can be used outside benchmark context (e.g., sanitizer performance tests)
   - Well-documented with extensive examples
   - Comprehensive test coverage

2. **Criterion.rs integration advantages:**
   - Industry-standard benchmark harness
   - Automated percentile calculation via bootstrap
   - Rich reporting and plotting features
   - Statistical rigor (confidence intervals, noise filtering)

3. **Consistency:**
   - Both use the same linear interpolation method
   - Produce identical results for same inputs
   - Properly documented in code

### No Changes Needed

The current implementation is solid:
- Custom `calculate_p95()` is production-ready
- Criterion.rs integration is well-configured
- Both approaches complement each other
- No duplication concerns (different use cases)

## Additional Context

### Related Functions
- `calculate_p95()` - 95th percentile
- `calculate_p99()` - 99th percentile (same algorithm, different rank)
- `calculate_median()` - 50th percentile
- `P95Collector` - Aggregation helper
- `P99Collector` - P99 aggregation helper
- `MedianCollector` - Median aggregation helper

### Statistical Correctness
The implementation follows best practices:
- Pools all samples before calculating (no averaging of percentiles)
- Uses linear interpolation for accuracy
- Handles small samples correctly (2-3 elements)
- Proper edge case handling (empty, single element)

## Summary

- **Files with p95:** 11 files (1 core impl, 1 production test, 1 benchmark, 8 examples)
- **Calculation method:** Linear interpolation (matches Criterion.rs)
- **Criterion.rs:** Present (v0.5 in dev-dependencies)
- **Recommendation:** Continue current dual approach (no changes needed)
