# Strand Module Test Coverage Analysis

**Bead:** bf-5t5p7  
**Date:** 2026-07-14  
**Module:** `src/strand/`

## Overview

The strand module consists of 10 Rust source files implementing a prioritized waterfall of bead selection strategies. All modules have comprehensive unit tests in `#[cfg(test)]` sections, with 162 total tests across the module.

## Test Files Summary

| Module | Tests | Primary Focus |
|--------|-------|----------------|
| `mod.rs` | 11 | Waterfall orchestration, strand ordering, restart logic |
| `explore.rs` | 9 | Multi-workspace discovery, deadlock prevention |
| `knot.rs` | 13 | Exhaustion diagnostics, three-state verification |
| `mend.rs` | 68 | Maintenance operations, cleanup, registry management |
| `pluck.rs` | 13 | Primary bead selection, filtering, split logic |
| `pulse.rs` | 13 | Health scanning, bead creation from findings |
| `reflect.rs` | 7 | Meta-analysis, learning consolidation |
| `splice.rs` | 2 | Worker failure documentation |
| `unravel.rs` | 14 | Alternative proposal generation |
| `weave.rs` | 12 | Gap analysis, bead creation from docs |
| **Total** | **162** | |

## Functionality Coverage by Module

### 1. `mod.rs` - Waterfall Orchestrator

**Tests cover:**
- Empty waterfall returning no work
- First-come-first-served strand selection
- Waterfall restart on `WorkCreated`
- Strand name enumeration
- Error handling (continue to next strand)
- Restart cap preventing infinite loops (MAX_RESTARTS = 3)
- Strand evaluation after restart cap exceeded
- Empty `BeadFound` continuing to next strand
- Multiple beads returning first candidate
- Full waterfall construction from config

**Coverage:** Excellent - all major waterfall behaviors tested.

### 2. `explore.rs` - Multi-Workspace Discovery

**Tests cover:**
- Disabled strand returns no work
- Empty workspace list handling
- Home workspace exclusion
- Workspace without `.beads/` directory
- Nonexistent workspace paths
- **Deadlock scenarios:**
  - Multi-workspace with excluded first workspace
  - Assigned beads allow advancement
  - Excluded beads allow advancement
  - Combined excluded and assigned beads

**Missing coverage:**
- Registry integration for dead worker detection
- Timestamp-based ordering of workspace priority
- Error handling for permission issues

**Coverage:** Good - core logic covered, edge cases with registry partially tested.

### 3. `knot.rs` - Exhaustion Diagnostics

**Tests cover:**
- NO_BEADS_EXIST diagnosis (no alert)
- ALL_CLAIMED diagnosis (no alert)
- INVISIBLE diagnosis with telemetry emission
- Alert rate limiting with cooldown
- Diagnostic details in telemetry
- Mixed status (done/blocked) classification
- Store error handling
- All diagnostic outcomes (`diagnose_*` tests)
- Worker deduplication in claimed-by tracking
- Knot always returns NoWork

**Coverage:** Excellent - all three states and rate limiting thoroughly tested.

### 4. `mend.rs` - Maintenance Operations

**Tests cover:**
- **Stale peer cleanup:**
  - Crashed peer beads released (WorkCreated)
  - No stale peers returns NoWork
- **Orphaned in-progress cleanup:**
  - Orphaned beads released
  - Live worker beads not released
  - Own worker beads not released
  - Dead registered workers released
- **Collision handling:**
  - Same NATO, different adapter scenarios
  - Live vs dead worker handling
- **Lock file cleanup:**
  - Orphaned locks removed
  - Non-needle locks ignored
  - Mixed orphaned and held locks
- **Stale dependencies:**
  - Closed blocker removal
  - Open blocker preservation
  - Non-block type handling
  - Bead without dependencies handling
  - Telemetry emission on removal/error
- **Database operations:**
  - Repair on corruption
  - Clean database no work
  - Persistent corruption non-fatal
  - Full rebuild on repair failure
- **Agent log cleanup:**
  - Old file deletion
  - Recent file preservation
  - In-progress bead log preservation
  - Active worker log preservation
  - Unregistered worker log deletion
  - Retention policy handling (0 = disabled)
- **Heartbeat cleanup:**
  - Orphaned heartbeat removal
  - Registered heartbeat preservation
  - Fresh orphaned heartbeat preservation
  - Multiple orphaned removal
  - Qualified ID handling
  - Own heartbeat preservation
- **Registry cleanup:**
  - Dead worker removal
  - Live worker preservation
  - Own entry preservation
  - Multiple dead workers
  - Missing/corrupt file handling
- **Idle worker flagging:**
  - Beads processed check
  - Age threshold check
  - Boundary condition handling
  - Telemetry emission
  - Zero-bead workers
- **Trace and learning cleanup:**
  - Telemetry emission on cleanup

**Coverage:** Outstanding - 68 tests comprehensively cover all maintenance operations.

### 5. `pluck.rs` - Primary Selection

**Tests cover:**
- Candidate sorting: `(priority, created_at, id)` ordering
- Tie-breaking by bead ID
- Label filtering with default excludes
- Custom exclude label overrides
- Store-level filtering vs strand-level filtering
- All excluded returns NoWork via strand filter
- Stale assignee filtering
- Empty queue handling
- Store error propagation
- Deterministic ordering across same queue state
- **Split logic:**
  - Split triggered above threshold
  - Split not triggered below threshold
  - Split disabled when threshold = 0

**Coverage:** Excellent - all selection and filtering logic tested.

### 6. `pulse.rs` - Health Scanning

**Tests cover:**
- Strand name verification
- Disabled strand returns no work
- No scanners returns no work
- Cooldown enforcement
- Output parsing (warnings extraction)
- Severity respect
- Scanner execution with bead creation
- Deduplication across scans
- Max beads per run limiting
- No findings returns NoWork
- State persistence
- Custom prompt template substitution
- Default prompt template fallback

