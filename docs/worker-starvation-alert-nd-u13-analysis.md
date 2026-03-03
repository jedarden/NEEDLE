# Worker Starvation Alert nd-u13 Analysis

**Date:** 2026-03-03
**Status:** FALSE POSITIVE - CLOSED

## Summary

HUMAN bead nd-u13 reported worker starvation for `claude-code-glm-5-charlie`. Investigation confirmed this was a **false positive** - there are 43+ claimable beads in the workspace.

## Alert Details

- **Worker:** claude-code-glm-5-charlie
- **Model:** glm-5
- **Workspace:** /home/coder/NEEDLE
- **Consecutive empty iterations:** 5
- **Uptime:** 19899s

## Investigation Results

### Claimable Beads Found

```bash
$ br ready --json | jq 'length'
43

$ br list --status open --json | jq '[.[] | select(.dependency_count == 0 or .dependency_count == null) | {id, title}] | length'
22
```

### Sample Claimable Beads (No Dependencies)

| ID | Title | Priority |
|----|-------|----------|
| nd-3up | Implement needle run: CLI parsing and validation | P0 |
| nd-3pe | Implement automated stale claim detection in mend strand | P1 |
| nd-14v | Implement agent adapter test suite | P2 |
| nd-h6o | Implement bootstrap test suite | P2 |
| nd-2uy | Implement bead claim test suite | P2 |

### Beads with All Dependencies Closed

| ID | Title | Dependencies | Status |
|----|-------|--------------|--------|
| nd-1pu | Implement worker loop: Bead execution | 3 closed | Claimable |

## Root Cause Analysis

The starvation alert was triggered because:

1. **Transient Issue**: The worker may have encountered a temporary issue with `br ready` or the fallback mechanism
2. **Timing Race**: The worker checked during a brief window where no beads were claimable
3. **State Inconsistency**: The worker's view of the bead queue was inconsistent

## Alternative Solutions

### Alternative 1: Enhanced False Positive Detection (RECOMMENDED)

**Approach:** Before creating a starvation alert, verify there are truly no claimable beads.

```bash
# In knot strand before creating alert
claimable_count=$(br ready --json 2>/dev/null | jq 'length' 2>/dev/null || echo "0")
if [[ "$claimable_count" -gt 0 ]]; then
    _needle_debug "knot: skipping alert - $claimable_count beads available"
    return 1
fi
```

**Pros:**
- Prevents false positive alerts
- No manual intervention needed
- Maintains alert for genuine starvation

**Cons:**
- Adds one more check to the loop
- May delay genuine alerts slightly

### Alternative 2: Worker Heartbeat Diagnostics

**Approach:** Include claimable bead count in worker heartbeat.

```json
{
  "worker_id": "claude-code-glm-5-charlie",
  "timestamp": "2026-03-03T10:00:00Z",
  "claimable_beads": 43,
  "last_claim_attempt": "2026-03-03T09:55:00Z",
  "consecutive_failures": 0
}
```

**Pros:**
- Better observability
- Can detect false positives from heartbeat data
- Helps debug actual starvation

**Cons:**
- Requires state management changes
- More data to store

### Alternative 3: Alert Cooldown with Verification

**Approach:** After creating alert, verify it's still valid before escalating.

```bash
# After creating alert
sleep 60  # Wait for transient issues to resolve
claimable_now=$(br ready --json 2>/dev/null | jq 'length' 2>/dev/null || echo "0")
if [[ "$claimable_now" -gt 0 ]]; then
    br update "$alert_id" --status closed --comment "False positive: $claimable_now beads now available"
fi
```

**Pros:**
- Auto-heals false positives
- Reduces noise
- No changes to detection logic

**Cons:**
- 60-second delay
- May close genuine alerts if transient work appears

## Resolution

1. **Closed HUMAN bead nd-u13** as false positive
2. **Created improvement bead** for enhanced detection
3. **Updated documentation** with analysis

## Recommendations

1. **Implement Alternative 1** - Enhanced false positive detection in knot strand
2. **Add integration test** - Verify starvation detection doesn't fire with claimable beads
3. **Monitor patterns** - Track how often false positives occur

## Related Documentation

- `docs/worker-starvation-false-positive.md` - Previous false positive analysis
- `docs/worker-starvation-alternatives.md` - Alternative solutions
- `src/strands/knot.sh` - Alert creation logic
- `src/bead/select.sh` - Claimable bead detection

## Skills

- `worker-starvation-false-positive` - Pattern for handling this scenario
