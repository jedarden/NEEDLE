# Alternative Solutions for Worker Starvation False Alarms

**Analysis Date:** 2026-03-04
**HUMAN Bead:** nd-2iw (Worker claude-code-glm-5-bravo starvation alert)
**Diagnosis:** UNEXPECTED STARVATION (FALSE ALARM)

## Root Cause Analysis

### Evidence of False Alarm
- **Current State:** 25 beads ready to work (including P0 and P1 tasks)
- **Project Completion:** 72% (85/118) - NOT complete
- **Database Corruption:** `beads.db.corrupted-20260304-035203` (1.0M) exists
- **WAL File Size:** 48MB (abnormally large - should be <1MB)
- **Pattern:** 4 recent false alarms closed (nd-6hc, nd-6qd, nd-2hf, nd-2jp)

### Worker Reported vs Reality
```
Worker: "No beads in /home/coder/NEEDLE or subfolders"
Reality: 25 ready beads exist, including:
  - nd-xnj (P0): Implement worker naming module
  - nd-2gc (P0): Implement Strand 1: Pluck
  - nd-qni (P0): Implement worker loop
  - nd-2ov (P0): Implement needle run
  ... and 21 more
```

### Systemic Issue
This is the **5th worker starvation false alarm** despite multiple attempted fixes:
- ✗ Starvation alert verification improvements (commit 6eadb59)
- ✗ False positive detection (commit 9252f92)
- ✗ Worker starvation bug fixes (commit 562d2ba)
- ✗ Discovery mechanism fixes (commit e6aa8ee)
- ✗ Multiple `br ready` fallback improvements

**Conclusion:** The worker query logic is fundamentally broken when database corruption occurs.

---

## Alternative Solutions

### Alternative 1: Database Health Check Before Starvation Alert ⭐ RECOMMENDED

**Technical Approach:**
Add pre-flight checks before creating starvation alerts:

1. **WAL Size Check:** If `beads.db-wal > 10MB`, database is suspect
2. **Query Verification:** Run `br ready` to verify worker's claim of "no work"
3. **Auto-Repair:** If corruption detected, rebuild database from `issues.jsonl`
4. **Conditional Alert:** Only create HUMAN alert if health checks pass

**Implementation:**
```bash
# In worker's starvation detection logic (Priority 6)
check_database_health() {
    local wal_size=$(stat -f%z .beads/beads.db-wal 2>/dev/null || stat -c%s .beads/beads.db-wal)

    # WAL file should be small (<1MB normally, <10MB max)
    if [ "$wal_size" -gt 10485760 ]; then
        echo "⚠️ Database corruption detected (WAL: ${wal_size} bytes)"
        return 1
    fi

    # Verify no ready beads exist
    local ready_count=$(br ready --format json | jq 'length')
    if [ "$ready_count" -gt 0 ]; then
        echo "⚠️ False alarm: ${ready_count} beads ready to work"
        return 1
    fi

    return 0
}

# Before creating starvation alert
if ! check_database_health; then
    echo "Database health check failed - rebuilding..."
    rebuild_database_from_jsonl
    exit 0  # Don't create starvation alert
fi
```

**Pros:**
- Prevents false alarms at the source
- Automatically repairs corruption
- No human intervention needed
- Addresses root cause

**Cons:**
- Requires modifying worker's Priority 6 logic
- Adds ~1-2 seconds to starvation detection

**Effort:** 2-3 hours (modify worker starvation detection, add health checks, test)

---

### Alternative 2: Auto-Close False Alarm Detection ⭐ IMMEDIATE FIX

**Technical Approach:**
Create a monitoring hook that auto-closes false alarm starvation alerts:

1. **Hook Trigger:** On HUMAN bead creation matching "Worker.*has no work available"
2. **Verification:** Run `br ready` to check for available beads
3. **Auto-Close:** If beads exist, close as false alarm
4. **Database Repair:** Trigger rebuild if corruption detected

