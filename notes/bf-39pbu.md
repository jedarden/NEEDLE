# P95 Calculation Implementations in NEEDLE

## Research Date: 2026-07-14

## Overview

NEEDLE uses multiple approaches for p95/percentile calculation depending on context:
1. Custom implementation in `src/stats/mod.rs`
2. Criterion.rs for benchmarking
3. Manual inline calculations for specific use cases

---

## 1. Custom Implementation: `calculate_p95()`

**Location:** `src/stats/mod.rs` (lines 290-394)

**Function Signature:**
```rust
pub fn calculate_p95(latencies: &[u128]) -> u128
```

**Algorithm:**
- Uses **nearest-rank method** (no interpolation)
- Formula: `index = (n * 95) / 100` where n = number of elements
- Returns actual value from sorted data (not interpolated)
- O(n log n) time complexity due to sorting
- Returns 0 for empty input

**Usage in Codebase:**
- Documented with comprehensive examples
- Unit tested with 5 test cases
- Public API for general use

**Example Usage:**
```rust
use needle::stats::calculate_p95;

let latencies = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
let p95 = calculate_p95(&latencies);
assert_eq!(p95, 100); // index = (10 * 95) / 100 = 9
```

---

## 2. Criterion.rs Integration

**Location:** `Cargo.toml`, `criterion.toml`, `benches/sanitize.rs`

**Dependency Status:**
```toml
[dev-dependencies]
criterion = "0.5"
```

**Configuration:** `criterion.toml`
- Sample size: 10 (default), overridden to 100 in benchmarks
- Warm-up time: 3 seconds
- Measurement time: 5 seconds
- Output format: verbose
- Plotting backend: auto

**Usage in Benchmarks:**
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

**How Criterion Calculates Percentiles:**
- Uses **bootstrap analysis** for percentile estimation
- Automatically calculates p95, p99, and other percentiles
- More samples → more accurate percentile estimation
- Reports percentiles in verbose output and HTML reports

---

## 3. Manual Inline Calculations

**Locations:**
- `src/sanitize/mod.rs` (line 797) - performance assertion test
- `benches/sanitize.rs` (lines 216, 270, 324, 426, 470) - benchmark functions

**Pattern Used:**
```rust
latencies.sort();
let p95 = latencies[(latencies.len() * 95) / 100];
```

**Purpose:**
- Explicit control over calculation in assertion tests
- Immediate output without Criterion's overhead
- Custom formatting for benchmark reporting

---

## 4. Current Approaches Comparison

| Approach | Use Case | Pros | Cons |
|----------|----------|------|------|
| **`calculate_p95()`** | General code, tests | • Public API<br>• Simple, direct<br>• Well-documented<br>• No external deps | • O(n log n) sort<br>• No interpolation |
| **Criterion.rs** | Benchmark suites | • Bootstrap analysis<br>• Confidence intervals<br>• HTML reports<br>• Statistical rigor | • Dev-only dependency<br>• Benchmark harness overhead |
| **Manual inline** | Specific assertions | • Full control<br>• No overhead<br>• Custom output | • Duplicates logic<br>• No reuse |

---

## 5. Recommended Approach by Context

### For General Code / Tests
**Use:** `calculate_p95()` from `needle::stats`
```rust
use needle::stats::calculate_p95;
let p95 = calculate_p95(&latencies);
```

### For Benchmark Suites
**Use:** Criterion.rs with proper configuration
```rust
let mut group = c.benchmark_group("latency_percentiles");
group.bench_function("p95_100kb", |b| {
    b.iter(|| { /* benchmark code */ });
});
```

### For Assertion Tests with Custom Output
**Use:** Manual inline calculation with explicit output
```rust
latencies.sort();
let p95 = latencies[(latencies.len() * 95) / 100];
eprintln!("P95: {:.2} ms", p95);
```

---

## 6. All P95 Calculation Locations

1. **`src/stats/mod.rs`** - `calculate_p95()` function (lines 385-394)
2. **`src/sanitize/mod.rs`** - Performance assertion test (line 797)
3. **`benches/sanitize.rs`** - Five manual calculations:
   - Line 216: `bench_sanitize_10kb`
   - Line 270: `bench_sanitize_100kb`
   - Line 324: `bench_sanitize_1mb`
   - Line 426: `assertion_test`
   - Line 470: `bench_median_latency`

---

## 7. Criterion.rs Dependency Status

**Status:** ✅ Present and actively used

**Evidence:**
- Listed in `[dev-dependencies]` in `Cargo.toml`
- Configuration file exists: `criterion.toml`
- Benchmark harness configured: `benches/sanitize.rs`
- Benchmark target defined in `Cargo.toml`:
  ```toml
  [[bench]]
  name = "sanitize"
  harness = false
  ```

**Usage Pattern:**
- Primarily for performance regression testing
- Provides statistical rigor beyond simple percentiles
- Generates HTML reports at `./target/criterion/`

---

## 8. Algorithm Differences

### Custom Implementation (Nearest-Rank)
- Deterministic: Always returns an actual input value
- No interpolation: Returns value at exact index
- Simple: Easy to understand and verify
- Standard: De facto standard for latency reporting

### Criterion.rs (Bootstrap Analysis)
- Statistical: Uses resampling for confidence intervals
- Sophisticated: Accounts for distribution shape
- Production-grade: Suitable for published benchmarks
- Overhead: More computation, not suitable for hot paths

---

## Acceptance Criteria Status

| Criterion | Status | Evidence |
|-----------|--------|----------|
| List all locations where p95 is calculated | ✅ | 7 locations documented above |
| Identify Criterion.rs dependency status | ✅ | Present in dev-dependencies, actively used |
| Document which approach is recommended | ✅ | Context-based recommendations provided |

---

## Conclusion

NEEDLE uses a **hybrid approach** for p95 calculations:

1. **Production code:** Custom `calculate_p95()` for simplicity and no external dependencies
2. **Benchmarks:** Criterion.rs for statistical rigor and reporting
3. **Assertions:** Manual inline for explicit control and immediate output

This approach balances simplicity, performance, and statistical correctness across different contexts.
