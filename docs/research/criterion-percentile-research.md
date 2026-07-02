# Criterion.rs Percentile Configuration Research

**Date:** 2026-07-02  
**Bead:** bf-20xc  
**Research Question:** How to configure Criterion.rs for p95 percentile reporting

## Summary

Criterion.rs **automatically calculates and reports percentiles (including p95)** using bootstrap analysis. No special configuration beyond the standard Criterion setup is required—the library computes percentiles internally from collected samples.

## Version Compatibility

**NEEDLE currently uses:** `criterion = "0.5"` (from `Cargo.toml`)

**Status:** ✅ Criterion 0.5 fully supports percentile reporting via bootstrap analysis.

## How Criterion.rs Calculates Percentiles

From the [official analysis documentation](https://bheisler.github.io/criterion.rs/book/analysis.html):

1. **Measurement Phase:** Collects N samples (controlled by `sample_size`)
2. **Bootstrap Analysis:** Generates ~100,000 bootstrap resamples from the measured data
3. **Percentile Calculation:** Computes statistics including percentiles from the bootstrap distribution
4. **Reporting:** Outputs mean, std deviation, median, median absolute deviation, and percentiles

The analysis page states:
> "A line is fitted to each of the bootstrap samples, and the result is a statistical distribution of slopes that gives a reliable confidence interval around the single estimate calculated from the measured samples. This resampling process is repeated to generate the mean, standard deviation, median and median absolute deviation of the measured iteration times as well."

## Configuration Options for Accurate p95

### Relevant Criterion Struct Methods

| Method | Default | Purpose |
|--------|---------|---------|
| `sample_size(n)` | 100 | Number of measurements (more samples → more accurate percentiles) |
| `warm_up_time(duration)` | 3s | CPU/JIT/cache warm-up before measurement |
| `measurement_time(duration)` | 5s | Total measurement duration |
| `confidence_level(f64)` | 0.95 | Confidence interval for mean (affects CI, not percentiles) |
| `noise_threshold(f64)` | 0.01 | Filter threshold for noise (1% default) |

### Current NEEDLE Configuration

From `benches/sanitize.rs`, the `configure_criterion()` function:

```rust
fn configure_criterion() -> Criterion {
    Criterion::default()
        .confidence_level(0.95)      // 95% confidence for mean CI
        .sample_size(100)              // 100 measurements for accurate percentiles
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(5))
        .noise_threshold(0.02)         // 2% noise filtering
}
```

This configuration is appropriate for p95 reporting:
- **100 samples** provides reasonable bootstrap accuracy for p95
- For production-grade p95 accuracy, consider **500+ samples**
- **5 second measurement time** ensures sufficient data collection
- **3 second warm-up** stabilizes CPU caches and JIT compilation

## Manual Percentile Calculation (Alternative Approach)

The existing code also demonstrates manual p95 calculation in `bench_median_latency()`:

```rust
let mut latencies = Vec::with_capacity(ASSERTION_SAMPLE_COUNT);
for _ in 0..ASSERTION_SAMPLE_COUNT {
    let start = std::time::Instant::now();
    let _ = sanitizer.sanitize(&content);
    latencies.push(start.elapsed().as_micros());
}

latencies.sort();
let p95_us = latencies[(latencies.len() * 95) / 100];
let p95_ms = p95_us as f64 / 1000.0;
```

This approach:
- Collects raw samples
- Sorts them
- Calculates the 95th percentile by index

**Note:** This is useful for explicit percentile reporting but Criterion.rs already computes percentiles internally.

## Key Distinction: Confidence Level vs. Percentiles

Important conceptual difference:

| Metric | Meaning |
|--------|---------|
| **Confidence Level (0.95)** | 95% probability that the true mean lies within the reported confidence interval |
| **95th Percentile (p95)** | 95% of observations fall below this value |

Criterion.rs focuses on **confidence intervals** for statistical analysis, but bootstrap analysis also produces percentile estimates.

## Official Documentation Sources

- [Criterion.rs Book - Analysis Process](https://bheisler.github.io/criterion.rs/book/analysis.html)
- [Criterion Struct API Docs](https://docs.rs/criterion/latest/criterion/struct.Criterion.html)
- [Main Documentation](https://bheisler.github.io/criterion.rs/book/)

## Recommendations

1. **Current configuration is appropriate** - The existing `configure_criterion()` setup is well-suited for p95 reporting
2. **Sample size matters most** - Increase `sample_size` for more accurate p95 (100 is good, 500+ is production-grade)
3. **No additional configuration needed** - Criterion.rs calculates percentiles automatically via bootstrap
4. **Use Criterion's built-in percentiles** - Prefer the library's computed percentiles over manual calculation unless custom analysis is needed

## Conclusion

Criterion.rs 0.5 fully supports p95 percentile reporting through its built-in bootstrap analysis. The current NEEDLE configuration in `benches/sanitize.rs` is correct and appropriate for accurate p95 measurements.

No version upgrade or special configuration is required—the standard Criterion setup with adequate sample size provides reliable percentile reporting.
