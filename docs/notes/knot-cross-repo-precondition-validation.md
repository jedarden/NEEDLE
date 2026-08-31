# Knot Strand Cross-Repo Precondition Validation

## Overview

Modified NEEDLE's Knot strand to automatically run `validate_cross_repo_preconditions.sh` BEFORE emitting a starvation alert. This prevents false positive alerts when the ready frontier is empty because all open beads are blocked by unmet cross-repo preconditions.

## Problem

When beads have unmet cross-repo preconditions (e.g., waiting for a bead in another workspace to be closed), they are not yet ready for work. The `validate_cross_repo_preconditions.sh` script marks these beads as `manual_blocked=true`, making them invisible to the ready frontier.

However, before this change, NEEDLE's Knot strand would detect the empty ready frontier and emit a starvation alert, even though the system was functioning correctly - it was just waiting on external work.

## Solution

The Knot strand now runs cross-repo precondition validation before emitting a starvation telemetry event:

1. **Before alert emission**: When the backoff window elapses and starvation is confirmed, run `validate_cross_repo_preconditions.sh`
2. **Check results**: If the script marks beads as `manual_blocked`, the empty ready frontier is legitimate
3. **Log instead of alert**: When beads are marked, log "starvation due to unmet cross-repo preconditions" and reset exhaustion tracking
4. **Only alert if needed**: Only emit starvation telemetry if no beads were marked (genuine configuration error)

## Implementation

### Modified Files

- `/home/coding/NEEDLE/src/strand/knot.rs`

### Changes Made

1. **Added `std::process::Command` import** (line 18)
   - Required to run external validation scripts

2. **Added `run_cross_repo_validation()` method** (lines 291-345)
   - Locates `validate_cross_repo_preconditions.sh` in SEAM workspace
   - Runs the script with `--verbose` flag
   - Parses output to count beads marked as `manual_blocked`
   - Returns tuple: `(beads_marked, success, details)`

3. **Modified `evaluate()` method** (lines 467-560)
   - After backoff window elapses, run cross-repo validation
   - Log validation results with structured tracing
   - If beads were marked, log warning and return without emitting telemetry
   - Reset exhaustion tracking when cross-repo preconditions explain the empty frontier
   - Only proceed with alert if no beads were marked (or validation failed)

## Behavior

### Normal Case (No Unmet Preconditions)

```
Starvation detected → Backoff window elapses
→ Cross-repo validation runs → No beads marked
→ Emission of starvation telemetry (as before)
```

### Cross-Repo Precondition Case (New Behavior)

```
Starvation detected → Backoff window elapses
→ Cross-repo validation runs → 5 beads marked as manual_blocked
→ Log warning: "starvation due to unmet cross-repo preconditions"
→ Reset exhaustion tracking → No telemetry emitted
```

### Validation Failure

```
Starvation detected → Backoff window elapses
→ Cross-repo validation fails → Log warning
→ Proceed with starvation telemetry (validation failure noted)
```

## Testing

The changes compile successfully:
```bash
cd /home/coding/NEEDLE && cargo check --lib
# Finished `dev` profile in 8.37s
```

All existing Knot strand tests continue to pass, ensuring backward compatibility.

## Benefits

1. **Prevents false positives**: No more starvation alerts when waiting on cross-repo dependencies
2. **Automatic detection**: Validation runs as part of the normal starvation detection flow
3. **Clear logging**: Structured logs distinguish between genuine starvation and cross-repo waiting
4. **Backward compatible**: Existing behavior unchanged when no cross-repo preconditions exist

## Related Components

- `validate_cross_repo_preconditions.sh` in SEAM workspace
- SEAM's `starvation_recovery_loop.go` (already runs this validator as part of recovery)
- NEEDLE's Knot strand (now runs this validator before alerting)

## Future Improvements

1. Consider caching validation results to avoid repeated script execution
2. Add metrics for cross-repo precondition frequency
3. Expose validation status in telemetry events
