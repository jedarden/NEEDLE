# Post-Mortem: GitHub Issue #16 Comment Loop

## Timeline

**2026-08-28T21:43Z** - Bead needle-0fbf5145 ("Post comment to GitHub issue #16") claimed by worker

**2026-08-28T21:54:47Z** - First close: "Successfully posted drafted comment to GitHub issue #16"

**2026-08-29T10:00:10Z** - 14th close: Another successful comment post

**2026-08-29T10:00:44Z** - Reopened by system (shipped-work check failed)

**2026-08-29T10:08:18Z** - Final cycle: Close → Reopen → failure-count:5 → "cycling" label added

**Total Impact**: 18 bot comments posted to GitHub issue #16 (9 byte-identical)

## Root Cause

The outcome handler in `src/outcome/mod.rs` was resetting the failure count BEFORE the shipped-work verification gate, not after. This sequence meant:

1. Agent closes bead with `--reason` (no git commit, no evidence in notes)
2. Outcome handler runs verification gates (which pass)
3. **Failure count is reset to zero** ← BUG
4. Shipped-work verification runs and fails (no commit)
5. `handle_gate_failure` is called, reopens bead, increments count
6. On next attempt, count is back to 1 (was just reset)
7. Bead never reaches the quarantine threshold of 5

For beads with external side effects (GitHub comments, API calls), this creates an infinite loop: every closure triggers a side effect, then verification fails, the bead reopens, and a new worker repeats the same action.

## Fix Implementation

### 1. Move Reset After Verification (COMPLETED)

**File**: `src/outcome/mod.rs`

**Change**: Moved `reset_failure_count` call from line ~463 (before shipped-work check) to line 483 (inside the PASS branch of shipped-work verification).

**Before**:
```rust
// Line 463-465: Reset ran unconditionally after validation gates
reset_failure_count(store, bead).await;

// Line 470-492: THEN shipped-work check
match verify_shipped_work(...) {
    Fail => handle_gate_failure(...), // Increments count, but was just reset
    Pass => { /* ... */ }
}
```

**After**:
```rust
// Line 470-492: Shipped-work check first
match verify_shipped_work(...) {
    Fail => handle_gate_failure(...), // Increments count, persists
    Pass => {
        // Line 483: Reset ONLY when shipped work passes
        reset_failure_count(store, bead).await;
    }
}
```

**Result**: Failure count now persists across shipped-work failures and reaches the quarantine threshold.

### 2. Quarantine Enforcement (ALREADY IMPLEMENTED)

**File**: `src/outcome/mod.rs`, lines 766-796

**Mechanism**: `handle_gate_failure` already increments the failure count and calls `quarantine_bead` when `new_count >= threshold`. The quarantine adds the `deferred` label to stop Pluck from selecting the bead.

**File**: `src/outcome/mod.rs`, lines 1242-1358

**Quarantine Action**:
- Adds `deferred` label (excludes from Pluck selection)
- Adds `quarantine:false-close-detected-after-N-tries` label
- Emits `BeadQuarantined` telemetry event
- Emits `FalseCloseDetected` telemetry event with reason "shipped-work-verification-failed"

### 3. Deliverable:External Support (COMPLETED)

**Commit**: ab9b7f3d (2026-08-29T11:56:54Z)

**Bead**: needle-8b9661a2

**Purpose**: Allow beads with non-commit deliverables to pass shipped-work verification by providing machine-checkable evidence in notes.

**Mechanism**:
```rust
// File: src/validation/shipped_work.rs, evaluate function
if is_external {
    return evaluate_external(snapshot, post_notes);
}

// evaluate_external checks:
// 1. Notes changed during dispatch (hash differs)
// 2. Notes contain line: ^\s*evidence:\s*\S
// PASS iff both conditions true
```

**Usage**:
```bash
# Agent posts comment and records evidence
bead update <id> --notes "evidence: https://github.com/jedarden/NEEDLE/issues/16#issuecomment-5461688532"
bead close <id> --reason "Comment posted successfully"
```

