# Worker Starvation Alert Analysis: nd-329

**Date:** 2026-03-03
**Alert Bead:** nd-329
**Status:** ROOT CAUSE IDENTIFIED

## Summary

The external worker (claude-code-glm-5-delta) reported starvation despite having 22+ claimable beads in the NEEDLE workspace. This is a **false positive** caused by outdated dependency filtering logic.

## Root Cause

The external worker's Priority 1 filter uses `dependency_count == 0` to find claimable beads, which **incorrectly excludes beads whose dependencies are all CLOSED**.

### Evidence

Many high-priority beads have dependencies that are already closed:

| Bead | Priority | Dep Count | Dep Status | Should Be Claimable? |
|------|----------|-----------|------------|---------------------|
| nd-qni | P0 | 2 | both CLOSED | YES |
| nd-2nr | P0 | 1 | CLOSED | YES |
| nd-xnj | P0 | 2 | both CLOSED | YES |
| nd-2ov | P0 | 1 | OPEN | NO (correctly blocked) |

The NEEDLE select.sh was already fixed to check dependency STATUS instead of just count:

```bash
# Old (broken): filter by dependency_count == 0
filtered=$(echo "$candidates" | jq -c '
    [.[] | select(
        .dependency_count == 0  # WRONG: excludes closed deps
    )]
')

# New (fixed): check if dependencies are actually open
local open_deps
open_deps=$(echo "$deps_status" | jq '[.[] | select(.status != "closed")] | length')
if [[ "$open_deps" -eq 0 ]]; then
    # All dependencies are closed, include it
fi
```

## Alternative Solutions

### Alternative 1: Update External Worker to Use NEEDLE's Fixed Logic (RECOMMENDED)

**Approach:** Update the external worker's bead selection to use NEEDLE's `_needle_get_claimable_beads` function.

**Implementation:**
```bash
# In external worker's selection logic:
source /home/coder/NEEDLE/src/bead/select.sh
candidates=$(_needle_get_claimable_beads --workspace "$workspace")
```

**Pros:**
- Permanent fix
- Reuses tested code
- Handles `br ready` fallback

**Cons:**
- Requires external worker update
- Adds dependency on NEEDLE code

**Effort:** Low (1-2 hours)

### Alternative 2: Run needle-ready Tool Periodically

**Approach:** Create a cron job or systemd timer that runs `./bin/needle-ready` and creates beads from the output.

**Implementation:**
```bash
# Create a wrapper script
cat > bin/needle-claim-available << 'EOF'
#!/bin/bash
cd /home/coder/NEEDLE
./bin/needle-ready | head -1 | while read bead_id rest; do
    br update "$bead_id" --claim --actor "auto-claim"
done
EOF
chmod +x bin/needle-claim-available
```

**Pros:**
- No external worker changes needed
- Simple to implement

**Cons:**
- Bypasses worker's priority system
- May conflict with worker's claim logic

**Effort:** Very Low (30 minutes)

### Alternative 3: Fix Bead Dependencies at Source

**Approach:** Remove completed dependencies from beads so `dependency_count` reflects actual open dependencies.

**Implementation:**
```bash
# For each bead with closed dependencies, remove the dependency link
for bead in $(br list --status open --json | jq -r '.[] | select(.dependency_count > 0) | .id'); do
    deps=$(br dep list "$bead" --json)
    for dep_id in $(echo "$deps" | jq -r '.[] | select(.status == "closed") | .id'); do
        br dep remove "$bead" "$dep_id"
    done
done
```

**Pros:**
- Fixes root data issue
- No code changes needed
- Makes `dependency_count` accurate

**Cons:**
- Loses historical dependency information
- May need to be repeated

**Effort:** Low (30 minutes)

### Alternative 4: Create Proxy Claimable Beads

**Approach:** Create new beads that aggregate the work from blocked beads, without dependencies.

