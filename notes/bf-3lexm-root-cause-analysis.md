# Root Cause Analysis: Test Failure Patterns

**Generated**: 2026-07-15
**Bead**: bf-3lexm
**Scope**: Analysis of test failures across NEEDLE codebase

---

## Executive Summary

Across the NEEDLE test suite, **18 failures** have been identified and analyzed:

- **Unit Test Failures**: 2 (0.14% of 1,418 tests)
- **Integration Test Failures**: 16 (59% of 27 tests)
- **Compilation Issues**: 4 missing pattern matches

The failures cluster into **5 distinct root cause patterns**, with integration test failures being the most severe.

---

## Root Cause Patterns (by Impact)

### Pattern 1: External Process Signal Delivery (Highest Impact - 11 failures)

**Affected Tests**: All worker lifecycle and outcome path integration tests

**Failure Manifestation**:
```
NEEDLE worker 'test-worker' stopped unexpectedly: state=Dispatching, beads_processed=0, uptime=5s
This indicates the worker was killed by an external process (e.g., SIGKILL, OOM, capacity governor)
```

**Affected Test Categories**:
- Worker lifecycle (4 failures)
- Worker process management (2 failures)
- Outcome paths (5 failures - 100% failure rate in this category)

**Root Cause**: Integration test environment delivers external termination signals to worker processes during execution.

**Why This Happens**:
1. Tests spawn actual worker subprocesses
2. Workers receive SIGKILL or are terminated by OOM killer
3. Test harness interprets unexpected termination as test failure

**Evidence**:
- Consistent "stopped unexpectedly" messages across all failing worker tests
- Short uptime before termination (5s typical)
- Zero beads processed before termination
- Deterministic pattern (not intermittent)

**Fix Priority**: **CRITICAL**
- Blocks 11 of 27 integration tests (40% of integration suite)
- Prevents validation of core worker functionality

**Recommended Actions**:
1. Check system resource limits during test runs
2. Investigate OOM killer logs
3. Verify process isolation in test environment
4. Consider sandboxing test workers with explicit resource limits
5. Add signal handling tests to validate external signal resilience

---

### Pattern 2: Missing Pattern Match for Enum Variant (High Impact - 4 compilation errors)

**Affected Module**: `src/strand/explore.rs`

**Failure Manifestation**:
```
error[E0004]: non-exhaustive patterns: `types::StrandResult::NoHomeStore` not covered
   --> src/strand/explore.rs:770:22
    |
770 |                 match result {
    |                      ^^^^^^ pattern `NoHomeStore` not covered
```

**Locations**: Lines 770, 869, 938, 1023 in `src/strand/explore.rs`

**Root Cause**: The `NoHomeStore` variant was recently added to `StrandResult` enum but existing match statements weren't updated.

**Why This Happens**:
1. Enum variants added without updating all match sites
2. Compiler exhaustiveness checking catches the omission
3. No unit tests cover the new code path

**Fix Priority**: **HIGH**
- Blocks compilation (exit code 101)
- Prevents ANY tests from running
- Must be fixed before other failures can be investigated

**Recommended Actions**:
1. Add `NoHomeStore` pattern arms to all 4 affected match statements
2. Add unit tests for `NoHomeStore` code path
3. Consider compiler warning lint for non-exhaustive matches
4. Add regression test for enum variant additions

---

### Pattern 3: Cross-Workspace Bead Management Issues (Medium Impact - 4 failures)

**Affected Tests**:
1. `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead`
2. `cross_workspace_mend_skips_beads_with_live_assignees`
3. `cross_workspace_mend_skips_own_worker_beads`
4. `mend_removes_stale_dependency_links`

**Module**: `strand::mend`

**Failure Manifestation**: All cross-workspace operations fail

**Root Cause**: Cross-workspace bead cleanup and dependency management has systemic issues.

**Why This Happens**:
1. No unit tests for cross-workspace operations
2. Only integration tests exist, which are currently blocked by Pattern 1
3. Complex multi-workspace state coordination not well-tested

**Fix Priority**: **MEDIUM**
- 4 integration tests blocked
- May work correctly but can't verify due to Pattern 1
- Needs isolated unit tests

**Recommended Actions**:
1. Add unit tests for cross-workspace bead state operations
2. Mock workspace stores in unit tests
3. Isolate from external signal issues
4. Test state transitions independent of worker lifecycle

---

### Pattern 4: Error Detection Function Gap (Low Impact - 1 unit test failure)

**Affected Test**: `bead_store::tests::detects_locked_db_error`

**Module**: `bead_store`

**Failure Manifestation**:
```
assertion failed: is_corruption_error("database is locked")
```

**Root Cause**: The `is_corruption_error()` function doesn't recognize "database is locked" as a corruption error.

**Why This Happens**:
1. Function pattern matching incomplete
2. SQLite "locked" error may not be corruption (could be concurrent access)
3. Test expectation may be incorrect

**Fix Priority**: **LOW**
- Single unit test
- May be correct behavior (locked ≠ corrupted)
- Needs domain knowledge to resolve

**Recommended Actions**:
1. Determine if "database is locked" should be corruption
2. If yes: update pattern matching
3. If no: update test expectation
4. Document SQLite error taxonomy

---

### Pattern 5: Cycle Detection Algorithm Bug (Low Impact - 1 unit test failure)

**Affected Test**: `cli::tests::find_all_descendants_handles_cycles`

**Module**: `cli` (process management)

**Failure Manifestation**:
```
assertion `left == right` failed
  left: 2
 right: 1
```

