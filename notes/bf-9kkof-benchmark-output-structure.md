# Benchmark Output Structure Analysis (Bead bf-9kkof)

## Overview

The NEEDLE project uses Criterion.rs for benchmarking, with a single benchmark suite defined in `benches/sanitize.rs`. This document captures the complete output structure and identifies where statistics appear.

## Benchmark Execution

```bash
cargo bench --bench sanitize
```

## Output Streams

### Standard Output (stdout)

Criterion.rs writes benchmark results to stdout in the following format:

#### Benchmark Result Format

```
<benchmark_name>
                        time:   [<lower> <mean> <upper>]
                        thrpt:  [<lower> <mean> <upper>]
                 change:
                        time:   [<%change_lower> <%change_mean> <%change_upper>] (p = <p_value> < 0.05)
                        thrpt:  [<%change_lower> <%change_mean> <%change_upper>]
                        Performance has <improved|regressed>.
Found <count> outliers among <total> measurements (<percentage>%)
  <count> (<percentage>%) <severity> <severity>
```

#### Example Output

```
sanitize_10kb/throughput_bytes
                        time:   [728.07 µs 733.68 µs 738.84 µs]
                        thrpt:  [13.217 MiB/s 13.310 MiB/s 13.413 MiB/s]
                 change:
                        time:   [-7.1826% -5.7675% -4.6305%] (p = 0.00 < 0.05)
                        thrpt:  [+4.8554% +6.1205% +7.7384%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
```

### Standard Error (stderr)

**Issue Identified:** Custom `eprintln!` statements from the benchmark code (lines 217-220, 270-273, 323-326 in `benches/sanitize.rs`) are **not appearing** in stderr output. These should print P95 latency measurements:

```rust
// Expected but NOT appearing in stderr:
eprintln!(
    "10KB trace p95 latency: {:.2} ms ({} samples)",
    p95_ms, ASSERTION_SAMPLE_COUNT
);
```

**Hypothesis:** Criterion.rs may be suppressing or redirecting stderr output during benchmark execution.

## JSON Report Structure

Criterion generates detailed JSON reports in `target/criterion/`:

### Directory Structure

```
target/criterion/
├── sanitize_10kb/
│   ├── throughput_bytes/
│   │   ├── new/
│   │   │   ├── benchmark.json
│   │   │   ├── estimates.json
│   │   │   ├── sample.json
│   │   │   └── tukey.json
│   │   └── base/
│   └── throughput_ops/
├── sanitize_100kb/
├── sanitize_1mb/
├── latency_percentiles/
└── report/
```

### benchmark.json

Contains benchmark metadata:

```json
{
    "group_id": "sanitize_10kb",
    "function_id": "throughput_bytes",
    "value_str": null,
    "throughput": {"Bytes": 10240},
    "full_id": "sanitize_10kb/throughput_bytes",
    "directory_name": "sanitize_10kb/throughput_bytes",
    "title": "sanitize_10kb/throughput_bytes"
}
```

### estimates.json

Contains statistical calculations:

```json
{
    "mean": {
        "confidence_interval": {
            "confidence_level": 0.95,
            "lower_bound": 752183.47,
            "upper_bound": 760431.27
        },
        "point_estimate": 756007.22,
        "standard_error": 2096.00
    },
    "median": {
        "confidence_interval": {
            "confidence_level": 0.95,
            "lower_bound": 758662.94,
            "upper_bound": 759294.88
        },
        "point_estimate": 758954.01,
        "standard_error": 175.88
    },
    "median_abs_dev": {
        "confidence_interval": {
            "confidence_level": 0.95,
            "lower_bound": 1482.01,
            "upper_bound": 3267.04
        },
        "point_estimate": 2222.76,
        "standard_error": 443.43
    },
    "slope": {
        // Linear regression slope (ns/iter)
    },
    "std_dev": {
        "confidence_interval": {
            "confidence_level": 0.95,
            "lower_bound": <value>,
            "upper_bound": <value>
        },
        "point_estimate": <value>,
        "standard_error": <value>
    }
}
```

