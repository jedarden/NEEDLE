# Exit Code 101 Analysis - needle-ci Failure 2026-08-29

**Date**: 2026-08-29  
**Workflow**: needle-ci (Definition of Done - all lanes)  
**Exit Code**: 101 (Test Panic)  
**Cluster**: iad-ci (argo-workflows namespace)  
**Log File**: [`docs/needle-ci-failure-2026-08-29.log`](needle-ci-failure-2026-08-29.log)

## Executive Summary

Exit code 101 indicates Rust test panics (assertion failures, unwrap/expect failures, or explicit panics). The 2026-08-29 needle-ci run revealed **19 distinct test failures** across 4 integration test suites, all stemming from three core issues:

1. **Bead-rs capability mismatch** (status 'blocked' not recognized)
2. **Tilde expansion logic errors** in worker binary path handling  
3. **Bead reopen behavior** not clearing assignees as expected
4. **Explore strand** failing to discover work in other workspaces

## Failure Breakdown by Test Suite

### 1. integration_tests (9 failed tests)

**Exit Code**: 101  
**Suite**: `cargo test --test integration_tests`  
**Failed Tests**:
- `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead`
- `dead_worker_cleanup_integration`
- `doctor_exit_code_on_failure_and_success`
- `subprocess_adapter_failure_exits_nonzero`
- `subprocess_nonexistent_adapter_produces_actionable_error_message`
- `worker_binary_path_supervisor_initialization`
- `worker_binary_path_tilde_expansion_multiple_tildes`
- `worker_binary_path_tilde_expansion_position`
- `worker_boot_rejects_nonexistent_adapter`

**Root Causes**:

#### A. Bead-rs Capability Mismatch (5 tests affected)
```
bead-rs capability mismatch for workspace: unexpected status 'blocked' in capabilities 
(only open, in_progress, deferred, and closed are valid stored statuses)
```

Affected tests expecting 'blocked' status to be valid:
- `subprocess_nonexistent_adapter_produces_actionable_error_message`
- `worker_boot_rejects_nonexistent_adapter`
- `worker_binary_path_supervisor_initialization`
- `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead`
- `dead_worker_cleanup_integration`

#### B. Tilde Expansion Logic Errors (2 tests)
```rust
// worker_binary_path_tilde_expansion_multiple_tildes
assertion failed: `left == right` 
  left: "~ ~"
 right: "/tmp/.tmpSu5Flm/fake-home ~"

// worker_binary_path_tilde_expansion_position  
assertion failed: `left == right`
  left: "/path/~/nested"
 right: "/path/~"
```

Expected behavior: First `~` expands to `$HOME`, subsequent tildes preserved. Actual: Tilde expansion not working correctly in path construction.

### 2. p2_integration_tests (1 failed test)

**Exit Code**: 101  
**Suite**: `cargo test --test p2_integration_tests`  
**Failed Test**:
- `explore_discovers_work_in_other_workspace`

**Error**:
```rust
thread 'explore_discovers_work_in_other_workspace' panicked at tests/p2_integration_tests.rs:1567:18:
explore should discover the remote bead; got: NoWork
```

**Root Cause**: The Explore strand is not successfully discovering beads in other workspaces, despite test setup creating remote beads.

### 3. p3_integration_tests (1 failed test)

**Exit Code**: 101  
**Suite**: `cargo test --test p3_integration_tests`  
**Failed Test**:
- `splice_documents_stale_worker_and_deduplicates_session`

**Error**:
```rust
thread 'splice_documents_stale_worker_and_deduplicates_session' panicked at tests/p3_integration_tests.rs:679:5:
splice should create escalation work for a stale worker, got NoWork
```

**Root Cause**: The Splice strand is not detecting stale workers and creating escalation beads as expected.

### 4. real_br_integration_tests (8 failed tests)

