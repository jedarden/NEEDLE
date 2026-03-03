# Worker Starvation Alert nd-390 Analysis

**Date:** 2026-03-03
**Bead ID:** nd-390
**Status:** FALSE POSITIVE - Work Available

## Summary

Worker `claude-code-glm-5-delta` triggered a starvation alert despite 18+ claimable beads being available in the workspace. This is a **false positive** caused by a discovery/claim logic issue, not an actual lack of work.

## Evidence of False Positive

### Available Beads (verified 2026-03-03)

```
$ br ready --workspace /home/coder/NEEDLE
ID      PRI  TYPE    TITLE
------  ---  ------  --------------------------------------------------
nd-2gc  P0   task    Implement Strand 1: Pluck (strands/pluck.sh)
nd-2nr  P0   task    Implement OS detection module (bootstrap/detect_os.sh)
nd-2ov  P0   task    Implement needle run: Single worker invocation
nd-3up  P0   task    Implement needle run: CLI parsing and validation
nd-qni  P0   task    Implement worker loop: Core structure and initialization
nd-xnj  P0   task    Implement worker naming module (runner/naming.sh)
... (18 total claimable beads)
```

### Bead Status Check

All beads are:
- Status: `open`
- Claimed: `null` (unclaimed)
- Dependency count: `0` (no blocking dependencies)
- Type: `task` (not HUMAN/alert type)

## Root Cause Analysis

### Possible Causes

1. **Timing Issue**
   - Worker checked during a transient state
   - Beads were temporarily claimed by another worker
   - Race condition in claim/release cycle

2. **Workspace Path Mismatch**
   - Worker was looking in wrong directory
   - `br ready --workspace` vs `br list` behavior difference
   - Config workspace vs actual workspace mismatch

3. **Filter Bug**
   - The fallback filter in `select.sh` may have excluded beads incorrectly
   - Previous fixes addressed some issues, but edge cases may remain
   - `dependency_count` vs actual dependency status check

4. **PATH/Environment Issue**
   - `br` CLI not found during worker execution
   - Different PATH in worker subprocess
   - Missing environment variables

### Why Starvation Alert Was Created

The alert was created by "Priority 6" (strand engine fallback), indicating:
- All 7 strands returned "no work found"
- The pluck strand (Priority 1) failed to find beads
- The explore strand (Priority 2) found no workspaces with work
- Maintenance (Priority 3) completed successfully
- Gap analysis (Priority 4) found no gaps
- HUMAN alternatives (Priority 5) found no HUMAN beads to unblock
- Priority 6 created the alert

## Alternative Solutions

### Alternative 1: Pre-Flight Verification (RECOMMENDED)

**Approach:** Add verification step before creating starvation alert.

**Implementation:**
```bash
# In starvation alert creation code
_verify_work_available() {
    local workspace="$1"

    # Direct check using br list (bypasses potential br ready issues)
    local count
    count=$(cd "$workspace" && br list --status open --json 2>/dev/null | \
            jq '[.[] | select(.claim_token == null or .claim_token == "") | select(.issue_type != "human")] | length')

    if [[ "$count" -gt 0 ]]; then
        _needle_debug "Pre-flight found $count claimable beads - skipping alert"
        return 0  # Work available, don't alert
    fi
    return 1  # No work, proceed with alert
}
```

**Pros:**
- Prevents false positive alerts
- Uses direct database query as source of truth
- Low overhead

**Cons:**
- Adds extra check before every alert
- May mask real issues if not careful

### Alternative 2: Enhanced Diagnostic Logging

**Approach:** Log detailed state when starvation is detected.

**Implementation:**
```bash
_log_starvation_diagnostics() {
    local workspace="$1"

    echo "=== STARVATION DIAGNOSTICS ===" >&2
    echo "Workspace: $workspace" >&2
    echo "Current directory: $(pwd)" >&2
    echo "PATH: $PATH" >&2
    echo "br location: $(which br 2>/dev/null || echo 'NOT FOUND')" >&2

    echo "--- br list output ---" >&2
    cd "$workspace" && br list --status open --json 2>&1 | head -100 >&2

    echo "--- br ready output ---" >&2
    br ready --workspace="$workspace" --unassigned --json 2>&1 | head -100 >&2

    echo "--- Filtered count ---" >&2
    br list --status open --json 2>/dev/null | \
        jq '[.[] | select(.claim_token == null)] | length' >&2
    echo "=== END DIAGNOSTICS ===" >&2
}
```

**Pros:**
- Helps diagnose root cause
- No behavior change
- Valuable for debugging

**Cons:**
- Doesn't prevent false positives
- Verbose output

### Alternative 3: Database Health Check

**Approach:** Verify database integrity before reporting starvation.

**Implementation:**
```bash
_check_database_health() {
    local workspace="$1"
    local db_file="$workspace/.beads/beads.db"

    if [[ ! -f "$db_file" ]]; then
        _needle_warn "No beads database found at $db_file"
        return 1
    fi

    # Check if database is readable
    if ! br list --json &>/dev/null; then
        _needle_warn "Database query failed - possible corruption"
        return 1
    fi

    return 0
}
```

**Pros:**
- Detects database issues
- Prevents alerts due to corrupted state

**Cons:**
- Doesn't catch logic bugs
- Additional overhead

### Alternative 4: Workaround Tool Usage

**Approach:** Use existing `bin/needle-ready` as fallback.

**Implementation:**
Already exists as `bin/needle-ready`. Workers can use this directly:

```bash
# Instead of relying on br ready
CLAIMABLE=$(./bin/needle-ready --json)
if [[ -n "$CLAIMABLE" ]] && [[ "$CLAIMABLE" != "[]" ]]; then
    # Work available, pick one
    BEAD_ID=$(echo "$CLAIMABLE" | jq -r '.[0].id')
    # ... claim and process
fi
```

**Pros:**
- Already implemented
- Bypasses br ready schema issues
- Client-side filtering

**Cons:**
- Requires code changes
- Not integrated into strand engine

## Recommended Resolution

1. **Immediate:** Close nd-390 as false positive
2. **Short-term:** Implement Alternative 1 (pre-flight verification)
3. **Long-term:** Add Alternative 2 (diagnostic logging) for debugging

## Action Items

- [ ] Close nd-390 with resolution notes
- [ ] Add pre-flight check to strand engine
- [ ] Create integration test for false positive scenario
- [ ] Update `worker-starvation-false-positive` skill

## Related Documentation

- `docs/worker-starvation-false-positive.md`
- `docs/worker-starvation-alternatives.md`
- `src/bead/select.sh` - Fallback implementation
- `bin/needle-ready` - Workaround tool