**Coverage:** Good - core scanning logic tested. Missing:
- Scanner execution error handling
- Multiple scanner interaction
- Configured threshold enforcement

### 7. `reflect.rs` - Meta-Analysis

**Tests cover:**
- Disabled strand behavior
- Threshold checking with/without state
- Force consolidation
- Agent invocation conditions (no retrospective)
- Agent skip conditions (retrospective present, None agent)
- Max extraction per run limiting

**Coverage:** Basic - entry conditions and agent invocation tested. Missing:
- Consolidate operation details
- Parse functionality
- State file operations
- Cooldown enforcement
- Skill promotion logic

### 8. `splice.rs` - Worker Failure Documentation

**Tests cover:**
- Disabled strand returns no work
- No heartbeats returns no work

**Coverage:** Minimal - only entry conditions tested. Missing:
- Dead worker detection
- Looping worker detection
- Failure bead creation
- State persistence (splice_state.json)
- Deduplication logic
- Heartbeat directory scanning

### 9. `unravel.rs` - Alternative Proposals

**Tests cover:**
- Strand name verification
- Disabled strand behavior
- No human beads returns no work
- Alternative child bead creation
- Max beads per run limiting
- Max alternatives per bead limiting
- 7-day cooldown enforcement
- No alternatives returns NoWork
- Agent failure handling
- Original bead preservation
- State persistence
- Custom prompt template substitution
- Default prompt template content

**Coverage:** Good - core logic tested. Missing:
- Parse functionality edge cases
- State file corruption handling
- Multiple human beads interaction

### 10. `weave.rs` - Gap Analysis

**Tests cover:**
- Disabled strand behavior
- Excluded workspace handling
- Cooldown enforcement
- No docs returns no work
- No gaps returns no work
- Bead creation from agent response
- Max beads per run limiting
- Deduplication (seen titles, existing beads)
- Agent failure returns error
- State persistence
- JSON parsing in code fences

**Coverage:** Good - creation and guardrails tested. Missing:
- Gap detection logic details
- Multiple workspace handling
- Documentation parsing edge cases

## Integration Test Coverage

Integration tests in `/tests/` directory include:

### `p2_integration_tests.rs`
- Mend strand stale peer cleanup
- Mend strand no work scenario
- Strand waterfall ordering with Mend

### `p3_integration_tests.rs`
- Weave, Unravel, Pulse strands with mock agents
- WorkCreated detection

### `integration_tests.rs`
- Pluck strand usage in end-to-end worker tests
- StrandResult pattern matching

### Other integration tests
- `otlp_integration.rs` - telemetry for strand evaluation
- `property_tests.rs` - invariant testing
- `real_br_integration_tests.rs` - real br CLI integration

## Test Dependencies and Setup

### Common Test Dependencies

All strand tests require:

1. **Tokio runtime** - `#[tokio::test]` for async execution
2. **Test utilities:**
   - Stub implementations of `Strand` trait
   - Mock `BeadStore` implementations
   - Test bead factories (`make_test_bead`)
   - Temporary directories (`tempfile::tempdir()`)
3. **Crate dependencies:**
   - `types` - `Bead`, `BeadId`, `StrandResult`, `BeadStatus`
   - `bead_store` - `BeadStore` trait
   - `telemetry` - `Telemetry` for emission testing
   - `config` - `Config` for `from_config` tests
   - `registry` - `Registry` for Explore/Mend strands

### Strand-Specific Setup

| Strand | Special Setup |
|--------|--------------|
| `explore` | Multiple workspace fixtures, registry mocks |
| `mend` | Registry files, heartbeat files, lock files, trace directories |
| `pluck` | Bead stores with priority/various statuses |
| `pulse` | Scanner configurations, output parsing fixtures |
| `reflect` | State files, learning files, retrospective fixtures |
| `splice` | Heartbeat fixtures (minimal test coverage) |
| `unravel` | Human-labeled beads, agent response fixtures |
| `weave` | Documentation fixtures, agent response fixtures |

## Missing Test Coverage Areas

### High Priority Gaps

1. **Splice (2 tests only)**
   - Actual dead/looping worker detection
   - Failure bead creation
   - State persistence and deduplication

2. **Reflect (7 tests)**
   - Consolidate operation details
   - Parse functionality edge cases
   - Cooldown enforcement

### Medium Priority Gaps

3. **Explore (9 tests)**
   - Registry integration edge cases
   - Workspace permission errors
   - Timestamp-based priority ordering

4. **Pulse (13 tests)**
   - Scanner error handling
   - Multiple scanner interaction
   - Configured threshold enforcement

### Low Priority Gaps

5. **Weave/Unravel**
   - Parse functionality edge cases
   - State file corruption handling
   - Multiple workspace interaction

## Test Quality Assessment

### Strengths
- Comprehensive test counts (162 total)
- Thorough Mend coverage (68 tests for complex logic)
- Good coverage of guardrails and edge cases
- Mock/stub patterns well-established
- Deterministic testing with stub implementations

### Areas for Improvement
- Splice severely under-tested (2 tests for complex functionality)
- Reflect consolidation logic needs direct testing
- Integration tests could be more comprehensive
- Missing error injection tests for file system failures
- No concurrency/stress testing for strand evaluation

## Conclusion

The strand module has **good to excellent test coverage** overall, with Mend being exceptionally thorough. The primary gaps are in **Splice** (minimal tests) and **Reflect** (entry conditions only). The test infrastructure is well-established with common patterns for mocks, stubs, and fixtures.

**Recommendation:** Address Splice coverage gap first, then Reflect consolidation testing.
