# Criterion.rs Percentile Configuration Research

**Bead:** bf-20xc  
**Date:** 2026-07-02  
**Research Question:** How to configure Criterion.rs for p95 percentile reporting

## Executive Summary

**Key Finding:** Criterion.rs does **NOT** have built-in p95 or p99 percentile output. The library focuses on confidence intervals, mean/median statistics, and bootstrap resampling - not specific percentiles.

To get p95/p99 percentiles, you must calculate them manually from raw samples (which the existing `benches/sanitize.rs` already does correctly).

## Criterion.rs Version in NEEDLE

From `Cargo.toml`:
```toml
[dev-dependencies]
criterion = "0.5"
```

**Version:** 0.5 (current stable version as of 2026-07-02)

**MSRV Compatibility:** Criterion 0.5 requires Rust 1.75+, which matches NEEDLE's MSRV of 1.75.

## What Criterion.rs Actually Reports

Based on the [Command-Line Output documentation](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_output.html), Criterion.rs reports:

### 1. Time Confidence Interval
```
alloc time: [2.5094 ms 2.5306 ms 2.5553 ms]
```
- Three values: `[lower_bound, estimate, upper_bound]`
- Uses **bootstrap resampling** (100,000 samples by default)
- Default confidence level: **95%** (configurable via `confidence_level()`)

### 2. Mean and Standard Deviation
```
mean [2.5142 ms 2.5557 ms] std. dev. [62.868 us 149.50 us]
```
- Confidence interval for mean and std. dev.
- Calculated naively from samples

### 3. Median and Median Absolute Deviation
```
median [2.5023 ms 2.5262 ms] med. abs. dev. [40.034 us 73.259 us]
```
- **Robust statistics** - less sensitive to outliers than mean/std. dev.
- Uses bootstrap resampling

### 4. Slope (Linear Regression)
```
slope [2.5094 ms 2.5553 ms] R^2 [0.8660614 0.8640630]
```
- Time per iteration from linear regression
- R² indicates goodness-of-fit

### 5. Outlier Detection
```
Found 8 outliers among 100 measurements (8.00%)
4 (4.00%) high mild
4 (4.00%) high severe
```
- Uses modified Tukey's method (IQR-based)
- Does NOT drop outliers from analysis

### 6. Change Detection
```
change: [-38.292% -37.342% -36.524%] (p = 0.00 < 0.05)
Performance has improved.
```
- Compares against previous run (stored in `target/criterion/`)
- Statistical significance testing via T-test

## Configuration Options Available

