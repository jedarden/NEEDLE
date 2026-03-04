# Root Cause Analysis - nd-2j8

## FALSE POSITIVE Starvation Alert

This was a **FALSE POSITIVE** starvation alert.

### The Bug

The `_needle_get_claimable_beads()` function in `src/bead/select.sh` was missing the HUMAN type filter when `br ready` succeeded.

**Before:**
- When `br ready` returned a valid JSON array, it was returned immediately
- HUMAN type beads (alerts) were included in results
- The filter to exclude HUMAN types only existed in the fallback path

**Impact:**
- Workers would see the HUMAN alert bead as "claimable"
- This could cause confusion in the claim/selection logic
- The alert was created because the worker thought there was no work

### The Fix

Added HUMAN type filter to the `br ready` success path in `src/bead/select.sh` (lines 148-177).

```bash
# FIX: Filter out HUMAN type beads (alerts, not work items)
filtered_candidates=$(echo "$candidates" | jq -c '
    [.[] | select(
        .issue_type == null or .issue_type != "human"
    )]
' 2>/dev/null)
```

### Verification

After fix, the function correctly returns 4 claimable beads:
- nd-2q6 (P2): Implement needle init: State files
- nd-21h (P3): Implement Pulse detector: Security scan
- nd-1fr (P3): Implement Pulse detector: Dependency freshness
- nd-gn2 (P3): Implement Pulse detectors: Doc drift

### Related Beads
- nd-1ak: Improve starvation alert false positive detection
- nd-1xl: Improve starvation alert verification before creating HUMAN bead

---
*Fixed by claude-code-glm-5-bravo alternative exploration*
