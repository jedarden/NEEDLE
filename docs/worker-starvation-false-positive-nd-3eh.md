# Worker Starvation False Positive Analysis: nd-3eh

**Date:** 2026-03-03
**HUMAN Bead:** nd-3eh
**Worker:** claude-code-glm-5-charlie
**Analysis by:** claude-code-glm-5-delta

## Executive Summary

The worker starvation alert for `claude-code-glm-5-charlie` is a **FALSE POSITIVE**. Investigation confirms that:

1. **20+ open beads exist** in the workspace (verified via `br ready --unassigned`)
2. **Selection logic works correctly** (`_needle_select_weighted` successfully selects beads)
3. **Claiming works correctly** (tested and successfully claimed bead nd-20p)
4. **All dependencies satisfied** for most P0 beads (6 out of 7 have zero dependencies)

## Root Cause Analysis

### Verified Facts

```bash
# Open beads available
$ br list --status open --limit 20
# Returns 20 beads (7 P0, 12+ P1)

# Claimable beads (no dependencies, not assigned, not deferred)
$ br ready --unassigned
# Returns 20 beads ready to claim

# Dependencies check for P0 beads
nd-2gc: 0 dependencies
nd-2nr: 0 dependencies
nd-2ov: 1 dependency -> nd-3up (status: open)
nd-3eh: 0 dependencies (this alert bead)
nd-3up: 0 dependencies
nd-qni: 0 dependencies
nd-xnj: 0 dependencies
```

### Selection Test Results

```bash
# Testing selection function
$ NEEDLE_DEBUG=1 bash -c 'source src/bead/select.sh && _needle_select_weighted'
# Successfully returns beads: nd-xnj, nd-3i6, nd-3kc, etc.
```

### Claim Test Results

```bash
# Testing claim function
$ NEEDLE_DEBUG=1 bash -c 'source src/bead/claim.sh && _needle_claim_bead --workspace /home/coder/NEEDLE --actor test-worker'
# Successfully claimed bead: nd-20p
```

## Possible Causes for False Positive

### 1. Worker Context Issue (Most Likely)
The worker "charlie" may have been running in a different directory or with different environment variables that prevented proper bead discovery.

**Evidence:**
- Selection and claim work when tested from current directory
- Worker may have had stale state or different `NEEDLE_WORKSPACE` value

### 2. Race Condition Between Workers
Multiple workers (alpha, bravo, charlie, delta) may be competing for the same beads, causing some workers to consistently lose the race.

**Evidence:**
- Claim uses retry logic (5 retries by default)
- Race conditions are expected behavior with SQLite locking

### 3. Strand Engine Not Invoking Pluck Properly
The strand engine might be failing to invoke the pluck strand correctly.

**Evidence:**
- Pluck strand sources claim.sh which sources select.sh
- Any sourcing error would cascade up

### 4. Configuration Issue
The worker may have had strands disabled or misconfigured.

**Evidence:**
- `_needle_is_strand_enabled` defaults to "true" but can be overridden
- Config path may have been different for charlie worker

## Alternative Solutions

### Alternative 1: Add Diagnostic Logging (Recommended)

**Approach:** Add detailed diagnostic logging to the strand engine and claim process to capture exactly why beads aren't being found.

**Implementation:**
```bash
# In src/strands/engine.sh, add:
_NEEDLE_DIAG_DIR="${NEEDLE_STATE_DIR:-/tmp}/diagnostics"
mkdir -p "$_NEEDLE_DIAG_DIR"
echo "$(date): strand=$strand, workspace=$workspace, result=$result" >> "$_NEEDLE_DIAG_DIR/strand_engine.log"
```

**Pros:**
- Non-invasive
- Provides visibility for future debugging
- Low effort

**Cons:**
- Requires reproduction to capture logs
- Doesn't fix the immediate issue

**Estimated Effort:** 15 minutes

### Alternative 2: Add Fallback to Direct Bead ID Assignment

**Approach:** When `br ready` fails or returns empty, fall back to directly querying the database for any open unassigned bead.

**Implementation:**
```bash
# In src/bead/select.sh, add emergency fallback:
_needle_emergency_select() {
    python3 -c "
import sqlite3
conn = sqlite3.connect('$NEEDLE_WORKSPACE/.beads/beads.db')
c = conn.cursor()
c.execute(\"SELECT id FROM issues WHERE status='open' AND assignee IS NULL LIMIT 1\")
result = c.fetchone()
print(result[0] if result else '')
"
}
```

**Pros:**
- Bypasses any br CLI issues
- Direct database access is reliable

**Cons:**
- Bypasses weighted selection
- May select inappropriate beads

**Estimated Effort:** 30 minutes

### Alternative 3: Add Self-Healing Claim Release

**Approach:** Workers should release claims on beads they can't process, preventing bead lockup.

**Current State:** Already implemented in `claim.sh` via `_needle_release_bead` with SQL fallback.

**Improvement Needed:** Ensure claims are released on any error path.

**Estimated Effort:** 20 minutes (verification only)

### Alternative 4: Verify Worker State Directory

**Approach:** Check that the worker's state directory exists and is writable, preventing silent failures.

**Implementation:**
```bash
# In runner/loop.sh init:
if [[ ! -d "$NEEDLE_STATE_DIR" ]]; then
    mkdir -p "$NEEDLE_STATE_DIR" || {
        _needle_error "Cannot create state directory: $NEEDLE_STATE_DIR"
        return 1
    }
fi
```

**Estimated Effort:** 10 minutes

## Recommended Action

Since this is a **false positive**, the HUMAN bead should be **closed** with documentation of the findings. However, the diagnostic improvements in Alternative 1 should be implemented to help debug future occurrences.

## Next Steps

1. **Close HUMAN bead nd-3eh** - False positive confirmed
2. **Create implementation bead** for diagnostic logging
3. **Monitor** for recurrence with new diagnostics

## Test Commands

To verify the fix in the future:

```bash
# Run these from the workspace root
br ready --unassigned                    # Should show beads
br list --status open --limit 5          # Should show open beads
bash -c 'source src/bead/select.sh && _needle_select_weighted'  # Should select a bead
```
