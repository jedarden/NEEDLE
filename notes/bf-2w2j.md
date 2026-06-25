# Criterion.rs Percentile Configuration Research

## Task
Research Criterion.rs percentile configuration for NEEDLE benchmarking.

## Current State

**File**: `benches/sanitize.rs`
**Criterion Version**: 0.5 (from Cargo.toml)

NEEDLE already implements **custom percentile calculation** that is superior to Criterion's built-in capabilities.

## Key Finding

**Criterion.rs does NOT provide direct p95/p99 percentile configuration** through its API.

The framework focuses on:
- Mean and median with confidence intervals
- Bootstrap-based statistical analysis (default: 100,000 resamples)
- Significance testing for performance regression detection

## Criterion.rs Statistical Features

### Built-in Configuration Options

```rust
Criterion::default()
    .confidence_level(0.95)      // Default: 0.95 confidence interval
    .nresamples(100_000)         // Default: 100,000 bootstrap iterations
    .sample_size(100)            // Default: 100 measurements
    .significance_level(0.05)    // Default: 0.05
    .measurement_time(Duration::from_secs(10))  // Default: 5 seconds
```

### What Criterion Reports

- Mean with 95% confidence interval
- Median with Median Absolute Deviation (MAD)
- Slope analysis (throughput benchmarks)
- Regression detection via statistical significance testing
- Additional statistics via `--verbose` CLI flag

### What Criterion Does NOT Report

- **No p95 percentile**
- **No p99 percentile**
- No custom percentile configuration

## NEEDLE's Current Implementation (Recommended)

**File**: `benches/sanitize.rs` (lines 38-61)

NEEDLE's custom percentile implementation is **the recommended approach**:

```rust
fn report_percentiles(latencies_us: &[u128], benchmark_name: &str) -> u128 {
    let mut sorted = latencies_us.to_vec();
    sorted.sort();
    
    let len = sorted.len();
    let median_us = sorted[len / 2];
    let p95_us = sorted[(len * 95) / 100];
    let p99_us = sorted[(len * 99) / 100];
    
    eprintln!("{}: Latency percentiles", benchmark_name);
    eprintln!("  Median: {:.2} ms", median_us as f64 / 1000.0);
    eprintln!("  P95:    {:.2} ms", p95_us as f64 / 1000.0);
    eprintln!("  P99:    {:.2} ms", p99_us as f64 / 1000.0);
    
    median_us
}
```

This implementation:
- Calculates exact percentiles from sample data
- Reports median, p95, p99 in milliseconds
- Outputs to stderr for easy capture
- Integrates with Criterion's other statistical features

## Recommended Approach

**No changes needed** — NEEDLE's current implementation is optimal:

1. **Keep custom percentile calculation** — it provides metrics Criterion cannot
2. **Use Criterion for regression detection** — leverage its statistical analysis
3. **Combine both approaches** — Criterion for CI/CD regression detection, custom percentiles for latency reporting
4. **Use `--verbose` flag** when running benchmarks for additional Criterion statistics

### Example Integration Pattern

```rust
fn bench_with_percentiles(c: &mut Criterion) {
    let mut group = c.benchmark_group("sanitize_100kb");
    
    // Collect samples for custom percentiles
    let sample_count = 100;
    let mut latencies = Vec::with_capacity(sample_count);
    
    for _ in 0..sample_count {
        let start = std::time::Instant::now();
        sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }
    
    // Report custom percentiles (p95, p99)
    report_percentiles(&latencies, "sanitize_100kb");
    
    // Run Criterion benchmark for statistical analysis (regression detection)
    group.sample_size(sample_count);
    group.measurement_time(Duration::from_secs(10));
    group.bench_function("throughput", |b| {
        b.iter(|| sanitizer.sanitize(black_box(&content)));
    });
    group.finish();
}
```

## Configuration Requirements

### For p95/p99 Reporting
**None** — Criterion has no built-in support. Use custom implementation.

### For Enhanced Statistical Output
Run benchmarks with the `--verbose` flag:
```bash
cargo bench -- --verbose
```

### For Custom Confidence Intervals
```rust
Criterion::default()
    .confidence_level(0.99)  // 99% confidence instead of 95%
```

## References

- [Criterion.rs Advanced Configuration](https://bheisler.github.io/criterion.rs/book/user_guide/advanced_configuration.html)
- [Criterion.rs Command-Line Output](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_output.html)
- [Criterion.rs Analysis Process](https://bheisler.github.io/criterion.rs/book/analysis.html)
- [Criterion.rs API Documentation](https://docs.rs/criterion/latest/criterion/)

## Conclusion

Criterion.rs is designed for **regression detection** through statistical analysis, not **detailed percentile reporting**. NEEDLE's current custom implementation provides the missing percentile metrics and should be retained.