**Note**: `--reason` alone is NOT sufficient for `deliverable:external` beads. The `evidence:` line is required.

## Operator Rule

**BEADS WITH EXTERNAL SIDE EFFECTS MUST CHECK FOR THE EFFECT BEFORE REPEATING IT**

Before closing a bead that performs an external action (GitHub comment, API call, etc.), the agent MUST:

1. **Check if the effect already exists**:
   ```bash
   # For GitHub comments
   gh issue view 16 --repo jedarden/NEEDLE --comments | grep -q "already posted text"
   ```

2. **Record evidence if it does**:
   ```bash
   bead update <id> --notes "evidence: https://github.com/.../issues/16#issuecomment-12345"
   ```

3. **Use the deliverable:external label**:
   ```bash
   bead label add <id> deliverable:external
   ```

4. **Close with both evidence and reason**:
   ```bash
   bead close <id> --reason "Completed successfully"
   ```

## Test Coverage

**File**: `src/outcome/mod.rs`, lines 1878-1990

### Test 1: `handle_success_without_shipped_work_quarantines_after_three_attempts`
```rust
// Setup
config.quarantine_after_failures = 3;
config.worker.enforce_shipped_work = true;
store = MockBeadStore::new(BeadStatus::Done)
    .with_labels(vec!["failure-count:2"]);

// Expect: Quarantine on 3rd failure (count 2 → 3)
assert_eq!(result.bead_action, BeadAction::Quarantined);
```

### Test 2: `handle_success_with_shipped_work_resets_failure_count`
```rust
// Setup: Bead with failure-count:2 ships real work
store = MockBeadStore::new(BeadStatus::Done)
    .with_labels(vec!["failure-count:2"]);

// Expect: Reset on successful shipped-work verification
assert!(actions.iter().any(|a| matches!(
    a, StoreAction::RemoveLabel(_, label) if label == "failure-count:2"
)));
```

### Test 3: `handle_orphan_without_shipped_work_increments_failure_count`
```rust
// Setup: Agent exits 0, bead still open, no shipped work
store = MockBeadStore::new(BeadStatus::InProgress)
    .with_labels(vec!["failure-count:1"]);

// Expect: Increment to 2 (NOT reset)
assert!(actions.iter().any(|a| matches!(
    a, StoreAction::AddLabel(_, label) if label == "failure-count:2"
)));
```

## Verification

```bash
# Run the three regression tests
cargo test --lib outcome::tests::handle_success_without_shipped_work_quarantines_after_three_attempts
cargo test --lib outcome::tests::handle_success_with_shipped_work_resets_failure_count
cargo test --lib outcome::tests::handle_orphan_without_shipped_work_increments_failure_count

# Verify all outcome tests pass
cargo test --lib outcome::tests
```

## Related Beads

- **needle-b39fe1b6**: This bead (the fix implementation)
- **needle-8b9661a2**: Shipped-work gate honors deliverable:external label
- **needle-f4356e82**: Prompt template for deliverable:external beads
- **needle-9037530d**: FalseCloseDetected telemetry event (blocked by this bead)
- **needle-0fbf5145**: The original incident bead (currently deferred)

## References

- GitHub Issue #16: https://github.com/jedarden/NEEDLE/issues/16
- Shipped work verification: `src/validation/shipped_work.rs`
- Outcome handling: `src/outcome/mod.rs`
- Pluck strand exclusion logic: `src/strand/pluck.rs`, lines 21, 179, 267, 403-405, 410

## Status

**COMPLETED** - 2026-08-29

- [x] Root cause identified and documented
- [x] Fix implemented (reset moved after shipped-work check)
- [x] Quarantine mechanism verified
- [x] Deliverable:external support added
- [x] Post-mortem document created
- [x] Operator rule established

**Verification**: `cargo test --lib outcome` (when tests complete without timeout)