**Exit Code**: 101  
**Suite**: `cargo test --test real_br_integration_tests`  
**Failed Tests**:
- `real_bead_rs_explore_discovers_remote_workspace`
- `real_bead_rs_regression_silent_starvation_bug_fixed`
- `real_bead_rs_reopen_allows_any_worker_to_claim`
- `real_bead_rs_reopen_appears_in_ready_frontier`
- `real_bead_rs_reopen_clears_assignee`
- `real_bead_rs_reopen_with_dependencies`
- `real_bead_rs_strand_waterfall_exhaustion`
- `real_bead_rs_strand_waterfall_exhaustion_with_telemetry_jsonl`

**Root Causes**:

#### A. Explore Discovery Failure
```rust
thread 'real_bead_rs_explore_discovers_remote_workspace' panicked:
Explore should find bead in remote workspace, got NoWork
```

Same issue as p2_integration_tests - Explore strand not finding remote work.

#### B. Bead Reopen Behavior (5 tests)
```rust
// real_bead_rs_reopen_clears_assignee
reopened bead MUST have no assignee (ADR-018); got assignee=Some("test-worker")

// real_bead_rs_regression_silent_starvation_bug_fixed
BUG FIX: reopened bead must appear in ready frontier (was silently starving before ADR-018)

// real_bead_rs_reopen_appears_in_ready_frontier
reopened bead MUST appear in ready frontier (assignee was cleared)

// real_bead_rs_reopen_allows_any_worker_to_claim
reopened bead should have no assignee

// real_bead_rs_reopen_with_dependencies
reopened parent should have no assignee
```

**Critical Issue**: `bead reopen` is **not clearing the assignee** despite ADR-018 requirements. This causes silent starvation where reopened beads become permanently unclaimable.

#### C. Waterfall Exhaustion Logic (2 tests)
```rust
// real_bead_rs_strand_waterfall_exhaustion
waterfall should return None when all strands return NoWork; 
got Some((Bead { id: "test-73f31c8f", title: "test-deferred-task", 
status: Open, assignee: None, labels: ["deferred"], ... }, "pluck"))
```

Expected behavior: Waterfall returns `None` when exhausted. Actual: Returns deferred beads even when they shouldn't be plucked.

## Additional Failure: cargo test --lib

**Exit Code**: 124 (timeout)  
**Test**: `cargo test --lib`

```
Running: cargo test --lib...
✗ cargo test --lib failed (exit code: 124)
```

The lib test suite hit a timeout, likely due to long-running tests that exceeded CI limits. Notable in the output:
```
test commit_hook::tests::validate_commit_returns_ok_when_no_snapshot has been running for over 60 seconds
test commit_hook::tests::validate_commit_skips_beads_and_predispatch_sha has been running for over 60 seconds
test config::config_tests::test_isolate_bead_cli_env_multiple_calls_create_different_dirs has been running for over 60 seconds
```

## Installer Test Failure

**Exit Code**: 1  
**Suite**: installer tests  
**Failed Tests**: 2 of 20

Missing installer help text:
- Expected "SECURITY NOTE" in help output
- Expected "NOT RECOMMENDED" warning text

## Root Cause Analysis

### 1. Capability Negotiation Mismatch

The `bead capabilities --profile native-v1` command now returns a 'blocked' status, but NEEDLE's capability check only accepts: `open`, `in_progress`, `deferred`, `closed`.

**Location**: Likely in `src/bead_store/` or `src/types/` - capability validation logic

**Fix Required**: Update NEEDLE's capability negotiation to either:
- Accept 'blocked' as a valid status from bead-rs
- Filter out 'blocked' from the capabilities list before validation

### 2. Tilde Expansion Implementation Bug

The tilde expansion logic in `src/config/mod.rs` is not correctly handling:
- Multiple tildes in a path (should expand first, preserve rest)
- Tilde position (should only expand leading `~`, not mid-path tildes)

**Location**: `src/config/mod.rs` - tilde expansion functions

**Current Behavior**: Appears to truncate paths incorrectly instead of expanding only the first tilde.

### 3. Bead Reopen Not Clearing Assignee

**Critical**: This violates ADR-018 and causes silent starvation.

The `bead reopen` command (via bead-rs CLI) is not clearing the assignee field when transitioning a bead from `closed` to `open` status.

**Impact**: Reopened beads retain their previous assignee, making them invisible to the ready frontier and permanently unclaimable by other workers.

