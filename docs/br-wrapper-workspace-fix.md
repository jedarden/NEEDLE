# BR Wrapper Workspace Detection Fix

**Date:** 2026-03-03
**Issue:** Worker Starvation False Positive (nd-2nl)

## Problem

Workers in the NEEDLE workspace reported starvation despite having 43+ open beads. The `br ready` command was returning beads from the wrong workspace.

## Root Cause

The `br` wrapper script at `/home/coder/.local/bin/br` had a hardcoded path for the ready-queue workaround:

```bash
READY_QUEUE="/home/coder/FABRIC/.beads/ready-queue.json"
```

This caused `br ready` to return `bd-*` beads from the FABRIC workspace instead of `nd-*` beads from the NEEDLE workspace.

## Fix Applied

Updated the wrapper script to detect the current workspace dynamically:

```bash
# Find workspace root by walking up from current directory
_find_workspace_root() {
    local dir="$PWD"
    while [[ "$dir" != "/" ]]; do
        if [[ -d "$dir/.beads" ]]; then
            echo "$dir"
            return 0
        fi
        dir="$(dirname "$dir")"
    done
    return 1
}

WORKSPACE_ROOT=$(_find_workspace_root)
if [[ -n "$WORKSPACE_ROOT" ]]; then
    READY_QUEUE="$WORKSPACE_ROOT/.beads/ready-queue.json"
else
    READY_QUEUE="/home/coder/FABRIC/.beads/ready-queue.json"
fi
```

## Verification

```bash
# Before fix: bd-* beads from FABRIC
$ br ready --json | jq '.[0].id'
"bd-2zt"

# After fix: nd-* beads from NEEDLE  
$ br ready --json | jq '.[0].id'
"nd-qni"
```

## Impact

- Workers in NEEDLE workspace now see correct beads
- Workers in other workspaces will also get correct beads
- No changes needed to worker code

## Files Modified

- `/home/coder/.local/bin/br` (lines 11-29)

## Related Issues

- nd-2nl: ALERT: Worker claude-code-glm-5-charlie has no work available
- nd-373: Fix br ready schema error
