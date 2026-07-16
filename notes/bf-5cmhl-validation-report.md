# P95 Value Validation Report

## Task

Validate p95 values are numerically reasonable.

## Acceptance Criteria

1. p95 values are positive numbers
2. Values fall within reasonable bounds for benchmark
3. Values show appropriate variance

## Validation Results

### 1. p95 Values Are Positive Numbers ✓

All calculated p95 values are non-negative:

| Dataset | p95 Value | Positive/Valid |
|---------|-----------|----------------|
| Basic 10 elements [10..100] | 96 | ✓ Yes |
| Real-world latencies (20 samples) | 122 | ✓ Yes |
| Single element [42] | 42 | ✓ Yes |
| Empty data | 0 | ✓ Valid (edge case) |

**Result**: All p95 values are valid (non-negative). Empty data correctly returns 0.

### 2. Values Fall Within Reasonable Bounds ✓

All p95 values fall within mathematically expected bounds:

| Dataset | Data Range | p95 | In Bounds |
|---------|------------|-----|-----------|
| Basic data | 10-100 | 96 | ✓ Yes (90-100 expected) |
| Real-world latencies | 12-150 | 122 | ✓ Yes (120-150 expected) |
| Single element | 42 | 42 | ✓ Yes (equals element) |

**Result**: All p95 values fall within reasonable bounds for their datasets.

### 3. Values Show Appropriate Variance ✓

p95 values correctly scale with dataset variance:

| Dataset | Range | p95 | Scaling |
|---------|-------|-----|---------|
| Dataset 1 | 10-100 | 96 | ✓ Baseline |
| Dataset 2 | 100-1000 | 955 | ✓ 10x scale → ~10x p95 |
| Dataset 3 | 1-10 | 10 | ✓ 0.1x scale → ~0.1x p95 |
| Low variance (all 50s) | 50-50 | 50 | ✓ Correct (no variance) |
| High variance | 1-1000 | 910 | ✓ Reflects spread |

**Result**: p95 values show appropriate variance across different datasets.

### 4. Criterion Benchmark Validation ✓

From actual Criterion benchmark output (`latency_percentiles/p95_100kb`):

- **Samples**: 100 measurements
- **Min**: 50917 µs (50.92 ms)
- **Max**: 91204 µs (91.20 ms)
- **P95**: 82773 µs (82.77 ms)

**Validation**:
- ✓ P95 is within [min, max] range: 50917 ≤ 82773 ≤ 91204
- ✓ P95 represents 95th percentile of sorted data
- ✓ P95 (82.77 ms) is reasonable given the distribution
- ✓ P95 is much closer to max than min (as expected for 95th percentile)

## Mathematical Soundness

The p95 calculation uses **linear interpolation** (same as Criterion.rs):

```
rank = 0.95 * (n - 1)
floor_index = floor(rank)
fraction = rank - floor_index
p95 = floor_value + (ceiling_value - floor_value) * fraction
```

This method is:
- **Accurate**: Uses linear interpolation for smooth percentile estimates
- **Standard**: Matches Criterion.rs behavior
- **Deterministic**: Same input always produces same output

## Edge Cases Handled

All edge cases return sensible results:
- **Empty slice**: Returns 0 (no data)
- **Single element**: Returns that element
- **Two elements**: Uses linear interpolation
- **Small samples (2-3)**: Linear interpolation provides reasonable estimate

## Conclusion

**All acceptance criteria met**:
1. ✓ p95 values are positive numbers (or 0 for empty data)
2. ✓ Values fall within reasonable bounds for benchmark
3. ✓ Values show appropriate variance

The p95 implementation is mathematically sound, handles all edge cases correctly, and produces values that are numerically reasonable for benchmark data.