**Implementation:**
```bash
# Create .beads/hooks/on-human-created.sh
#!/bin/bash

BEAD_ID=$1
BEAD_TITLE=$2

# Only check worker starvation alerts
if [[ ! "$BEAD_TITLE" =~ "has no work available" ]]; then
    exit 0
fi

echo "🔍 Verifying worker starvation alert: $BEAD_ID"

# Check for ready beads
READY_COUNT=$(br ready --format json | jq 'length')

if [ "$READY_COUNT" -gt 0 ]; then
    echo "⚠️ FALSE ALARM: $READY_COUNT beads ready to work!"

    # Check database health
    WAL_SIZE=$(stat -c%s .beads/beads.db-wal 2>/dev/null || echo "0")
    if [ "$WAL_SIZE" -gt 10485760 ]; then
        CAUSE="Database corruption detected (WAL: $WAL_SIZE bytes)"

        # Backup and rebuild
        mv .beads/beads.db .beads/beads.db.corrupted-$(date +%Y%m%d-%H%M%S)
        br rebuild  # Rebuild from issues.jsonl
    else
        CAUSE="Worker query mismatch (unknown)"
    fi

    # Close as false alarm
    br comment $BEAD_ID "**FALSE ALARM DETECTED**

Verification shows **$READY_COUNT beads ready to work**.

**Root Cause:** $CAUSE

**Ready Beads:**
\`\`\`
$(br ready | head -10)
\`\`\`

Auto-closing as false alarm."

    br close $BEAD_ID

    echo "✅ Closed $BEAD_ID as false alarm"
fi
```

**Pros:**
- ✅ Immediate deployment (no worker modification)
- ✅ Auto-repairs database corruption
- ✅ Documents false alarm cause
- ✅ Zero human intervention

**Cons:**
- Reactive (alert still created, then closed)
- Doesn't fix worker query logic

**Effort:** 30 minutes (create hook, test, deploy)

---

### Alternative 3: Database Rebuild from JSONL on WAL Anomaly

**Technical Approach:**
Proactive database maintenance to prevent corruption from affecting workers:

1. **Cron Job:** Every 30 minutes, check `beads.db-wal` size
2. **Threshold:** If WAL > 10MB, trigger rebuild
3. **Rebuild:** Backup current DB, rebuild from `issues.jsonl`
4. **Notification:** Create INFO bead documenting rebuild

**Implementation:**
```bash
# Create .beads/maintenance/db-health-check.sh
#!/bin/bash

WAL_FILE=".beads/beads.db-wal"
WAL_SIZE=$(stat -c%s "$WAL_FILE" 2>/dev/null || echo "0")
THRESHOLD=10485760  # 10MB

if [ "$WAL_SIZE" -gt "$THRESHOLD" ]; then
    echo "⚠️ WAL file too large: $WAL_SIZE bytes (threshold: $THRESHOLD)"

    # Backup corrupted database
    BACKUP=".beads/beads.db.corrupted-$(date +%Y%m%d-%H%M%S)"
    mv .beads/beads.db "$BACKUP"
    echo "💾 Backed up corrupted DB to $BACKUP"

    # Rebuild from JSONL
    echo "🔧 Rebuilding database from issues.jsonl..."
    br rebuild

    echo "✅ Database rebuilt successfully"

    # Create notification bead
    br create "Database corruption repaired automatically" \
        --description "WAL file exceeded 10MB ($WAL_SIZE bytes). Database rebuilt from issues.jsonl source of truth." \
        --type info \
        --priority 4
fi
```

**Cron Setup:**
```bash
# Add to worker startup or system cron
*/30 * * * * cd /home/coder/NEEDLE && .beads/maintenance/db-health-check.sh
```

**Pros:**
- Proactive prevention
- Maintains database health automatically
- Prevents cascading failures

**Cons:**
- Doesn't prevent initial corruption
- Adds system complexity (cron job)
- May rebuild unnecessarily if WAL grows during heavy activity

**Effort:** 1 hour (create script, test rebuild, setup cron)

---

### Alternative 4: Worker Query Validation Layer

**Technical Approach:**
Add validation to ensure worker's internal query matches `br ready` results:

1. **Dual Query:** Worker runs both internal query AND `br ready`
2. **Comparison:** If results differ, log discrepancy
3. **Fallback:** Use `br ready` result as source of truth
4. **Alert Suppression:** Don't create starvation alert if `br ready` shows work

