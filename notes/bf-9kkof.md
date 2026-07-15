# Benchmark Output Structure - bf-9kkof

## Task

Capture and inspect benchmark output structure for the NEEDLE project.

## Benchmark Execution

Benchmark executed successfully:
```bash
cargo bench --bench sanitize
```

## Output Structure

The benchmark uses Criterion.rs and produces structured output with the following sections per benchmark:

### 1. Benchmark Name
```
sanitize_10kb/throughput_bytes
```
Format: `{test_name}/{metric_type}`

### 2. Time Measurements
```
time:   [783.08 µs 786.41 µs 790.32 µs]
```
- Format: `[lower_bound mean upper_bound]`
- Units vary by measurement (µs, ms, s)
- Represents confidence interval around the mean

### 3. Throughput Measurements
```
thrpt:  [12.357 MiB/s 12.418 MiB/s 12.471 MiB/s]
```
- Format: `[lower_bound mean upper_bound]`
- Units vary (MiB/s, Kelem/s, elem/s)
- Shows operations/bytes per second

### 4. Change Section (Comparison to Baseline)
```
change:
        time:   [+4.9542% +5.5568% +6.2870%] (p = 0.00 < 0.05)
        thrpt:  [-5.9151% -5.2642% -4.7203%]
        Performance has regressed.
```
- Shows percent change from previous baseline
- Includes p-value for statistical significance
- Status message: "Performance has regressed" / "No change in performance detected"

### 5. Outlier Information
```
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
```
- Count of outliers detected
- Classification: low severe, low mild, high mild, high severe

## Statistics Location

### Primary Statistics
The following statistics appear in the standard output:

1. **Mean with confidence interval** - lines 2-3 of each benchmark
2. **Throughput** - line 4 of each benchmark  
3. **Percent change** - change section
4. **P-value** - statistical significance test
5. **Outlier count and classification** - final section of each benchmark

### P95 Percentile
The benchmark includes a dedicated P95 benchmark:
```
latency_percentiles/p95_100kb
                        time:   [8.0228 ms 8.0937 ms 8.1743 ms]
```

This P95 calculation uses the `calculate_p95()` function from `src/stats/mod.rs` which implements:
- Linear interpolation for accurate percentile estimation
- Handles edge cases (empty, single element, small samples)
- Uses formula: `rank = 0.95 * (n - 1)` then interpolates

### Statistics Module (`src/stats/mod.rs`)

The stats module provides:

1. **P95 Calculation** - `calculate_p95()` function
   - Used by benchmarks for latency percentiles
   - Linear interpolation method (matches Criterion.rs)
   
2. **P95Collector** - Aggregates samples across iterations
   - Records individual latency measurements
   - Calculates p95 on pooled data (statistically sound)
   - Provides min/max/avg statistics

3. **VariantStats** - For A/B testing template variants
   - Success rates, duration tracking
   - Used by `needle stats` command

4. **StatsAggregator** - Processes telemetry JSONL logs
   - Correlates dispatch/outcome/completed events
   - Produces per-variant statistics

## Criterion Configuration

Located in `/home/coding/NEEDLE/criterion.toml`:
- Output format: `verbose`
- Sample size: 10
- Warm-up time: 3 seconds
- Measurement time: 5 seconds
- Percentiles configured for reporting
- Data saved to: `./target/criterion`

## Benchmark Results Summary

The benchmark ran 7 test cases:
1. `sanitize_10kb/throughput_bytes` - 786.41 µs mean
2. `sanitize_10kb/throughput_ops` - 783.04 µs mean
3. `sanitize_100kb/throughput_bytes` - 8.4356 ms mean
4. `sanitize_100kb/throughput_ops` - 8.3719 ms mean
5. `sanitize_1mb/throughput_bytes` - 87.064 ms mean
6. `sanitize_1mb/throughput_ops` - 82.314 ms mean
7. `latency_percentiles/p95_100kb` - 8.0937 ms mean

All statistics are displayed with:
- 95% confidence intervals
- Statistical significance testing (p-values)
- Performance regression detection
- Outlier identification

## Output Files

- Full benchmark output captured in: `/home/coding/NEEDLE/benchmark_output.txt`
- Detailed reports available in: `./target/criterion/` directory

## Acceptance Criteria Status

✅ Output is captured completely - Full output saved to `benchmark_output.txt`
✅ Output structure is documented - Structure analyzed and documented above
✅ Statistics section is identified - Statistics identified in both CLI output and stats module