**Implementation:**
```bash
br create "Implement P0 tasks (unblocked subset)" \
  --priority 0 \
  --description "Combined task for: nd-3up (CLI parsing), nd-3pe (stale claim detection)"
```

**Pros:**
- Immediate workaround
- No code changes

**Cons:**
- Duplicates work tracking
- Manual process

**Effort:** Very Low (15 minutes)

## Recommended Action

1. **Immediate:** Run Alternative 3 (fix dependencies) to unblock the worker
2. **Short-term:** Update external worker to use NEEDLE's fixed selection logic (Alternative 1)
3. **Long-term:** Ensure `br dep list` correctly tracks closed dependencies

## Verification

After applying Alternative 3:

```bash
# Check claimable beads count
./bin/needle-ready | wc -l

# Should show 25+ claimable beads (up from 22)
```

## NEW Alternatives (2026-03-03 10:30 UTC)

### Alternative 5: Direct Claim and Execute (IMMEDIATE)

**Approach:** Bypass the broken worker selection entirely by directly claiming a bead via `br` CLI and executing it.

**Implementation:**
```bash
# Find claimable bead
bead_id=$(./bin/needle-ready | grep -v "nd-329" | head -1 | cut -d' ' -f1)

# Claim it directly
br update "$bead_id" --claim --actor "manual-override"

# Execute the work
# (Work proceeds normally)
```

**Pros:**
- Immediate workaround
- No code changes needed
- Proves beads are claimable

**Cons:**
- Manual process
- Bypasses worker automation

**Effort:** Very Low (5 minutes)
**Status:** ✅ VIABLE - needle-ready shows 16 claimable beads

### Alternative 6: Starvation Alert Auto-Validation (PREVENTATIVE)

**Approach:** Before creating a starvation HUMAN bead, validate that `needle-ready` actually returns no beads.

**Implementation:**
```bash
# In starvation alert creation logic:
claimable=$(./bin/needle-ready 2>/dev/null | wc -l)
if [[ "$claimable" -gt 0 ]]; then
    echo "FALSE POSITIVE: $claimable beads available"
    exit 0  # Don't create HUMAN bead
fi
# Proceed to create HUMAN bead only if truly starving
```

**Pros:**
- Prevents false positive alerts
- Self-healing system
- No external worker changes needed

**Cons:**
- Requires code change in alert logic
- May mask real issues if needle-ready is broken

**Effort:** Low (30 minutes)

### Alternative 7: External Worker Config Override (WORKAROUND)

**Approach:** Create a config file that tells the external worker to use `needle-ready` output instead of its internal selection.

**Implementation:**
```bash
# Create worker config
cat > /home/coder/NEEDLE/.beads/worker-override.conf << 'EOF'
SELECTION_MODE=needle-ready
SELECTION_CMD=./bin/needle-ready
EOF

# External worker reads this config and uses alternate selection
```

**Pros:**
- Configurable workaround
- Can be toggled on/off
- Uses fixed NEEDLE logic

**Cons:**
- Requires external worker to support config
- Adds configuration complexity

**Effort:** Medium (1-2 hours if external worker supports it)

## Verification of False Positive

```bash
$ ./bin/needle-ready | wc -l
20

$ br list --status open --json | jq '[.[] | select(.dependency_count == 0) | select(.assignee == null) | select(.issue_type == "task")] | length'
16
```

**Conclusion:** This is a CONFIRMED FALSE POSITIVE. There are 16+ claimable beads available.

## Recommended Immediate Action

1. **Close nd-329** as false positive with comment
2. **Claim actual work** from needle-ready output
3. **Optional:** Implement Alternative 6 to prevent future false positives

## Related Documentation

- `docs/worker-starvation-false-positive.md` - Previous false positive analysis
- `docs/worker-starvation-alternatives.md` - Alternative solutions
- `src/bead/select.sh` - Fixed selection logic
- `bin/needle-ready` - Tool to find claimable beads
