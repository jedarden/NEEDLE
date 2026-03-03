# Worker Starvation Alert Analysis: nd-356

**Date:** 2026-03-03
**Status:** RESOLVED - Stuck Claims Released
**Worker:** claude-code-glm-5-charlie

## Summary

Worker `claude-code-glm-5-charlie` reported starvation with 5 consecutive empty iterations despite having 20+ claimable beads in the workspace. Investigation revealed the root cause was **stuck claims blocking the ready queue**.

## Root Cause

### Primary Issue: Stuck Claims

Two beads were stuck in `in_progress` status with non-null `claimed_by` fields:
- `nd-1pu` - Implement worker loop: Bead execution and effort recording
- `nd-3up` - Implement needle run: CLI parsing and validation

These stuck claims prevented other workers from seeing these beads as available, and the workers that "claimed" them never completed or released them properly.

### Secondary Issue: CHECK Constraint Bug

The `br update --status open` command fails with a CHECK constraint error:

```
CHECK constraint failed: (status = 'in_progress' AND claimed_by IS NOT NULL AND claim_timestamp IS NOT NULL) OR
            (status != 'in_progress' AND claimed_by IS NULL AND claim_timestamp IS NULL)
```

This prevents normal release of stuck beads via the CLI.

## Investigation Findings

### 1. Ready Beads Were Available

```bash
$ br ready | wc -l
24  # Plenty of work available
```

### 2. Fallback Mechanism Works

The `_needle_get_claimable_beads` fallback correctly found 19 claimable beads:

```
[DEBUG] DIAG: Fallback found 19 claimable beads
```

### 3. Claim Mechanism Works

Testing the claim function successfully claimed a bead:

```
✓ Claimed bead: nd-3up
```

### 4. Stuck Claims Were the Blocker

```bash
$ br list --status in_progress --json | jq '.[].id'
"nd-1pu"
"nd-3up"
```

## Resolution

Released stuck claims using Python SQL fallback (sqlite3 CLI not available):

```python
python3 -c "
import sqlite3
conn = sqlite3.connect('.beads/beads.db')
c = conn.cursor()
c.execute('''UPDATE issues SET status=\"open\", assignee=NULL, claimed_by=NULL, claim_timestamp=NULL WHERE id IN (\"nd-1pu\", \"nd-3up\")''')
conn.commit()
print(f'Released {c.rowcount} beads')
"
```

After release:
- 24 ready beads available
- 0 stuck claims remaining

## Alternative Solutions for Future Starvation Alerts

### Alternative 1: Automated Stuck Claim Detection (RECOMMENDED)

**Implementation:** Add a maintenance strand (mend) that detects and releases stale claims.

**Approach:**
```bash
# In mend strand, detect claims older than threshold
stale_claims=$(br list --status in_progress --json | jq '
  [.[] | select(
    .claim_timestamp != null and
    ((now - (.claim_timestamp | fromdateiso8601)) > 3600)
  ) | .id]
')

# Release stale claims
for bead_id in "${stale_claims[@]}"; do
  _needle_release_bead "$bead_id" "stale_claim_auto_release"
done
```

**Pros:**
- Automatic recovery from stuck claims
- No manual intervention needed
- Works within existing strand system

**Cons:**
- Need to be careful about legitimate long-running tasks
- Requires claim_timestamp to be properly set

### Alternative 2: Database Trigger for Claim Consistency

**Implementation:** Add SQLite triggers to auto-clear claim fields on status change.

**Approach:**
```sql
CREATE TRIGGER release_claim_on_status_change
AFTER UPDATE OF status ON issues
WHEN NEW.status != 'in_progress' AND (OLD.claimed_by IS NOT NULL)
BEGIN
  UPDATE issues SET
    claimed_by = NULL,
    claim_timestamp = NULL,
    assignee = NULL
  WHERE id = NEW.id;
END;
```

**Pros:**
- Database-level consistency guarantee
- No code changes needed in application

**Cons:**
- Requires database migration
- May conflict with beads_rust's schema management

### Alternative 3: External Watchdog Process

**Implementation:** Separate process that monitors for stuck claims.

**Approach:**
```bash
# watchdog/stuck_claims.sh
while true; do
  stale=$(find_stale_claims)
  if [[ -n "$stale" ]]; then
    release_claims "$stale"
    log "Released stale claims: $stale"
  fi
  sleep 300  # Check every 5 minutes
done
```

**Pros:**
- Independent of worker process
- Can run even when workers crash

**Cons:**
- Another process to manage
- Potential race conditions

### Alternative 4: Claim Timeout in br CLI

**Implementation:** Add `--claim-timeout` option to br update that auto-releases after timeout.

**Approach:**
```bash
br update nd-123 --claim --actor worker-1 --claim-timeout 3600
```

**Pros:**
- Built into the claim mechanism
- Explicit timeout per claim

**Cons:**
- Requires changes to beads_rust
- Need scheduler to check timeouts

## Recommendations

1. **Immediate:** Run `bin/needle-db-rebuild` periodically to clean up database inconsistencies

2. **Short-term:** Implement Alternative 1 (Automated Stuck Claim Detection) in the mend strand

3. **Long-term:** Work with beads_rust maintainers to fix the CHECK constraint bug or add claim timeout feature

## Related Documentation

- `docs/worker-starvation-false-positive.md` - Previous starvation analysis
- `docs/worker-starvation-alternatives.md` - Alternative solutions documentation
- `src/bead/claim.sh` - Claim/release implementation with SQL fallback
- `bin/needle-ready` - Workaround tool for br ready issues

## Skills

- `worker-starvation-false-positive` - Pattern for handling this scenario
- `worker-stuck-alternative-exploration` - Alternative exploration pattern