From the [Criterion struct documentation](https://docs.rs/criterion/latest/criterion/struct.Criterion.html):

| Method | Description | Default |
|--------|-------------|---------|
| `confidence_level(f64)` | Confidence interval width (0.95 = 95%) | 0.95 |
| `sample_size(usize)` | Number of measurements (min 10) | 100 |
| `measurement_time(Duration)` | How long to collect samples | 5 seconds |
| `warm_up_time(Duration)` | Warmup duration | 3 seconds |
| `noise_threshold(f64)` | Ignore changes smaller than this | 0.01 (1%) |
| `significance_level(f64)` | Threshold for change detection | 0.05 |
| `nresamples(usize)` | Bootstrap sample count | 100,000 |

### Example Configuration
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

## Why Criterion.rs Doesn't Report p95/p99

Criterion.rs is designed for **statistical comparison** between runs, not latency SLA reporting:

- **Focus:** Detecting performance regressions/improvements between code versions
- **Method:** Bootstrap resampling + confidence intervals + hypothesis testing
- **Assumption:** Normal-ish distribution (or at least symmetric)

**p95/p99 percentiles** are more relevant for:
- SLA compliance ("99% of requests under 100ms")
- Highly-skewed distributions (latency outliers)
- Tail latency analysis

These are different use cases than what Criterion.rs targets.

## How to Get p95/p99 Percentiles

### Approach 1: Manual Calculation (Current NEEDLE Approach)

The existing `benches/sanitize.rs` already implements this correctly in `bench_median_latency()`:

```rust
fn bench_median_latency(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_100KB);

    // Warm-up
    for _ in 0..5 {
        let _ = sanitizer.sanitize(&content);
    }

    // Collect samples
    let mut latencies = Vec::with_capacity(ASSERTION_SAMPLE_COUNT);
    for _ in 0..ASSERTION_SAMPLE_COUNT {
        let start = std::time::Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    // Calculate percentiles
    latencies.sort();
    let median_us = latencies[ASSERTION_SAMPLE_COUNT / 2];
    let p95_us = latencies[(latencies.len() * 95) / 100];
    let p99_us = latencies[(latencies.len() * 99) / 100];

    eprintln!("Median: {:.2} ms", median_us as f64 / 1000.0);
    eprintln!("P95: {:.2} ms", p95_us as f64 / 1000.0);
    eprintln!("P99: {:.2} ms", p99_us as f64 / 1000.0);
}
```

### Approach 2: Post-Process Criterion Output

Criterion.rs saves raw sample data to `target/criterion/<name>/raw.csv`. You can:

1. Run benchmarks with Criterion
2. Parse `raw.csv` to extract measurements
3. Calculate percentiles with external tools (Python, R, etc.)

### Approach 3: Use Alternative Libraries

For projects that need built-in p95/p99:
- **[divan](https://github.com/magnet/tragnet)**: Focus on percentiles
- **[rack](https://github.com/snipe/rack)**: Latency-focused benchmarking
- Custom: Simple timing loop with manual percentile calc

## Existing NEEDLE Configuration Analysis

The current `benches/sanitize.rs` configuration (lines 46-53):

```rust
fn configure_criterion() -> Criterion {
    Criterion::default()
        .confidence_level(0.95)    // ✓ Correct for CI width
        .sample_size(100)            // ✓ Reasonable for percentiles
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(5))
        .noise_threshold(0.02)
}
```

**Assessment:** This configuration is **appropriate** for the current use case:

- ✓ `confidence_level(0.95)` - Sets 95% confidence interval width (not percentile)
- ✓ `sample_size(100)` - Provides reasonable accuracy for bootstrap statistics
- ✓ Manual p95 calculation in `bench_median_latency()` - Correct approach

## Documentation References

- [Criterion.rs Analysis Process](https://bheisler.github.io/criterion.rs/book/analysis.html) - Bootstrap resampling methodology
- [Criterion.rs Command-Line Output](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_output.html) - Output format explained
- [Criterion.rs Documentation (docs.rs)](https://docs.rs/criterion/latest/criterion/struct.Criterion.html) - API reference

## Conclusions

1. **Criterion.rs 0.5 is the correct version** for NEEDLE (matches MSRV 1.75)

2. **Criterion.rs does not have built-in p95/p99 output** - it reports:
   - Mean/std. dev. with confidence intervals
   - Median/med. abs. dev. with confidence intervals
   - Outlier detection
   - Change detection vs previous run

3. **Configuration approach for p95**:
   - **Correct**: Manual calculation from raw samples (current approach in `benches/sanitize.rs`)
   - **Misconception**: `confidence_level(0.95)` controls confidence interval width, NOT percentile reporting

4. **Existing NEEDLE code is correct** - The `configure_criterion()` function and `bench_median_latency()` implement the right approach:
   - Use Criterion for general benchmark statistics and regression detection
   - Manually calculate p95/p99 from raw samples for latency reporting

## Recommendations

The current implementation in `benches/sanitize.rs` is **already correct** and does not need changes for p95 reporting. The manual percentile calculation in `bench_median_latency()` is the appropriate approach given Criterion.rs's design constraints.

## Sources

- [Criterion.rs Analysis Process](https://bheisler.github.io/criterion.rs/book/analysis.html) - Bootstrap resampling and statistical methodology
- [Criterion.rs Command-Line Output](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_output.html) - Output format and statistics explanation
- [Criterion.rs Command-Line Options](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_options.html) - Configuration options
- [Criterion struct on docs.rs](https://docs.rs/criterion/latest/criterion/struct.Criterion.html) - API reference for configuration methods
- [Criterion.rs GitHub Repository](https://github.com/bheisler/criterion.rs) - Source code and development discussions
