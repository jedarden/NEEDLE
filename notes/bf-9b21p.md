# Integration Test Results for bf-9b21p

## Summary
Ran all integration tests on 2026-07-14. **28 out of 163 tests failed** across 4 test files.

**Overall Success Rate: 85.3%**

## Test Suite Breakdown

### Passing Test Suites (14/18 files)
- **compilation_error_detection.rs**: 11/11 passed ✓
- **config_cli_tests.rs**: 10/10 passed ✓
- **heartbeat_validation.rs**: 3/3 passed ✓
- **workspace_fixtures.rs**: 8/8 passed ✓
- **p95_correctness.rs**: 7/7 passed ✓
- **needle_transform_claude.rs**: 4/4 passed ✓
- **otlp_integration.rs**: 0/0 passed (no tests) ✓
- **property_tests.rs**: 11/11 passed ✓
- **routing_integration.rs**: 21/21 passed ✓
- **routing_matcher_baseline.rs**: 7/7 passed ✓
- **sigterm_heartbeat_cleanup.rs**: 10/10 passed ✓
- **telemetry_field_verification.rs**: 0/0 passed (no tests) ✓
- **test_telemetry_write.rs**: 0/0 passed (no tests) ✓
- **test_telemetry_write_debug.rs**: 0/0 passed (no tests) ✓

### Failing Test Suites (4/18 files)
- **integration_tests.rs**: 11/27 passed, **16 failed** ✗
- **p2_integration_tests.rs**: 26/27 passed, **1 failed** ✗
- **p3_integration_tests.rs**: 18/22 passed, **4 failed** ✗
- **real_br_integration_tests.rs**: 14/21 passed, **7 failed** ✗

## Failed Tests by Category

### integration_tests.rs (16 failures)

**Category 1: Missing Adapter Configuration (10 tests)**
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

**Category 2: unwrap() on None Panics (4 tests)**
Tests that call `unwrap()` on `None` values:
1. `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` (line 1377)
2. `cross_workspace_mend_skips_beads_with_live_assignees` (line 1505)
3. `cross_workspace_mend_skips_own_worker_beads` (line 1623)
4. `mend_removes_stale_dependency_links` - `br create` failed

**Root Cause**: These tests expect bead or worker state to be present but it's not returned by the br CLI, likely due to missing bead store state or fixture setup issues.

**Category 3: Assertion Failure (1 test)**
1. `dead_worker_cleanup_integration` - Assertion `left == right` failed: both workers should be registered initially (left: 1, right: 2)

**Root Cause**: Worker registry not properly tracking all spawned workers in test fixtures.

**Category 4: String Slicing Issue (1 test)**
1. `exhaustion_with_idle_action_exit` - Panics at `src/transcript/mod.rs:278:31`: start byte index 2275 is not a char boundary; it is inside '─' (bytes 2273..2276)

**Root Cause**: UTF-8 character boundary violation in transcript module string slicing.

### p2_integration_tests.rs (1 failure)

**Strand Ordering Mismatch (1 test)**
1. `strand_waterfall_pluck_mend_explore_knot` - Expected `["pluck", "mend", "explore", "weave", "unravel", "pulse", "reflect", "knot"]` but got `["pluck", "mend", "explore", "weave", "unravel", "pulse", "reflect", "splice", "knot"]`

**Root Cause**: Strand waterfall now includes "splice" but test expectations don't account for it.

### p3_integration_tests.rs (4 failures)

**br CLI Interface Change (1 test)**
1. `unravel_creates_alternatives_without_modifying_original` - `br label add` missing required ID argument

**Scanner Integration Issues (2 tests)**
1. `pulse_deduplicates_across_scans` - assertion failed: expected `WorkCreated`
2. `pulse_detects_scanner_findings_and_creates_beads` - expected work creation, got `NoWork`
3. `weave_creates_beads_from_agent_response` - expected work creation, got `NoWork`

**Root Cause**: `br label add` command interface changed; scanner integration not producing expected work.

### real_br_integration_tests.rs (7 failures)

**br CLI Interface Change (5 tests)**
1. `real_br_mitosis_dedup_skips_existing_children` - `br label add` missing required ID argument
2. `real_br_mitosis_flock_serializes_concurrent_workers` - `br label add` missing required ID argument
3. `real_br_mitosis_precondition_checks` - `br label add` missing required ID argument
4. `real_br_strand_waterfall_exhaustion` - `br label add` missing required ID argument
5. `real_br_strand_waterfall_exhaustion_with_telemetry_jsonl` - `br label add` missing required ID argument

**Strand Ordering Mismatch (1 test)**
1. `real_br_strand_waterfall_ordering` - Same as p2 test (expects no "splice" strand)

**Database Recovery Failure (1 test)**
1. `real_br_database_corruption_auto_recovery` - `br sync --import-only` failed during rebuild

**Root Cause**: `br label add` command interface changed; strand ordering; database corruption recovery issue.

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
