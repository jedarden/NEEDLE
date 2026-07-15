# Integration Test Results for bf-9b21p

## Summary
Ran all integration tests on 2026-07-14. **16 out of 27 integration tests failed**.

## Test Suite Breakdown

### Passing Test Suites
- **compilation_error_detection.rs**: 11/11 passed ✓
- **config_cli_tests.rs**: 10/10 passed ✓  
- **heartbeat_validation.rs**: 3/3 passed ✓

### Failing Test Suite
- **integration_tests.rs**: 11/27 passed, **16 failed** ✗

## Failed Tests (16 total)

### Category 1: Missing Adapter Configuration (10 tests)
All these tests fail with the same error:
```
routed agent adapter 'claude-code-glm-4.7' not found — routing matched model 'unknown' with rule 'routing-default', but the adapter is missing from ~/.config/needle/adapters/claude-code-glm-4.7.yaml
```

Tests affected:
1. `end_to_end_single_bead_success`
2. `end_to_end_worker_loops_to_next_bead`
3. `exhaustion_with_idle_action_wait_survives_sleep`
4. `full_cycle_produces_telemetry_state_transitions`
5. `outcome_path_agent_not_found_exit_127`
6. `outcome_path_crash_exit_137`
7. `outcome_path_failure_exit_1`
8. `outcome_path_success_exit_0`
9. `outcome_path_timeout_exit_124`
10. `worker_processes_high_priority_beads_first`

**Root Cause**: Integration test fixtures are missing the required adapter configuration file for 'claude-code-glm-4.7'.

### Category 2: unwrap() on None Panics (4 tests)
Tests that call `unwrap()` on `None` values:
1. `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` (line 1377)
2. `cross_workspace_mend_skips_beads_with_live_assignees` (line 1505)
3. `cross_workspace_mend_skips_own_worker_beads` (line 1623)

**Root Cause**: These tests expect bead or worker state to be present but it's not returned by the br CLI, likely due to missing bead store state or fixture setup issues.

### Category 3: Assertion Failure (1 test)
1. `dead_worker_cleanup_integration` - Assertion `left == right` failed: both workers should be registered initially (left: 1, right: 2)

**Root Cause**: Worker registry not properly tracking all spawned workers in test fixtures.

### Category 4: String Slicing Issue (1 test)
1. `exhaustion_with_idle_action_exit` - Panics at `src/transcript/mod.rs:278:31`: start byte index 2275 is not a char boundary; it is inside '─' (bytes 2273..2276)

**Root Cause**: UTF-8 character boundary violation in transcript module string slicing.

## Additional Findings

### Compiler Warnings
- 19 warnings in `needle` library (mostly unreachable patterns in `cargo_test.rs`)
- 7 warnings in `routing_matcher_baseline` test (unused doc comments)

### Test Duration
- Total test run time: ~180 seconds
- Integration tests: 175.48 seconds (longest suite)

## Acceptance Criteria Status
❌ **FAILED** - Integration tests did not pass

**Blockers:**
1. Missing adapter configuration in test fixtures
2. Unimplemented test fixture logic for bead store operations
3. Worker registration tracking bug
4. UTF-8 string handling bug in transcript module

## Recommendations

### High Priority
1. Create test adapter configuration file for 'claude-code-glm-4.7' in test fixtures
2. Fix unwrap() calls - replace with proper error handling using `?` or `expect()` with context
3. Fix UTF-8 string slicing in transcript module (use `.char_indices()` for proper boundary detection)

### Medium Priority
4. Debug worker registration tracking in test fixtures
5. Clean up compiler warnings (unreachable patterns in cargo_test.rs)

## Next Steps
These failures indicate the integration test suite needs fixture setup work before it can pass reliably. The tests are probing functionality that requires proper test data and adapter configuration.
