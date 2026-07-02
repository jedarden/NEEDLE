# Criterion.rs p95 Configuration Verification

**Date:** 2026-07-02  
**Bead ID:** bf-68dh  
**Task:** Verify Criterion.rs dependency and version support for p95

## Summary

Criterion.rs 0.5 supports p95 percentile reporting through automatic calculation and reporting. The current benchmark harness is properly configured for p95 measurement with appropriate sample sizes and timing parameters.

## Criterion.rs Dependency

**Version:** 0.5  
**Location:** `Cargo.toml` (line 123)  
**Entry:** `criterion = "0.5"` (dev-dependencies)

Criterion.rs 0.5 fully supports p95 percentile calculation and reporting through its built-in bootstrap analysis.

## Current Configuration

The benchmark harness defines a `configure_criterion()` function (benches/sanitize.rs:46-53) with the following settings:

| Parameter | Value | Purpose |
|-----------|-------|---------|
| `confidence_level` | 0.95 | 95% confidence interval for reported statistics |
| `sample_size` | 100 | Number of measurements for accurate percentile estimation |
| `warm_up_time` | 3 seconds | CPU cache/JIT warm-up period |
| `measurement_time` | 5 seconds | Duration for sample collection |
| `noise_threshold` | 0.02 | 2% noise filtering for stable measurements |

### p95 Accuracy Notes

From the code documentation (benches/sanitize.rs:31-52):
- Criterion.rs automatically calculates and reports p95 when given sufficient samples
- The key for accurate p95 is `sample_size` - more samples = more accurate bootstrap percentile estimation
- 100 samples provides reasonable accuracy for p95
- For production-grade p95 accuracy, consider 500+ samples

## Benchmark Functions Inventory

All benchmark functions in the harness use the configured Criterion instance:

1. **`bench_sanitize_10kb`** (lines 198-211)
   - Throughput: Bytes (10KB)
   - Status: ✅ Uses configured Criterion

2. **`bench_sanitize_10kb_ops`** (lines 214-227)
   - Throughput: Operations per second
   - Status: ✅ Uses configured Criterion

3. **`bench_sanitize_100kb`** (lines 230-243)
   - Throughput: Bytes (100KB)
   - Status: ✅ Uses configured Criterion

4. **`bench_sanitize_100kb_ops`** (lines 246-259)
   - Throughput: Operations per second
   - Status: ✅ Uses configured Criterion

5. **`bench_sanitize_1mb`** (lines 262-275)
   - Throughput: Bytes (1MB)
   - Status: ✅ Uses configured Criterion

6. **`bench_sanitize_1mb_ops`** (lines 278-291)
   - Throughput: Operations per second
   - Status: ✅ Uses configured Criterion

7. **`bench_median_latency`** (lines 385-433)
   - Specialized latency benchmark with manual p95 calculation
   - Status: ✅ Uses configured Criterion + manual percentile reporting

8. **`report_skip_stats`** (lines 294-311)
   - Skip rate statistics (not a performance benchmark)
   - Status: N/A (observational, not measured)

## p95 Output Analysis

### Criterion Automatic p95

All benchmark functions that use `c.benchmark_group()` receive automatic p95 reporting from Criterion.rs. The p95 values are included in the default output and saved to the `target/criterion/` directory.

### Manual p95 Reporting

Two functions provide manual p95 calculation for immediate feedback:

1. **`bench_median_latency`** (lines 404-405, 411-419):
   - Calculates p95 from sorted latencies: `latencies[(latencies.len() * 95) / 100]`
   - Prints p95 to stderr during benchmark run

2. **`assertion_test`** (lines 360, 369):
   - Calculates p95 for assertion-style testing
   - Prints p95 alongside min, median, avg, and max

## Conclusion

✅ **Criterion.rs 0.5 confirmed with full p95 support**  
✅ **Current configuration is appropriate for p95 measurement**  
✅ **All benchmark functions receive automatic p95 reporting from Criterion**  
✅ **Additional manual p95 reporting available in specialized functions**

### Recommendations

1. **Current sample_size (100) is adequate** for development and CI purposes
2. **For production-grade p95 accuracy**, consider increasing `sample_size` to 500+
3. **No additional p95 integration needed** - all benchmarks already benefit from Criterion's automatic percentile reporting