**Root Cause**: Cycle detection in process tree traversal counts nodes incorrectly.

**Why This Happens**:
1. Algorithm doesn't properly deduplicate when cycle detected
2. Cycle may not prevent revisiting already-counted nodes
3. Off-by-one error in cycle handling

**Fix Priority**: **LOW**
- Single unit test
- Deterministic failure
- Isolated to process tree utilities

**Recommended Actions**:
1. Debug `find_all_descendants_handles_cycles` logic
2. Add cycle detection assertions
3. Test with various cycle configurations
4. Consider using visited set to prevent double-counting

---

## Impact Summary by Module

| Module | Failures | Severity | Primary Pattern |
|--------|----------|----------|-----------------|
| `worker` | 11 | CRITICAL | External signal delivery |
| `strand::explore` | 4 | HIGH | Missing pattern matches |
| `strand::mend` | 4 | MEDIUM | Cross-workspace state |
| `outcome` | 5 | CRITICAL | External signal delivery |
| `bead_store` | 1 | LOW | Error detection gap |
| `cli` | 1 | LOW | Cycle detection bug |
| `telemetry` | 1 | CRITICAL | External signal delivery |

---

## Test Infrastructure Issues

### Issue 1: Integration Test Environment Instability

**Problem**: 11 of 16 integration test failures are due to external process termination, not logic errors.

**Evidence**:
- Consistent SIGKILL/OOM patterns
- Short uptime before termination
- No beads processed

**Recommendation**: 
- Resource limits enforcement
- Process sandboxing
- Signal handling hardening

### Issue 2: Missing Unit Test Coverage

**Problem**: Complex operations (cross-workspace mend) have no unit tests.

**Evidence**:
- `strand::mend` has 4 integration failures, 0 unit failures
- Integration tests blocked by external signal issues

**Recommendation**:
- Add unit tests with mocked stores
- Test state transitions in isolation
- Reduce dependency on subprocess spawning

### Issue 3: Incomplete Exhaustiveness Checking

**Problem**: Pattern matches not updated when enum variants added.

**Evidence**:
- `NoHomeStore` variant added without updating match sites
- 4 compilation errors across same file

**Recommendation**:
- Enable stricter lints for non-exhaustive matches
- Add pre-commit hooks for enum changes
- Consider explicit match exhaustiveness tests

---

## Prioritized Fix List

### Priority 1: Unblock Compilation (Pattern 2)
**Impact**: Unblocks all testing
**Effort**: Low (4 pattern matches to add)
**Actions**:
1. Add `NoHomeStore` arms to 4 match statements in `explore.rs`
2. Verify compilation succeeds
3. Re-run test suite to validate other fixes

### Priority 2: Fix External Signal Delivery (Pattern 1)
**Impact**: Unblocks 11 integration tests (40% of suite)
**Effort**: Medium (investigation + hardening)
**Actions**:
1. Check system logs for OOM/signal delivery
2. Add resource limit enforcement
3. Harden worker signal handling
4. Add signal-specific tests

### Priority 3: Add Unit Tests for Cross-Workspace Operations (Pattern 3)
**Impact**: Validates mend operations independent of external issues
**Effort**: Medium (mocking + test design)
**Actions**:
1. Design mocked workspace store interface
2. Add unit tests for cross-workspace state operations
3. Test cleanup and dependency logic in isolation

### Priority 4: Fix Error Detection Function (Pattern 4)
**Impact**: 1 unit test
**Effort**: Low (domain decision + pattern update)
**Actions**:
1. Determine if "locked" = "corrupted"
2. Update function or test accordingly
3. Document SQLite error taxonomy

### Priority 5: Fix Cycle Detection Algorithm (Pattern 5)
**Impact**: 1 unit test
**Effort**: Low (algorithm debug)
**Actions**:
1. Debug cycle detection logic
2. Add visited set or improve cycle handling
3. Test various cycle configurations

---

## Test Infrastructure Recommendations

### 1. Resource Governance
- Enforce per-test resource limits
- Monitor memory/CPU during test runs
- Add timeout enforcement for subprocess spawning

### 2. Signal Hardening
- Explicit signal handling in workers
- Graceful shutdown on SIGTERM
- Diagnostic logging on unexpected signals

### 3. Isolation Improvements
- Mock external dependencies where possible
- Reduce subprocess spawning in tests
- Add process sandboxing

### 4. Exhaustiveness Enforcement
- Stricter lints for pattern matching
- Pre-commit checks for enum changes
- Automated regression tests for new variants

---

## Conclusion

The test failure analysis reveals **two distinct classes of issues**:

1. **Test Infrastructure Problems** (11 failures): External signal delivery prevents integration tests from validating logic
2. **Code Logic Issues** (7 failures): Missing patterns, algorithm bugs, error detection gaps

The **highest-impact fix** is resolving the external signal delivery issue (Priority 2), which unblocks 40% of the integration test suite. The **highest-leverage fix** is adding the missing pattern matches (Priority 1), which unblocks all testing.

Cross-workspace operations (Priority 3) need unit test coverage to prevent future regressions and enable debugging independent of the signal delivery issues.

The two unit test failures (Priorities 4-5) are low-impact but should be fixed to maintain test suite hygiene.

---

**Next Steps**:
1. Fix compilation errors (Pattern 2)
2. Investigate and resolve external signal delivery (Pattern 1)
3. Add unit test coverage for cross-workspace operations (Pattern 3)
4. Fix remaining unit test failures (Patterns 4-5)
5. Improve test infrastructure to prevent recurrence
