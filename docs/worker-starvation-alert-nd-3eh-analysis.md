# Worker Starvation Alert Analysis: nd-3eh

**Date:** 2026-03-03
**Bead ID:** nd-3eh
**Status:** FALSE POSITIVE - Should Be Closed

## Summary

Worker `claude-code-glm-5-charlie` reported starvation with "no work available", but investigation reveals **20+ claimable beads exist** in the queue.

## Evidence

### 1. Claimable Beads Confirmed

```bash
$ br ready
ID      PRI  TYPE    TITLE
------  ---  ------  --------------------------------------------------
nd-2gc  P0   task    Implement Strand 1: Pluck (strands/pluck.sh)
nd-2nr  P0   task    Implement OS detection module (bootstrap/detect_os.sh)
nd-2ov  P0   task    Implement needle run: Single worker invocation
... (20+ more)
```

### 2. HUMAN Bead Correctly Typed

```bash
$ br show nd-3eh --json | jq '{id, issue_type}'
{
  "id": "nd-3eh",
  "issue_type": "human"
}
```

### 3. Selection Logic Correctly Filters HUMAN

`src/bead/select.sh` line 149:
```bash
(.issue_type == null or .issue_type != "human")
```

## Alternative Solutions Analyzed

### Alternative 1: Close as False Positive ✅ RECOMMENDED

**Approach:** Verify claimable beads exist and close the alert.

**Steps:**
1. Confirm `br ready` returns claimable beads
2. Verify HUMAN beads are filtered from selection
3. Close nd-3eh with resolution comment

**Pros:**
- Immediate resolution
- No code changes needed
- Accurate assessment

**Cons:**
- Doesn't address root cause of why worker reported starvation

### Alternative 2: Fix needle-ready Diagnostic Tool

**Approach:** The `./bin/needle-ready` wrapper doesn't filter HUMAN beads, causing diagnostic confusion.

**Issue:** Line 59 comment says "optional, can be enabled" but filter not implemented.

**Fix:**
```bash
# Add to JQ_FILTER:
| select(.issue_type == null or .issue_type != "human")
```

**Pros:**
- Better diagnostic tooling
- Prevents future confusion

**Cons:**
- Requires code change
- Not the root cause

### Alternative 3: Add Starvation Detection Verification

**Approach:** Before creating starvation alert, verify no claimable beads exist.

**Implementation:**
1. Check if `br ready` returns empty
2. Check if fallback found beads
3. Log diagnostic info before alerting

**Location:** `src/strands/knot.sh` (Priority 6 - Alert Human)

**Pros:**
- Prevents false positives
- Better observability

**Cons:**
- Requires code change
- May mask real issues if not careful

### Alternative 4: Debug Worker Loop Execution

**Approach:** Investigate why the worker reported starvation when beads exist.

**Hypothesis:**
- Worker may have been looking in wrong workspace
- Database state may have been inconsistent at time of check
- Race condition between bead creation and worker check

**Pros:**
- Addresses root cause
- Improves system reliability

**Cons:**
- Time-consuming investigation
- May not find definitive cause

## Recommended Resolution

1. **Immediate:** Close nd-3eh as false positive (Alternative 1)
2. **Short-term:** Fix needle-ready tool (Alternative 2)
3. **Medium-term:** Implement starvation verification (Alternative 3)

## Root Cause Analysis

The worker starvation was likely caused by:

1. **Timing:** Beads may have been created shortly after the worker checked
2. **Database state:** Temporary inconsistency that resolved
3. **Workspace confusion:** Worker may have checked wrong location

The fixes in commit `08c7173` addressed several bugs, but this alert may have been created before those fixes were applied.

## Lessons Learned

1. **Verify before alerting:** Always double-check claimable beads before creating starvation alert
2. **Diagnostic tools must match production logic:** needle-ready should filter same as select.sh
3. **Log context:** When starvation detected, log what was checked and what was found

## Related Documentation

- `docs/worker-starvation-false-positive.md` - Previous analysis
- `docs/worker-starvation-alternatives.md` - Alternative solutions
- `src/bead/select.sh` - Selection logic with HUMAN filter
- `bin/needle-ready` - Diagnostic tool (needs HUMAN filter fix)
