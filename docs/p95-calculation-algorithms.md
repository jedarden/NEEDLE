# P95 Calculation Algorithms: Survey & Recommendations

## Overview

This document surveys p95 (95th percentile) calculation algorithms in the NEEDLE codebase and provides recommendations for implementation choices.

## Existing Implementations

### 1. Primary Implementation: Linear Interpolation

**Location:** `src/stats/mod.rs::calculate_p95` (lines 290-426)

**Algorithm:**
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
    
    // Linear interpolation: rank = (p / 100) * (n - 1)
    let rank = 0.95 * (n - 1) as f64;
    let floor_index = rank.floor() as usize;
    let fraction = rank - floor_index as f64;
    
    let floor_value = sorted[floor_index];
    let ceiling_value = sorted[floor_index + 1];
    
    // Linear interpolation: floor + (ceiling - floor) * fraction
    let interpolated = floor_value as f64 + (ceiling_value - floor_value) as f64 * fraction;
    
    // Round with epsilon for floating-point precision
    let epsilon = 1e-9;
    (interpolated + epsilon).round() as u128
}
```

**Formula:** `rank = 0.95 * (n - 1)`, then linearly interpolate between `sorted[floor(rank)]` and `sorted[ceil(rank)]`

**Example:** For `[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]`:
- `rank = 0.95 * 9 = 8.55`
- `floor_index = 8`, `fraction = 0.55`
- `interpolated = 90 + (100 - 90) * 0.55 = 95.5 → 96`

### 2. Alternative Method: Nearest-Rank (Simple Indexing)

**Location:** `src/sanitize/mod.rs` (line 797)

**Algorithm:**
```rust
latencies.sort();
let p95 = latencies[(SAMPLE_COUNT * 95) / 100];
```

**Formula:** Direct indexing at `⌈0.95 * n⌉ - 1`

**Example:** For same data `[10, 20, ..., 100]` (n=10):
- `index = (10 * 95) / 100 = 9` 
- `p95 = latencies[9] = 100`

**Note:** This is less accurate but simpler and faster.

### 3. Criterion.rs Integration

**Location:** `benches/sanitize.rs` and `Cargo.toml`

Criterion.rs (version 0.5) is already a dependency and **automatically calculates percentiles** using bootstrap analysis and linear interpolation internally.

**Configuration:** `criterion.toml` and custom configuration in `benches/sanitize.rs`:
```toml
[benchmarks]
# Criterion automatically calculates p95 and other percentiles
```

## Algorithm Comparison

| Aspect | Linear Interpolation | Nearest-Rank | Criterion.rs |
|--------|---------------------|--------------|--------------|
| Accuracy | High | Low-Medium | High |
| Smoothness | Smooth estimates | Discontinuous | Smooth estimates |
| Standard | Statistical literature | Simple heuristic | Industry standard for benchmarks |
| Complexity | O(n log n) sort + O(1) calc | O(n log n) sort + O(1) calc | Handled by library |
| Use Case | Production metrics | Quick estimates | Benchmarking |

## Edge Cases (All Handled)

### Empty Slice
- **Linear Interpolation:** Returns `0`
- **Nearest-Rank:** Would panic (must check first)

### Single Element
- **Linear Interpolation:** Returns that element
- **Nearest-Rank:** Returns that element (index 0)

### Two Elements
- **Linear Interpolation:** Interpolates between them
  - `[10, 20]`: rank = 0.95 * 1 = 0.95, result = 10 + 10 * 0.95 = 19.5 → 20
- **Nearest-Rank:** Returns maximum (index 1)

### Small Samples (< 20)
- **Linear Interpolation:** Still provides smooth estimates
- **Nearest-Rank:** Can be skewed toward maximum

### Unsorted Input
- **Linear Interpolation:** Sorts internally
- **Nearest-Rank:** Must sort first

### Large Samples (1000+)
- Both methods scale with sort complexity: O(n log n)

## Recommendation: Use Linear Interpolation

### For Production Metrics

**Use the existing `calculate_p95` function** in `src/stats/mod.rs`.

**Reasons:**
1. **Accurate:** Uses linear interpolation matching statistical standards
2. **Compatible:** Matches Criterion.rs approach for consistency
3. **Well-tested:** Comprehensive test coverage in `tests/p95_correctness.rs`
4. **Documented:** Extensive docstring with examples
5. **Edge-case safe:** Handles empty, single-element, unsorted inputs

### For Benchmarking

**Let Criterion.rs handle percentiles automatically.**

**Reasons:**
1. **Automatic:** Built-in percentile calculation
2. **Bootstrap analysis:** Provides confidence intervals
3. **Standard:** Industry-standard for Rust benchmarks
4. **No manual work:** Just configure and run

### For Quick Estimates (Tests/Debug)

**Use nearest-rank indexing when appropriate.**

**Reasons:**
1. **Simple:** One-liner calculation
2. **Fast:** No interpolation math
3. **Good enough:** For rough estimates in tests

**When to avoid:** Small samples or when accuracy matters.

## Criterion.rs vs Manual Implementation

### Decision: **Keep Both, Use Each for Its Purpose**

**Use Manual (`calculate_p95`) when:**
- Calculating percentiles in production code
- Need full control over the algorithm
- Working with arbitrary data outside benchmarks
- Building statistics for dashboards/alerts

**Use Criterion.rs when:**
- Writing benchmark harnesses
- Need confidence intervals
- Comparing benchmark runs
- Generating benchmark reports

### Don't Switch to Criterion.rs for Production Because:

1. **Criterion.rs is for benchmarking**, not general statistics
2. **Bootstrap analysis** is overkill for simple p95 calculation
3. **Tight coupling:** Your production code would depend on a benchmarking library
4. **Performance:** Bootstrap resampling (100,000 samples by default) is expensive
5. **Complexity:** Criterion.rs API is designed for benchmarks, not inline calculations

## Algorithm Choice: Linear Interpolation

### Why Linear Interpolation?

1. **Statistical Standard:** Used in most statistical software (R, Python scipy, etc.)
2. **Smooth:** Produces smooth percentile estimates across samples
3. **Accurate:** Better approximation of true percentile than nearest-rank
4. **Consistent:** Matches Criterion.rs for benchmark comparison consistency

### Why Not Nearest-Rank?

1. **Discontinuous:** Can jump abruptly with small data changes
2. **Biased:** Skewed toward maximum values for small samples
3. **Less accurate:** Rougher approximation of true percentile
4. **Non-standard:** Not used in statistical literature

## Implementation Status

✅ **Already Implemented:** `calculate_p95` using linear interpolation  
✅ **Comprehensive Tests:** `tests/p95_correctness.rs` with 8 test functions  
✅ **Documented:** Extensive docstring with algorithm description and examples  
✅ **Edge Cases:** Empty, single-element, unsorted, duplicates, outliers all handled  
✅ **Integrated:** Used in stats aggregation and performance tests  

**No changes needed** — the implementation is correct and well-documented.

## References

- **[Criterion.rs Analysis Documentation](https://bheisler.github.io/criterion.rs/book/analysis.html)** — Data analysis and percentile methods
- **[Criterion.rs Source](https://docs.rs/criterion/latest/src/criterion/lib.rs.html)** — Library source code
- **NEEDLE Implementation:** `src/stats/mod.rs::calculate_p95` (lines 290-426)
- **Test Suite:** `tests/p95_correctness.rs` — Comprehensive correctness tests

## Summary

The NEEDLE codebase uses **linear interpolation** for p95 calculation, which is the correct and recommended approach. The implementation is well-tested, documented, and handles all edge cases appropriately. Criterion.rs provides automatic percentile calculation for benchmarks, but for production statistics, the manual `calculate_p95` function is the right choice.
