# Alternative Solutions for nd-3qxa: Worker Starvation Alert

## HUMAN Bead Analysis

**Bead ID:** nd-3qxa
**Title:** ALERT: Worker claude-code-glm-5-bravo has no work available
**Type:** human

## Root Cause Analysis

### Investigation Findings

1. **`br ready` shows 20 beads available** - Work exists
2. **All non-human beads are assigned to "coder"** (a human)
3. **Work stealing timeout is 30 minutes** - Workers can't steal recently assigned beads
4. **The only unassigned non-human bead (nd-2q6) is blocked** by dependency nd-bqi
5. **Workers correctly exclude `type: human` beads** from claiming

### The Real Problem

This is a **work distribution issue**, not a worker bug:

```
┌─────────────────────────────────────────────────────────────────┐
│                    BEAD DISTRIBUTION                            │
├─────────────────────────────────────────────────────────────────┤
│  Unassigned, non-human, non-blocked:  0 beads  ← Workers need  │
│  Assigned to "coder" (human):        18 beads  ← Stealable     │
│  Assigned to other workers:           1 bead   ← Stealable     │
│  Unassigned but type=human:           1 bead   ← Excluded      │
│  Blocked by dependencies:             1 bead   ← Not ready     │
└─────────────────────────────────────────────────────────────────┘
```

Workers can only claim:
- Unassigned beads (none available except human type)
- Assigned beads after 30-minute work stealing timeout

## Alternative Solutions

### Alternative 1: Reduce Work Stealing Timeout (RECOMMENDED)

**Approach:** Lower the work stealing timeout from 30 minutes to 5 minutes for human-assigned beads.

**Implementation:**
```yaml
# In .beads/config.yaml
select:
  work_stealing_timeout: 300  # 5 minutes
  stealable_assignees: ["coder"]  # Humans whose beads can be stolen
```

**Pros:**
- Quick fix, no code changes needed
- Workers can pick up "stale" human assignments faster
- Respects existing work stealing logic

**Cons:**
- May interrupt human work in progress
- Doesn't address root cause (beads assigned to humans by default)

**Estimated Effort:** 5 minutes (config change only)

---

### Alternative 2: Unassign Beads By Default

**Approach:** Change bead creation to leave beads unassigned by default. Workers claim them as needed.

**Implementation:**
- Modify `br create` to default to unassigned
- Workers claim beads atomically when starting work
- Humans can still self-assign if needed

**Pros:**
- Clean solution - workers find work immediately
- Matches standard task queue patterns
- No work stealing complexity

**Cons:**
- Requires changing bead creation workflow
- May affect human workflow (manual assignment needed)
- Cultural change for project

**Estimated Effort:** 30 minutes (script changes)

---

### Alternative 3: Implement "Soft Assignment" Concept

**Approach:** Introduce assignment types: "hard" (exclusive) and "soft" (stealable immediately).

**Implementation:**
```bash
br create "Task" --assignee coder --assignment-type soft
# Workers can claim soft-assigned beads immediately
# Hard assignments require work stealing timeout
```

**Pros:**
- Granular control over assignment behavior
- Backwards compatible
- Supports both workflows

**Cons:**
- More complex mental model
- Requires `br` changes
- Additional configuration

**Estimated Effort:** 2 hours (requires br changes)

---

### Alternative 4: Add "Priority Boost" for Idle Workers

**Approach:** When a worker is idle for N iterations, temporarily reduce work stealing timeout.

**Implementation:**
```bash
# In worker loop
if (( consecutive_empty_iterations > 3 )); then
    export NEEDLE_WORK_STEALING_TIMEOUT=60  # 1 minute
fi
```

**Pros:**
- Self-adjusting based on workload
- No permanent config changes
- Workers become more aggressive when starving

**Cons:**
- Adds complexity to worker loop
- May cause "thundering herd" when multiple workers idle
- Doesn't address root cause

**Estimated Effort:** 1 hour

---

### Alternative 5: Create Unassigned "Ready" Beads

**Approach:** Immediately create some unassigned beads for workers to claim.

**Implementation:**
```bash
# Release some assigned beads back to unassigned
br update nd-2ov --release  # Unassign so workers can claim
br update nd-xnj --release
```

**Pros:**
- Immediate fix
- No code/config changes
- Provides work for idle workers

**Cons:**
- Manual intervention required
- Temporary fix
- May lose assignment context

**Estimated Effort:** 2 minutes (immediate)

---

## Recommended Solution

**Implement Alternative 5 immediately + Alternative 1 for long-term:**

1. **Immediate (Alt 5):** Release a few P0/P1 beads from "coder" assignment
2. **Long-term (Alt 1):** Reduce work stealing timeout to 5 minutes

This provides:
- Immediate work for idle workers
- Sustainable work distribution going forward
- No code changes required

## Implementation

### Step 1: Release High-Priority Beads

```bash
# Release P0 beads for workers to claim
br update nd-2ov --release --actor admin
br update nd-xnj --release --actor admin
br update nd-2gc --release --actor admin
```

### Step 2: Update Work Stealing Config

```yaml
# In .beads/config.yaml
select:
  work_stealing_enabled: true
  work_stealing_timeout: 300  # 5 minutes instead of 30
  stealable_assignees: ["coder"]
```

### Step 3: Verify Fix

```bash
# Check workers can now find work
br ready
# Should show released beads as unassigned
```

## Conclusion

The worker starvation alert is a **false positive** caused by work distribution patterns, not a bug. The worker correctly:
- Excludes human-type beads
- Respects work stealing timeout
- Follows dependency blocking rules

The fix is to adjust work distribution policy, not worker logic.