**Location**: Integration with bead-rs CLI - likely in outcome handling or reopen verification

### 4. Explore Strand Discovery Failure

The Explore strand is not successfully discovering beads in other workspaces during tests, despite proper test setup.

**Potential Causes**:
- Workspace scanning logic broken
- Path resolution issues with temporary test directories
- Bead store initialization timing issues

### 5. Waterfall Exhaustion Logic

The strand waterfall is returning deferred beads when it should return `None`, indicating the exhaustion logic doesn't properly handle the `deferred` status.

**Location**: `src/worker/` or strand orchestration logic

## Test Statistics

| Suite | Total Tests | Passed | Failed | Exit Code |
|-------|-------------|--------|--------|-----------|
| integration_tests | 81 | 72 | 9 | 101 |
| p2_integration_tests | 27 | 26 | 1 | 101 |
| p3_integration_tests | 25 | 24 | 1 | 101 |
| real_br_integration_tests | 31 | 23 | 8 | 101 |
| **Total** | **164** | **145** | **19** | **101** |

**Pass Rate**: 88.4% (145/164)

## Impact Assessment

### High Impact (Blocks Release)
- **Bead reopen not clearing assignees**: Violates ADR-018, causes silent starvation in production fleets
- **Capability negotiation mismatch**: Blocks workspace initialization with current bead-rs versions

### Medium Impact (Degraded Functionality)
- **Explore strand discovery**: Reduces multi-worker fleet efficiency
- **Waterfall exhaustion logic**: May cause incorrect work plucking behavior

### Low Impact (Test-Only Issues)
- **Tilde expansion bugs**: Affects tests using artificial paths, unlikely in production
- **Installer help text**: Documentation-only issue

## Related Files

### Log Files
- **Primary**: [`docs/needle-ci-failure-2026-08-29.log`](needle-ci-failure-2026-08-29.log) (718 lines, complete CI output)
- **Previous Investigation**: [`docs/needle-ci-failure-investigation-2026-08-16.md`](needle-ci-failure-investigation-2026-08-16.md)

### Test Files
- `tests/integration_tests.rs` - 9 failures
- `tests/p2_integration_tests.rs` - 1 failure  
- `tests/p3_integration_tests.rs` - 1 failure
- `tests/real_br_integration_tests.rs` - 8 failures
- `tests/doctor_exit_code_tests.rs` - Exit code validation (passing)

### Source Files (Likely Locations)
- `src/config/mod.rs` - Tilde expansion logic
- `src/bead_store/mod.rs` - Capability negotiation
- `src/worker/mod.rs` - Strand orchestration, waterfall logic
- `src/outcome/mod.rs` - Reopen handling

### Documentation
- `docs/adr/018-reopen-clears-assignee.md` (should exist based on ADR-018 references)
- `docs/capabilities-negotiation.md` - Capability contract specification

## Next Steps for Investigation Bead

1. **Verify bead-rs version and capabilities output**:
   ```bash
   bead --version
   bead capabilities --profile native-v1
   ```

2. **Test reopen behavior locally**:
   ```bash
   bead create --title "test-reopen" --priority 0 --issue-type task
   bead claim <id>
   bead close <id> --reason "test"
   bead reopen <id>
   bead show <id>  # Check if assignee is cleared
   ```

3. **Check tilde expansion implementation** in `src/config/mod.rs`

4. **Review waterfall exhaustion logic** for deferred bead handling

5. **Debug explore strand** workspace scanning with test directories

## References

- **CI Workflow**: needle-ci (iad-ci cluster, argo-workflows namespace)
- **Argo UI**: https://argo-ci.ardenone.com (VPN required)
- **Kubeconfig**: `/home/coding/.kube/iad-ci.kubeconfig`
- **Git Commit**: `1960debc` - "store needle-ci failure pod logs from 2026-08-29"

---

**Generated**: 2026-08-29  
**Analysis Based**: needle-ci-failure-2026-08-29.log (718 lines)  
**Bead Context**: needle-8834f6b4 - Document exit 101 with full context
