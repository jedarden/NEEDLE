# Worker Starvation False Positive Analysis

**Date:** 2026-03-03
**Status:** RESOLVED - Was Actually TRUE POSITIVE (bugs fixed)

## Background

Multiple HUMAN beads (nd-3bo, nd-2h0, nd-1zx, nd-165) were created as worker starvation alerts. Initial analysis suggested these were false positives because the fallback mechanism appeared to work.

## Actual Root Causes

Upon deeper investigation, the worker starvation was a **TRUE POSITIVE** caused by multiple bugs:

### Bug 1: Debug Output Pollution (src/lib/output.sh)

`_needle_debug` and `_needle_verbose` were outputting to stdout instead of stderr.

**Impact:** When functions were called in subshells to capture output, the debug messages corrupted the JSON, causing parsing failures.

**Fix:** Added `>&2` redirection to send debug/verbose output to stderr.

```bash
# Before
_needle_debug() {
    _needle_print_color "$NEEDLE_COLOR_DIM" "[DEBUG] $*"
}

# After
_needle_debug() {
    _needle_print_color "$NEEDLE_COLOR_DIM" "[DEBUG] $*" >&2
}
```

### Bug 2: Workspace Not Honored in Fallback (src/bead/select.sh)

The `br list` command operates on the current directory, but the fallback wasn't changing directories when `--workspace` was provided.

**Impact:** When workers passed a workspace parameter, the fallback ran in the wrong directory and found no beads (or wrong beads).

**Fix:** Added `cd "$workspace" &&` before `br list` when workspace is provided.

```bash
if [[ -n "$workspace" && -d "$workspace" ]]; then
    candidates=$(cd "$workspace" && br list --status open --json 2>/dev/null)
else
    candidates=$(br list --status open --json 2>/dev/null)
fi
```

### Bug 3: Dependency Filter Missing (src/bead/select.sh)

The fallback filter didn't check `dependency_count`, so beads with unmet dependencies were being selected.

**Impact:** Claim attempts on beads with open dependencies always failed with "cannot claim blocked issue".

**Fix:** Added `dependency_count == 0` to the filter criteria.

```bash
filtered=$(echo "$candidates" | jq -c '
    [.[] | select(
        .assignee == null and
        .blocked_by == null and
        (.deferred_until == null or .deferred_until == "") and
        (.dependency_count == null or .dependency_count == 0) and  # NEW
        (.issue_type == null or .issue_type != "human")
    )]
')
```

### Bug 4: Claim Not Workspace-Aware (src/bead/claim.sh)

The `br update --claim` command runs in the current directory, not the specified workspace.

**Impact:** Claims failed with NOT_INITIALIZED error when run from outside the workspace.

**Fix:** Run claim in workspace directory when `--workspace` is provided.

```bash
if [[ -n "$workspace" && -d "$workspace" ]]; then
    claim_result=$(cd "$workspace" && br update "$bead_id" --claim --actor "$actor" 2>&1)
else
    claim_result=$(br update "$bead_id" --claim --actor "$actor" 2>&1)
fi
```

## Verification

Successfully claimed bead `nd-14v` from `/tmp` directory with `--workspace /home/coder/NEEDLE` parameter after fixes.

## Files Modified

- `src/lib/output.sh` (lines 74-86): Debug/verbose output to stderr
- `src/bead/select.sh` (lines 125-147): Workspace and dependency filtering
- `src/bead/claim.sh` (lines 262-276): Workspace-aware claim

## Commit

```
fix(worker): Fix multiple bugs causing worker starvation
commit 08c7173
```

## Lessons Learned

1. **Always redirect debug output to stderr** in functions whose stdout is captured
2. **Test with workspace parameters** explicitly, not just from within the workspace
3. **Validate all filter criteria** match what the primary command (br ready) does
4. **Run external CLI commands in the correct directory** when they operate on current directory

## Related Skills

- `worker-starvation-false-positive`
- `br-cli-workspace-isolation-troubleshooting`
