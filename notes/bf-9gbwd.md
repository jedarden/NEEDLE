# p95 Calculation Survey

## Overview

Survey of all p95 latency calculation implementations in the NEEDLE codebase.

## Files Containing p95 Calculations

| File | Location | Purpose |
|------|----------|---------|
| `src/stats/mod.rs` | Lines 290-394 | Main `calculate_p95` function with comprehensive documentation |
| `src/sanitize/mod.rs` | Lines 780-814 | Performance test with inline p95 calculation |
| `benches/sanitize.rs` | Lines 197-232, 251-286, 305-340, 408-436, 447-499 | Multiple benchmark functions with explicit p95 reporting |

## Current Algorithm

All implementations use the **nearest-rank method**:

```
1. Sort values in ascending order
2. Calculate index: index = (length × 95) / 100
3. Return value at that index
```

**Key characteristics:**
- No interpolation between values
- Returns actual element from input
- O(n log n) time complexity due to sorting
- O(1) additional space (sorts in-place)

## Implementation Details

### 1. Main Stats Module (`src/stats/mod.rs`)

**Function:** `calculate_p95(latencies: &[u128]) -> u128`

```rust
pub fn calculate_p95(latencies: &[u128]) -> u128 {
    if latencies.is_empty() {
        return 0;
    }

    let mut sorted = Vec::from(latencies);
    sorted.sort();
    let index = (sorted.len() * 95) / 100;
    sorted[index]
}
```

**Features:**
- Comprehensive documentation with examples
- Handles empty input (returns 0)
- Works with unsorted input (sorts internally)
- Well-tested with unit tests

### 2. Sanitizer Performance Test (`src/sanitize/mod.rs`)

**Context:** Performance assertion test for trace sanitization

```rust
latencies.sort();
let median = latencies[SAMPLE_COUNT / 2];
let p95 = latencies[(SAMPLE_COUNT * 95) / 100];
```

**Features:**
- Uses `as_millis()` for timing precision
- SAMPLE_COUNT = 20 iterations
- Threshold: 10ms (release), 2000ms (debug)
- Reports both median and p95 in assertion messages

### 3. Benchmark Suite (`benches/sanitize.rs`)

**Multiple benchmark functions** for different trace sizes:
- `bench_sanitize_10kb` - 10KB traces
- `bench_sanitize_100kb` - 100KB traces  
- `bench_sanitize_1mb` - 1MB traces
- `bench_median_latency` - Percentile reporting

**Common pattern:**
```rust
latencies.sort();
let p95_us = latencies[(latencies.len() * 95) / 100];
let p95_ms = p95_us as f64 / 1000.0;
```

**Features:**
- Uses `as_micros()` for higher precision
- ASSERTION_SAMPLE_COUNT = 50 iterations
- Explicit eprintln! output for p95 values
- Also calculates p99 in some benchmarks: `latencies[(latencies.len() * 99) / 100]`

## Inconsistencies Found

| Aspect | Main Stats Module | Sanitizer Tests | Benchmark Suite |
|--------|-------------------|-----------------|------------------|
| **Return type** | `u128` | Used in assertion only | `f64` (converted from microseconds) |
| **Timing precision** | N/A (function only) | `as_millis()` | `as_micros()` |
| **Sample size** | Variable (input slice) | Fixed 20 | Fixed 50 |
| **Documentation** | Comprehensive docs | Minimal comments | Moderate comments |

## Summary

- **Total locations:** 3 files
- **Algorithm consistency:** All use nearest-rank method with identical formula
- **Primary use case:** Latency performance monitoring for trace sanitization
- **Performance thresholds:** 10ms release, 500-2000ms debug
- **No interpolation:** All implementations return actual values, not interpolated estimates

## Recommendations

1. **Consolidate:** Consider using `calculate_p95` from `stats/mod.rs` in test/benchmark code instead of inline calculations
2. **Standardize precision:** Choose either `as_millis()` or `as_micros()` consistently across all benchmarks
3. **Add p99 helper:** Consider adding a `calculate_p99` function to `stats/mod.rs` for consistency
