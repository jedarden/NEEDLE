# Worker Starvation Alternative Solutions

**Date:** 2026-03-03
**Related Bead:** nd-165 (CLOSED)
**Status:** RESOLVED - Fixes Applied

## Summary

The worker starvation alert (nd-165) was triggered when the NEEDLE worker could not find claimable beads. Investigation revealed multiple bugs that prevented the fallback mechanism from working correctly.

## Root Causes (All Fixed)

1. **Debug Output Pollution** (`src/lib/output.sh`)
   - `_needle_debug` and `_needle_verbose` were outputting to stdout instead of stderr
   - This corrupted JSON output when captured in subshells
   - **Fix:** Redirected output to stderr (`>&2`)

2. **Workspace Not Honored in Fallback** (`src/bead/select.sh`)
   - `br list` operates on current directory but fallback wasn't changing directories
   - **Fix:** Added `cd "$workspace" && br list` when workspace is provided

3. **Dependency Filter Missing** (`src/bead/select.sh`)
   - Fallback didn't filter by `dependency_count`, allowing blocked beads
   - **Fix:** Added `dependency_count == 0` to filter criteria

4. **Claim Command Not Workspace-Aware** (`src/bead/claim.sh`)
   - `br update --claim` runs in current directory, not workspace
   - **Fix:** Run claim in workspace directory when `--workspace` provided

## Alternative Solutions for Future Starvation Scenarios

### Alternative 1: Workaround Tools (IMMEDIATE)

**Use Case:** When `br ready` fails but fallback should work

```bash
# List claimable beads directly
./bin/needle-ready

# Or source and call directly
source src/bead/select.sh
_needle_get_claimable_beads --workspace /path/to/workspace
```

**Pros:**
- Bypasses `br ready` schema issues
- Uses fallback mechanism directly
- No code changes needed

**Cons:**
- Requires manual intervention
- Doesn't fix root cause

### Alternative 2: Database Rebuild (SHORT-TERM)

**Use Case:** When database schema is corrupted

```bash
./bin/needle-db-rebuild
```

**Pros:**
- Can resolve schema mismatches
- Refreshes database from source of truth (JSONL)

**Cons:**
- May lose transient state
- Takes time for large databases

### Alternative 3: Upgrade beads_rust (PERMANENT)

**Use Case:** When bugs in beads_rust are causing issues

```bash
# Upgrade to version with schema fix
cargo install beads_rust --version 0.1.14+
```

**Pros:**
- Permanent fix for `br ready` schema errors
- No fallback needed

**Cons:**
- Requires beads_rust release with fix
- External dependency upgrade

### Alternative 4: Better Detection Logic (PREVENTATIVE)

**Use Case:** Prevent false positive starvation alerts

**Implementation:**
1. Check if fallback found beads before alerting
2. Log diagnostic information when starvation detected
3. Verify bead claimability before creating alert

**Location:** `src/strands/knot.sh` (Strand 7 - Alert Human)

**Pros:**
- Prevents false positives
- Better observability

**Cons:**
- Requires code changes
- May mask real issues if not careful

## Verification

Current state shows 19+ claimable beads:

```bash
$ ./bin/needle-ready
nd-1pu [P0] Implement worker loop: Bead execution and effort recording
nd-qni [P0] Implement worker loop: Core structure and initialization
nd-2ov [P0] Implement needle run: Single worker invocation
...
```

## Recommendations

1. **Immediate:** Close starvation alert beads after verifying fixes
2. **Short-term:** Add integration tests for fallback path
3. **Long-term:** Upgrade beads_rust to fix schema bug

## Related Documentation

- `docs/worker-starvation-false-positive.md` - Previous false positive analysis
- `src/bead/select.sh` - Fallback implementation
- `bin/needle-ready` - Workaround tool

## Skills

- `worker-starvation-false-positive` - Skill for handling this scenario
