# Fleet Validation Guide for needle-44e7e5cd

## Overview

This guide documents the fleet validation process for the Mend staleness fix (bead needle-44e7e5cd). The fix prevents accumulation of open+assigned beads that become invisible to the ready frontier when `--count 1` workers relaunch under the same name.

## Failure Case (2026-08-16)

A fleet-wide sweep found **583 beads** stuck in assigned+open state across 47 of 66 workspaces. These beads were:
- Assigned to a worker name
- Status: Open (not InProgress)
- Invisible to `bf --ready` frontier
- Unable to be claimed by any worker

Root cause: `--count 1` workers relaunch under the same NATO name forever, making the assignee appear "alive" to Mend even though the worker was no longer working on that bead.

## The Fix

Mend now checks both:
1. **Worker liveness** (PID check)
2. **Current bead assignment** (heartbeat check)

An assignee on an **open** bead is cleared if:
- Worker has no heartbeat AND is not in registry (dead), OR
- Worker has a heartbeat BUT is working on a DIFFERENT bead, OR
- Worker has a heartbeat BUT is idle (no current bead)

## Validation Timeline

### Pre-Deployment Baseline (Before Fix Deployment)

Run the fleet validation script to establish a baseline:

```bash
./scripts/fleet-stale-assignee-check.sh /tmp/baseline-before-fix.json
```

Expected result: High counts (possibly 500+ beads across workspaces).

### Immediate Post-Deployment (Day 0-1)

After deploying the fix, run the validation script:

```bash
./scripts/fleet-stale-assignee-check.sh /tmp/baseline-day1.json
```

Expected result: Significant drop in stale assignee counts as Mend clears accumulated beads.

### Sustained Validation (Day 2-7)

Run daily validation to ensure counts stay low:

```bash
# Day 2
./scripts/fleet-stale-assignee-check.sh /tmp/baseline-day2.json

# Day 3
./scripts/fleet-stale-assignee-check.sh /tmp/baseline-day3.json

# Continue daily for a week
```

Expected result: Counts should remain near zero (< 50 total, ideally single digits).

## Acceptance Criteria

The fix is validated when:

1. **Regression test passes**: `cargo test mend_stale_assignee_regression`
2. **Immediate drop**: Day 0-1 shows significant reduction from baseline
3. **Sustained low counts**: Day 2-7 show counts stay below:
   - **Total fleet**: < 50 stale assignees
   - **Per workspace**: < 5 stale assignees (ideally 0-1)
4. **No rebound**: Counts don't climb back toward 583

## Using the Validation Script

### Basic Usage

```bash
# Human-readable output only
./scripts/fleet-stale-assignee-check.sh

# Save both human-readable and JSON output
./scripts/fleet-stale-assignee-check.sh /tmp/validation-$(date +%Y%m%d).json
```

### Interpreting Output

**Healthy** (GREEN):
```
✓ HEALTHY: No stale assignees found
The fix for needle-44e7e5cd is working correctly.
```

**Warning** (YELLOW):
```
⚠ WARNING: 15 stale assignee(s) found
This is within acceptable baseline but should be monitored.
```

**Critical** (RED):
```
✗ CRITICAL: 234 stale assignees found!
This exceeds the baseline threshold and may indicate the fix is not working.
```

### Exit Codes

- `0`: Healthy (0 stale assignees)
- `2`: Warning (1-49 stale assignees)
- `1`: Critical (50+ stale assignees)

## Automated Monitoring

For continuous monitoring, add to cron:

```bash
# Daily fleet validation at 10 AM UTC
0 10 * * * /home/coding/NEEDLE/scripts/fleet-stale-assignee-check.sh /var/log/needle/stale-assignees-$(date +\%Y\%m\%d).json
```

## Comparison Script

Compare two validation runs:

```bash
#!/bin/bash
# compare-validations.sh baseline.json current.json

BEFORE=$(jq '.summary.stale_assignee_count' "$1")
AFTER=$(jq '.summary.stale_assignee_count' "$2")
DELTA=$((AFTER - BEFORE))

echo "Before: $BEFORE stale assignees"
echo "After:  $AFTER stale assignees"
echo "Change: $DELTA"

if [[ $AFTER -lt $BEFORE ]]; then
    echo "✓ Improvement: $((BEFORE - AFTER)) beads cleared"
elif [[ $AFTER -eq $BEFORE ]]; then
    echo "No change"
else
    echo "✗ Regression: $((AFTER - BEFORE)) new stale beads"
fi
```

## Troubleshooting

### High counts persist

1. **Check Mend is running**: Workers should have Mend enabled
2. **Check bead-rs version**: Must support the fix (bead-rs R026+)
3. **Check worker heartbeats**: Workers must emit heartbeats with `current_bead`
4. **Check worker registry**: Workers must register in the registry

### Single workspace with high count

Investigate the specific workspace:
```bash
# Find the workspace
./scripts/fleet-stale-assignee-check.sh | grep "WORKSPACE-NAME"

# Check its beads directly
sqlite3 ~/.beads/beads.db "SELECT id, title, assignee FROM issues WHERE status = 'open' AND assignee IS NOT NULL;"
```

## References

- **Bead**: needle-44e7e5cd
- **Fix commit**: (to be added after deployment)
- **Regression test**: `tests/mend_stale_assignee_regression.rs`
- **Implementation**: `src/strand/mend.rs::cleanup_stale_assignees_on_open_beads`
- **Incident date**: 2026-08-16
- **Baseline**: 583 beads across 47 workspaces
