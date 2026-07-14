# Bead bf-ekbhe: P95 Calculation Helper Function

## Finding

The `calculate_p95` helper function is **already implemented** in `src/stats/mod.rs` (lines 385-394).

## Implementation Details

**Function signature:** `pub fn calculate_p95(latencies: &[u128]) -> u128`

**Location:** `/home/coding/NEEDLE/src/stats/mod.rs`

**Algorithm:** Uses the nearest-rank method:
1. Returns 0 for empty input (graceful handling)
2. Sorts the input slice internally
3. Calculates index: `(len * 95) / 100`
4. Returns value at that index

**Acceptance criteria status:**
- ✅ Helper function exists and is exported
- ✅ Function signature matches: `calculate_p95(latencies: &[u128]) -> u128`
- ✅ Uses correct p95 algorithm (nearest-rank method)
- ✅ Handles empty input gracefully (returns 0)

## Tests

All 5 test cases pass:
- `calculate_p95_empty` - empty input returns 0
- `calculate_p95_single_element` - single element returns that value
- `calculate_p95_sorted` - sorted array
- `calculate_p95_unsorted` - unsorted array (sorts internally)
- `calculate_p95_twenty_elements` - larger dataset

## Verification

```bash
cargo test calculate_p95 --lib
# test result: ok. 5 passed; 0 failed; 0 ignored
```

The implementation is complete, well-documented, and fully tested.