**Implementation:**
```bash
# In worker's Priority 1 (Local workspace) logic
claim_local_work() {
    # Current internal query
    local internal_result=$(query_local_beads)

    # Validation query (source of truth)
    local validation_result=$(br ready --workspace "$WORKSPACE_PATH" --format json)
    local ready_count=$(echo "$validation_result" | jq 'length')

    # Compare results
    if [ -z "$internal_result" ] && [ "$ready_count" -gt 0 ]; then
        echo "⚠️ QUERY MISMATCH: Internal query found 0 beads, but br ready found $ready_count"
        echo "Using br ready result as source of truth"

        # Log for debugging
        br create "Worker query mismatch detected" \
            --description "Worker internal query returned 0 beads, but \`br ready\` found $ready_count beads. Using br ready fallback." \
            --type bug \
            --priority 1

        # Use validation result
        internal_result="$validation_result"
    fi

    # Continue with claim logic...
}
```

**Pros:**
- Prevents false starvation at source
- Logs query discrepancies for debugging
- Provides fallback to known-good query

**Cons:**
- Doubles query overhead (performance impact)
- Doesn't fix underlying query bug
- Bandaid solution

**Effort:** 2 hours (modify worker logic, add validation layer, test)

---

## Recommended Implementation Plan

### Phase 1: Immediate Fix (Alternative 2) - 30 minutes ⭐ DEPLOY NOW

**Why:** Stops false alarms immediately without worker modification

```bash
# 1. Create auto-close hook
cat > .beads/hooks/on-human-created.sh << 'EOF'
[Alternative 2 script above]
EOF
chmod +x .beads/hooks/on-human-created.sh

# 2. Test with current false alarm
.beads/hooks/on-human-created.sh nd-2iw "ALERT: Worker claude-code-glm-5-bravo has no work available"

# 3. Verify it auto-closes
br show nd-2iw  # Should be CLOSED
```

### Phase 2: Proactive Prevention (Alternative 3) - 1 hour

**Why:** Prevents database corruption from accumulating

```bash
# 1. Create maintenance script
mkdir -p .beads/maintenance
cat > .beads/maintenance/db-health-check.sh << 'EOF'
[Alternative 3 script above]
EOF
chmod +x .beads/maintenance/db-health-check.sh

# 2. Run immediately to fix current corruption
.beads/maintenance/db-health-check.sh

# 3. Schedule periodic checks
# Add to worker startup or cron
```

### Phase 3: Root Cause Fix (Alternative 1) - 2-3 hours

**Why:** Fixes the underlying worker logic issue

```bash
# 1. Add health check to worker's Priority 6
# Modify strands/weave.sh or equivalent starvation detection

# 2. Test with artificial corruption
# 3. Verify starvation alerts only created when legitimate
```

---

## Success Criteria

- [ ] nd-2iw closed as false alarm
- [ ] Database rebuilt from JSONL (if corrupted)
- [ ] Auto-close hook deployed and tested
- [ ] No new false alarm starvation alerts in next 24 hours
- [ ] Database health monitoring in place

---

## Testing Plan

### Test 1: Verify False Alarm Detection
```bash
# Run auto-close hook on nd-2iw
.beads/hooks/on-human-created.sh nd-2iw "ALERT: Worker claude-code-glm-5-bravo has no work available"

# Expected: Bead closes with comment documenting 25 ready beads
br show nd-2iw | grep "FALSE ALARM"
```

### Test 2: Verify Database Rebuild
```bash
# Check current WAL size
stat -c%s .beads/beads.db-wal

# Run maintenance script
.beads/maintenance/db-health-check.sh

# Expected: If WAL > 10MB, database rebuilt
```

### Test 3: Verify No More False Alarms
```bash
# Run worker for 1 hour
# Monitor for new starvation alerts
# Expected: No false alarms created
```

---

## Implementation Status

**Current Phase:** Planning
**Next Action:** Deploy Alternative 2 (Auto-Close Hook)
**ETA:** 30 minutes
**Assignee:** claude-code-sonnet-alpha