**Note:** Estimates.json does **NOT** contain P95/P99 percentiles. Criterion calculates confidence intervals for mean/median but does not store percentiles in JSON.

### sample.json

Contains raw iteration data:

```json
{
    "sampling_mode": "Linear",
    "iters": [2.0, 4.0, 6.0, ..., 200.0],
    "times": [1499894.0, 3007379.0, ..., 149943386.0]
}
```

## Benchmark Groups

The benchmark suite defines multiple groups:

### 1. Throughput by Bytes ( sanitize_10kb/throughput_bytes )
- **Metric:** Bytes/second (MiB/s)
- **Sample size:** 10KB, 100KB, 1MB traces
- **Statistics:** Mean time, throughput, confidence intervals

### 2. Throughput by Operations ( sanitize_10kb/throughput_ops )
- **Metric:** Operations/second (elem/s or Kelem/s)
- **Sample size:** Single operations on 10KB, 100KB, 1MB traces
- **Statistics:** Mean time, throughput, confidence intervals

### 3. Latency Percentiles ( latency_percentiles/p95_100kb )
- **Metric:** Raw time (ms)
- **Purpose:** P95 latency measurement for 100KB traces
- **Custom calculation:** Uses `needle::stats::calculate_p95()` in benchmark code
- **Issue:** Custom P95 output not appearing in stderr

### 4. Skip Statistics ( report_skip_stats )
- **Metric:** Keyword pre-filter skip rate
- **Purpose:** Measures Aho-Corasick efficiency
- **Issue:** Output not appearing in stderr

## Configuration

From `criterion.toml`:

```toml
criterion_home = "./target/criterion"
output_format = "verbose"        # Controls stdout verbosity
ploting_backend = "auto"
default_sample_size = 10         # Benchmark iterations (overridden by code)
warm_up_time = 3                 # Seconds
measurement_time = 5             # Seconds
```

From code (`benches/sanitize.rs`):

```rust
fn configure_criterion() -> Criterion {
    Criterion::default()
        .confidence_level(0.95)
        .sample_size(100)              // 100 measurements
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(5))
        .noise_threshold(0.02)
}
```

## Statistics Section Identification

### Where Statistics Currently Appear

1. **Console stdout:** Mean, median, MAD, confidence intervals (via Criterion)
2. **JSON estimates:** Mean, median, MAD, std_dev, slope (via Criterion)
3. **JSON sample:** Raw iteration times

### Where Statistics SHOULD Appear (But Don't)

1. **Console stderr:** Custom P95 latency measurements (missing)
2. **Console stderr:** Skip statistics (missing)
3. **JSON estimates:** P95/P99 percentiles (not calculated by Criterion)

### Gap Analysis

| Statistic | Location | Status |
|-----------|----------|--------|
| Mean time | stdout + JSON | ✅ Present |
| Median time | stdout + JSON | ✅ Present |
| MAD | stdout + JSON | ✅ Present |
| Std dev | JSON only | ✅ Present |
| Confidence intervals | stdout + JSON | ✅ Present |
| **P95 latency** | stderr (expected) | ❌ Missing |
| **P99 latency** | stderr (expected) | ❌ Missing |
| **Skip rate** | stderr (expected) | ❌ Missing |
| Throughput (bytes) | stdout | ✅ Present |
| Throughput (ops) | stdout | ✅ Present |
| Outliers | stdout | ✅ Present |

## Recommendations

1. **Fix custom stderr output:** Investigate why `eprintln!` statements are suppressed
2. **Add percentiles to JSON:** Extend Criterion or post-process to store P95/P99 in JSON
3. **Consolidate statistics:** Consider generating a unified statistics JSON file combining Criterion estimates with custom calculations
