# Bead bf-4hq: Concurrent Claim Exclusivity Property Test

## Task

Add real concurrent claim exclusivity property test using actual flock.

## Finding

The test already exists and is fully implemented in `tests/real_br_integration_tests.rs` (lines 1198-1307).

## Implementation Details

The test `test_concurrent_claim_exclusivity(num_workers: u32)`:

1. Creates a single bead in a real `br` workspace via `create_test_workspace()` and `create_bead()`
2. Spawns N Tokio tasks, each running through `Claimer::claim_next()` path
3. Asserts exactly 1 task receives `ClaimOutcome::Claimed`, the rest receive `ClaimOutcome::AllRaceLost` or `ClaimOutcome::NoCandidates`
4. Is parameterized over N via the `num_workers` parameter
5. Uses real flock via `fs2` (Claimer internally uses `fs2::FileExt` for flock serialization)

## Test Coverage

Three tests exist for different worker counts:
- `real_br_property_3_concurrent_claim_exclusivity_n2` (2 workers)
- `real_br_property_3_concurrent_claim_exclusivity_n5` (5 workers)
- `real_br_property_3_concurrent_claim_exclusivity_n20` (20 workers)

All tests consistently pass under `cargo test --features integration`.

## Acceptance Criteria Verification

- [x] Test passes consistently under `cargo test --features integration` with real `br` binary
- [x] N=2, N=5, N=20 all assert exactly 1 success
- [x] Test is documented with 'Property 3 (true concurrent)' comment (line 1199)

## Verification Timestamp

- 2026-05-30: Re-verified all 3 tests pass under `cargo test --features integration`

## Conclusion

No implementation work was needed. The concurrent claim exclusivity property test was already implemented and passing. This bead is ready to be closed.
