# Alternative Solutions for HUMAN Bead nd-pniz

## HUMAN Bead Summary

**Bead ID:** nd-pniz
**Title:** ALERT: Worker claude-code-glm-5-bravo has no work available
**Root Cause:** Worker starvation due to bug in work stealing implementation

## Root Cause Analysis

### Problem Statement
The worker reported "no work available" despite:
- 6 open beads in the database
- 4 beads assigned to "coder" (stealable)
- Work stealing enabled in config
- Claims over 15 hours old (well past 5-minute timeout)

### Investigation Findings

1. **br ready returns only HUMAN bead** - This is expected behavior; it filters for unassigned beads only.

2. **Work stealing IS implemented** - The code at lines 418-473 in `src/bead/select.sh` correctly checks for stealable claims.

3. **THE BUG: Incorrect priority flag syntax**
   - Line 422: `br list --status open --priority 0,1,2,3 --json`
   - This syntax returns **1 bead** instead of **6 beads**
   - The comma-separated value is interpreted as a single priority, not multiple

4. **Verification:**
   ```bash
   # Returns 6 beads (correct)
   br list --status open --json

   # Returns 1 bead (bug - comma syntax doesn't work)
   br list --status open --priority 0,1,2,3 --json
   ```

5. **br list priority flag expects:**
   - Repeated flags: `-p 0 -p 1 -p 2 -p 3`
   - Or range: `--priority-min 0 --priority-max 3`

---

## Alternative Solutions

### Alternative 1: Fix Priority Flag Syntax (RECOMMENDED)

**Approach:** Change `--priority 0,1,2,3` to `--priority-min 0 --priority-max 3`

**Technical Implementation:**
```bash
# In src/bead/select.sh, line 422 and 511
# BEFORE:
all_open_beads=$(br list --status open --priority 0,1,2,3 --json 2>/dev/null)

# AFTER:
all_open_beads=$(br list --status open --priority-min 0 --priority-max 3 --json 2>/dev/null)
```

**Pros:**
- Minimal change - single line fix
- Maintains priority filtering intent
- Uses proper br CLI syntax
- Works with existing br version

**Cons:**
- None identified

**Estimated Effort:** 5 minutes (one-line fix in 2 locations)

---

### Alternative 2: Remove Priority Filter Entirely

**Approach:** Remove the `--priority` flag entirely since we're filtering client-side anyway

**Technical Implementation:**
```bash
# In src/bead/select.sh, lines 422 and 511
# BEFORE:
all_open_beads=$(br list --status open --priority 0,1,2,3 --json 2>/dev/null)

# AFTER:
all_open_beads=$(br list --status open --json 2>/dev/null)
```

**Pros:**
- Simpler code
- No risk of future br CLI syntax changes
- All filtering happens client-side with jq anyway

**Cons:**
- Slightly more data transferred (but negligible for small bead counts)
- May include P4+ beads that would be filtered later

**Estimated Effort:** 5 minutes (one-line fix in 2 locations)

---

### Alternative 3: Use Repeated -p Flags

**Approach:** Use repeated `-p` flags which br CLI supports

**Technical Implementation:**
```bash
# In src/bead/select.sh, lines 422 and 511
# BEFORE:
all_open_beads=$(br list --status open --priority 0,1,2,3 --json 2>/dev/null)

# AFTER:
all_open_beads=$(br list --status open -p 0 -p 1 -p 2 -p 3 --json 2>/dev/null)
```

**Pros:**
- Explicit priority filtering
- Uses documented br CLI syntax

**Cons:**
- More verbose
- Harder to maintain priority ranges

**Estimated Effort:** 5 minutes (one-line fix in 2 locations)

---

## Recommendation

**Implement Alternative 1** (Fix Priority Flag Syntax) because:
1. It's the minimal change that fixes the bug
2. It maintains the original intent of filtering by priority
3. It uses the proper br CLI syntax (`--priority-min/max`)
4. It's the most maintainable solution

---

## Implementation Steps

1. Edit `src/bead/select.sh` line 422:
   ```bash
   all_open_beads=$(br list --status open --priority-min 0 --priority-max 3 --json 2>/dev/null)
   ```

2. Edit `src/bead/select.sh` line 511 (fallback path):
   ```bash
   candidates=$(cd "$workspace" && br list --status open --priority-min 0 --priority-max 3 --json 2>/dev/null)
   ```

3. Test the fix:
   ```bash
   source src/bead/select.sh && _needle_get_claimable_beads | jq 'length'
   # Should return 4 (the stealable beads)
   ```

4. Verify workers can now find work

---

## Verification Commands

```bash
# After fix, verify work stealing finds beads:
source src/bead/select.sh
_needle_get_claimable_beads | jq -c '.[].id'

# Expected output:
# "nd-32x"
# "nd-3jf"
# "nd-2pw"
# "nd-33b"
```

---

## Related Beads

- nd-32x: Fix external worker discovery mechanism (can now be claimed)
- nd-3jf: Update external worker dependencies (can now be claimed)
- nd-1md: Alternative work stealing (related but different issue)

---

*Analysis completed: 2026-03-04*
*Worker: claude-code-glm-5-bravo*
