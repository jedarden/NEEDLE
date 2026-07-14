# Bead bf-1gp34: Implement correct p95 calculation algorithm

## Summary

Verified and confirmed that the `calculate_p95` function correctly implements the p95 percentile calculation algorithm using linear interpolation.

## Implementation Details

The `calculate_p95` function in `src/stats/mod.rs` implements:

1. **Linear Interpolation Algorithm** (same as Criterion.rs):
   - Formula: `rank = 0.95 * (n - 1)`
   - Interpolates between floor and ceiling values
   - Rounds to nearest integer with epsilon for floating point precision

2. **Edge Case Handling**:
   - Empty slice: returns 0
   - Single element: returns that element
   - Multiple elements: linear interpolation provides smooth estimates

3. **Documentation**:
   - Comprehensive function documentation with algorithm explanation
   - Multiple examples showing expected behavior for various inputs
   - Edge cases clearly documented

## Tests

All tests pass (13 total):

### Unit Tests (5 tests in `src/stats/mod.rs`)
- `calculate_p95_empty`: Empty slice returns 0
- `calculate_p95_single_element`: Single element returns that value
- `calculate_p95_sorted`: 10 elements → 96
- `calculate_p95_unsorted`: Unsorted input produces same result
- `calculate_p95_twenty_elements`: 20 elements → 191

### Integration Tests (7 tests in `tests/p95_correctness.rs`)
- `test_p95_known_values`: Known values verify algorithm
- `test_p95_edge_cases`: Empty, single, two, three elements
- `test_p95_duplicate_values`: All same values, many duplicates
- `test_p95_unsorted_input`: Unsorted produces same result as sorted
- `test_p95_large_dataset`: 1000 elements scales correctly
- `test_p95_realistic_latency_data`: Real-world latency distribution
- `test_p95_with_outliers`: Data with outliers

### Integration Test (1 test in `src/sanitize/mod.rs`)
- `sanitizer_performance_100kb_median`: Uses `calculate_p95` for percentile calculation

## Commit

Work was committed in `65a1328`:
```
fix(needle-bf-1gp34): use calculate_p95 function in sanitize test
```

This commit updated the `sanitizer_performance_100kb_median` test to use the proper `calculate_p95` function instead of a simple nearest-rank method, ensuring consistency with the correct linear interpolation algorithm.

## Acceptance Criteria

✅ Function implements correct p95 algorithm (linear interpolation)
✅ Edge cases handled properly (empty, single, small samples)
✅ Unit tests pass with known test vectors (13 tests, all pass)
✅ Algorithm documented in code comments (comprehensive documentation)
