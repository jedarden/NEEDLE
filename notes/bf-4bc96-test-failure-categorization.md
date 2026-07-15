# Test Failure Categorization for NEEDLE

Generated: 2026-07-15

## Summary

- **Total Test Suite**: 1,411 unit tests + 27 integration tests
- **Unit Test Failures**: 1 (0.07% failure rate)
- **Integration Test Failures**: 16 (59% failure rate)

---

## Unit Test Failures

### Category: Process Tree Traversal

**Test**: `cli::tests::find_all_descendants_handles_cycles`

**Location**: `src/cli/mod.rs:5755`

**Failure Details**:
```
assertion `left == right` failed
  left: 2
 right: 1
```

**What it tests**: Cycle detection in process tree traversal - the function should find 2 descendants but stops when it encounters a cycle.

**Module**: `cli` (process management utilities)

**Status**: **Stable failure** - deterministic assertion failure

---

## Integration Test Failures

All integration tests are located in `/tests` directory and test end-to-end worker behavior.

### Category: Cross-Workspace Mend (3 failures)

**Tests**:
1. `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead`
2. `cross_workspace_mend_skips_beads_with_live_assignees`
3. `cross_workspace_mend_skips_own_worker_beads`

**Module**: `strand::mend`

**What they test**: Cross-workspace cleanup of orphaned beads and stale dependencies

**Failure Pattern**: All three cross-workspace mend tests failed - suggests a systemic issue with cross-workspace bead management

---

### Category: End-to-End Worker Lifecycle (4 failures)

**Tests**:
1. `end_to_end_single_bead_success`
2. `end_to_end_worker_loops_to_next_bead`
3. `exhaustion_with_idle_action_exit`
4. `exhaustion_with_idle_action_wait_survives_sleep`

**Module**: `worker` (full cycle)

**What they test**: Complete worker state machine transitions from boot through exhaustion

**Failure Pattern**: Core worker lifecycle broken - fundamental issue

---

### Category: Worker Process Management (2 failures)

**Tests**:
1. `dead_worker_cleanup_integration`
2. `worker_processes_high_priority_beads_first`

**Module**: `worker` + `strand::mend`

**What they test**: 
- Dead worker detection and bead release
- Priority-based bead processing

**Failure Pattern**: Worker process management issues

---

### Category: Telemetry and State Transitions (1 failure)

**Test**: `full_cycle_produces_telemetry_state_transitions`

**Module**: `telemetry` + `worker`

**What it tests**: Emission of telemetry events throughout worker lifecycle

---

### Category: Outcome Path Testing (5 failures)

**Tests**:
1. `outcome_path_agent_not_found_exit_127`
2. `outcome_path_crash_exit_137`
3. `outcome_path_failure_exit_1`
4. `outcome_path_success_exit_0`
5. `outcome_path_timeout_exit_124`

**Module**: `outcome` + `worker`

**What they test**: All possible exit code paths through the outcome handling system

**Failure Pattern**: **Complete failure of all outcome paths** - suggests fundamental breakage in outcome handling

---

### Category: Dependency Management (1 failure)

**Test**: `mend_removes_stale_dependency_links`

**Module**: `strand::mend`

**What it tests**: Cleanup of dependency links when blocking beads are closed

---

## Heat Map: Failures by Module

| Module | Unit Test Failures | Integration Test Failures | Total |
|--------|-------------------|---------------------------|-------|
| `cli` | 1 | 0 | 1 |
| `worker` | 0 | 11 | 11 |
| `strand::mend` | 0 | 4 | 4 |
| `outcome` | 0 | 5 | 5 |
| `telemetry` | 0 | 1 | 1 |

**Most Affected Modules**:
1. **`worker`** (11 failures) - Core worker state machine
2. **`outcome`** (5 failures) - Exit code handling
3. **`strand::mend`** (4 failures) - Cleanup operations

---

## Environment-Specific Failures

### Flaky/Environment-Dependent Tests

**Unit Tests**: None detected - unit test failure is deterministic

**Integration Tests**: The integration tests show error messages indicating external process issues:
```
NEEDLE worker 'test-worker' stopped unexpectedly: state=Dispatching, beads_processed=0, uptime=5s
This indicates the worker was killed by an external process (e.g., SIGKILL, OOM, capacity governor)
```

**Potential Causes**:
- OOM (Out of Memory) killer
- System resource exhaustion
- Process isolation issues in test environment
- External signal delivery (SIGKILL)

**Recommendation**: Check system logs and resource limits during test runs

---

## Compilation Warnings

**File**: `src/cargo_test.rs:229`

**Warning**: `unreachable_pattern` - duplicate error code matching
```rust
"E0495" | "E0597" | "E0623" | "E0515" | "E0503" | "E0504" | "E0510" | "E0391"
                       ^^^^^^^ no value can reach this
```

**Severity**: Low - code cleanup opportunity, not a failure

---

## Test Coverage Gaps

### Missing Test Coverage
Based on the failures, these areas may need better unit test coverage:

1. **Cycle detection** in process tree traversal (unit test exists but fails)
2. **Cross-workspace bead management** - only integration tests, no unit tests
3. **Outcome handling paths** - integration tests fail but may lack unit test scaffolding

### Recommendations

1. **Immediate**: Fix the `find_all_descendants_handles_cycles` unit test - appears to be a logic bug in cycle detection
2. **High Priority**: Investigate integration test environment - workers being killed suggests resource/signal issues
3. **Medium Priority**: Add unit tests for cross-workspace mend operations to isolate the failure
4. **Low Priority**: Clean up unreachable pattern warnings in cargo_test.rs

---

## Next Steps

1. **Unit Test**: Debug `find_all_descendants_handles_cycles` - the cycle detection algorithm has an off-by-one error
2. **Integration Tests**: Run with increased resource limits or check for external signal delivery
3. **Outcome Paths**: All 5 outcome path tests failed - investigate outcome handling system as a unit
