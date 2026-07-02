# Criterion.rs Percentile Configuration Research

## Task: Research Criterion.rs options for percentile reporting (p95 latency)

## Executive Summary

**Criterion.rs does NOT provide direct p95/p99 percentile reporting.** The library is designed for **mean execution time with confidence intervals**, not latency percentile distributions.

## Criterion.rs Statistical Output

### What Criterion.rs Provides

| Metric | Description | Default/Config |
|--------|-------------|----------------|
| **Mean Execution Time** | Primary benchmark metric | Measured and reported |
| **95% Confidence Interval** | Statistical confidence around mean | Default: `0.95` (configurable via `confidence_level()`) |
| **Median** | Median execution time | Shown in bencher output format |
| **Throughput** | Bytes/elements per second | Optional (via `Throughput` enum) |
| **Outlier Detection** | Identifies outliers using Tukey's method | Uses quartiles/IQR |

### What Criterion.rs Does NOT Provide

- **P50, P95, P99, P99.9 latency percentiles** - Not available in any output format
- **Latency distribution graphs** - Not part of standard output
- **Tail latency analysis** - Not a design goal of the library

## Configuration Options

### Code Configuration

```rust
use criterion::*;

// Configure confidence level (controls confidence interval width)
 Criterion::default()
    .confidence_level(0.95)  // 95% confidence interval (default)
    .significance_level(0.05) // statistical significance threshold
    
// Or per-benchmark group
let mut group = c.benchmark_group("my-group");
group.confidence_level(0.99);
```

### Command-Line Options

```bash
# Change output format
cargo bench -- --output-format bencher  # Shows median, not percentiles

# Quick mode (not for accurate measurements)
cargo bench -- --quick
```

### Available Output Formats

| Format | Description |
|--------|-------------|
| `criterion` (default) | Full output with mean, confidence intervals, outlier counts, throughput |
| `bencher` | Simplified format showing median (for compatibility with bencher/libtest) |

## Sources

- [Criterion.rs Book - Advanced Configuration](https://bheisler.github.io/criterion.rs/book/user_guide/advanced_configuration.html)
- [Criterion.rs Book - Command-Line Options](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_options.html)
- [Criterion.rs Book - Analysis Process](https://bheisler.github.io/criterion.rs/book/analysis.html)
- [Criterion struct on docs.rs](https://docs.rs/criterion/latest/criterion/struct.Criterion.html)
- [Criterion.rs CHANGELOG](https://github.com/bheisler/criterion.rs/blob/master/CHANGELOG.md)

## Conclusion

Criterion.rs is not suitable for p95/p99 latency reporting. If you need latency percentiles, consider:

1. **Custom measurement**: Use Criterion's lower-level APIs to collect individual samples and calculate percentiles yourself
2. **Alternative tools**: Use benchmarking tools designed for latency distribution analysis
3. **Accept mean-based metrics**: Criterion.rs excels at detecting performance regressions in mean execution time, which is often sufficient for optimization work

## Version Compatibility

- **NEEDLE currently uses**: `criterion = "0.5"`
- **Latest version**: 0.7.0 (released 2025-07-25)
- **MSRV for 0.5**: Rust 1.64+
- **No breaking changes to statistical output**: Versions 0.5 through 0.7 maintain the same statistical model (mean + confidence intervals)
