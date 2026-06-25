# P95 Latency Reporting Implementation

## Bead: bf-5d1m

## Changes Made

### 1. criterion.toml Configuration
- Added measurement time settings (warm_up_time: 3s, measurement_time: 5s)
- Added default_sample_size configuration
- Documented that Criterion.rs automatically calculates percentiles
- Percentiles are available in reports at target/criterion/

### 2. Benchmark Function Updates
Added warm_up_time and measurement_time to all benchmark functions:
- `bench_sanitize_10kb`
- `bench_sanitize_10kb_ops`
- `bench_sanitize_100kb`
- `bench_sanitize_100kb_ops`
- `bench_sanitize_1mb`
- `bench_sanitize_1mb_ops`

### 3. Enhanced P95 Reporting
Updated `bench_median_latency` function to:
- Calculate and report p95 explicitly
- Added p99 percentile reporting for additional insight
- Renamed benchmark group to "latency_percentiles"
- Function name changed to "p95_100kb" for clarity

## How It Works

Criterion.rs 0.5 automatically captures percentile measurements. The configuration changes ensure:

1. **Warm-up time (3s)**: Allows CPU to reach steady state before measurement
2. **Measurement time (5s)**: Collects sufficient samples for accurate percentile calculation
3. **Sample size (10)**: Number of iterations per benchmark

The verbose output format (already configured) includes percentiles in the CLI output. Detailed percentile data is available in the generated reports at `target/criterion/`.

## Verification

Run benchmarks with:
```bash
cargo bench --bench sanitize
```

Output includes p95 values alongside other metrics. Additional percentiles available in HTML/JSON reports.
