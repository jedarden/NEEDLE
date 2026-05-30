# bead bf-4hq: Real Concurrent Claim Exclusivity Property Test

## Finding

The tests requested in this bead already exist in `tests/real_br_integration_tests.rs` (lines 1205-1313).

## Test Implementation

- **Test helper**: `test_concurrent_claim_exclusivity(num_workers: u32)` (lines 1216-1298)
- **Test cases**:
  - `real_br_property_3_concurrent_claim_exclusivity_n2` (N=2)
  - `real_br_property_3_concurrent_claim_exclusivity_n5` (N=5)
  - `real_br_property_3_concurrent_claim_exclusivity_n20` (N=20)

## Verification

```bash
cargo test --features integration --test real_br_integration_tests real_br_property_3_concurrent_claim_exclusivity
```

Result: All 3 tests pass consistently.

## Acceptance Criteria Met

- ✅ Test passes consistently under `cargo test --features integration` with real `br` binary
- ✅ N=2, N=5, N=20 all assert exactly 1 success
- ✅ Test is documented with 'Property 3 (true concurrent)' comment

## Implementation Details

The test:
1. Creates a single bead in a real `br` workspace
2. Spawns N Tokio tasks, each running through `Claimer::claim_next()`
3. Asserts exactly 1 task receives `ClaimOutcome::Claimed`, others receive `ClaimOutcome::AllRaceLost` or `ClaimOutcome::NoCandidates`
4. Uses real flock via `fs2` (lock_dir shared by all workers)

## Note

The bead predates the test implementation. The tests were added after this bead was created, fulfilling the original requirement.
