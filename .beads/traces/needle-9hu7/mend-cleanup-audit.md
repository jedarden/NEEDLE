# Mend Strand Cleanup Audit Report

**Bead:** needle-9hu7
**Date:** 2026-04-25
**Scope:** Review `src/strand/mend.rs` for completeness of cleanup responsibilities

## Executive Summary

The mend strand handles **most** state cleanup responsibilities well. Key gaps identified:

1. **Rate limiter state cleanup** — NOT IMPLEMENTED (high priority)
2. **Dead PID lock file release** — NOT IMPLEMENTED (medium priority)
3. **Idle worker detection** — NOT IMPLEMENTED (low priority)
4. **Agent log early cleanup** — PARTIAL (edge case)
5. **Telemetry coverage** — GAPS in error paths

## Detailed Findings by Category

### 1. Registry Cleanup ✅ (Implemented)

| Scenario | Status | Implementation |
|----------|--------|----------------|
| Dead workers (PID gone) deregistered | ✅ YES | `cleanup_dead_workers()` lines 289-362 |
| Workers with no heartbeat + dead PID | ✅ YES | Covered by PID check above |
| Workers > idle_timeout with 0 beads | ❌ NO | Not tracked in registry |

**Notes:** The registry already filters dead PIDs in `list()` and persists cleanup. Mend's `cleanup_dead_workers()` provides proactive cleanup every cycle.

**Gap:** No detection of "zombie" workers that registered but never processed any beads (stuck at 0 beads_processed for extended time).

---

### 2. Heartbeat Cleanup ✅ (Mostly Implemented)

| Scenario | Status | Implementation |
|----------|--------|----------------|
| Stale heartbeats for dead PIDs | ✅ PARTIAL | PeerMonitor removes during crash cleanup |
| Orphaned heartbeats (no registry entry) | ✅ YES | `cleanup_orphaned_heartbeats()` lines 364-459 |

**Notes:** 
- PeerMonitor (in `peer/mod.rs`) removes heartbeats for crashed workers as part of `handle_crashed_peer()`
- Mend's `cleanup_orphaned_heartbeats()` handles the case where heartbeat files exist without registry entries
- Both check staleness before removal to avoid races

**Gap:** Direct cleanup in mend for stale heartbeats with dead PIDs (not via peer monitoring path) is not implemented, but this is acceptable since PeerMonitor handles it.

---

### 3. Rate Limiter State Cleanup ❌ (NOT IMPLEMENTED)

| Scenario | Status | Impact |
|----------|--------|--------|
| Stale `last_refill` timestamps reset | ❌ NO | Old buckets may artificially limit |
| Dead PID reconciliation | ❌ NO | Not applicable (RPM is provider-scoped) |
| Orphaned state file removal | ❌ NO | Files accumulate forever |

**Location:** `~/.needle/state/rate_limits/{provider}.json`

**Structure (from `src/rate_limit/mod.rs`):**
```rust
struct TokenBucket {
    tokens: f64,
    capacity: u32,
    last_refill: DateTime<Utc>,  // ← Can become very stale
}
```

**Problems:**
1. If a provider's last refill was days/weeks ago, the bucket may have near-zero tokens
2. Token refill calculates elapsed time from `last_refill` — if very old, tokens are refilled to capacity, which is correct behavior
3. State files for unused providers accumulate forever
4. No cleanup of state files for providers no longer in config

**Recommendation:** Add rate limiter state cleanup to mend:
- Remove state files for providers not in current config
- Optionally reset buckets with very old `last_refill` (though this self-corrects on next request)

**Follow-up bead needed:** Rate limiter state cleanup implementation

---

### 4. Lock File Cleanup ⚠️ (Partially Implemented)

| Scenario | Status | Implementation |
|----------|--------|----------------|
| Stale lock files (age > timeout) | ✅ YES | `cleanup_orphaned_locks()` lines 461-554 |
| Dead PID lock release (immediate) | ❌ NO | Only age-based cleanup |

**Current behavior:**
- Uses `lock_ttl_secs` from config (default: 300 seconds)
- Probes lock with `try_acquire_flock()` — if acquired, removes the file
- Does NOT check if lock holder's PID is dead

**Gap:** If a worker dies while holding a claim lock, the lock file persists until timeout (5 minutes by default). During this window, the bead's lock file exists even though no live process holds it, potentially causing confusion.

**Note:** This is relatively benign because:
1. Claim locks use flock — if the holder dies, the lock is released automatically by the kernel
2. Mend's probe (`try_acquire_flock()`) will succeed and clean up the file
3. The file is just metadata; the actual flock is gone

**Recommendation:** Add immediate cleanup for lock files whose holder PID is dead (parse PID from filename or check all locks).

**Follow-up bead needed:** Dead PID lock file immediate release

---

### 5. Log File Cleanup ✅ (Implemented)

| Scenario | Status | Implementation |
|----------|--------|----------------|
| Logs older than `retention_days` | ✅ YES | `cleanup_old_agent_logs()` lines 622-730 |
| Logs from workers with 0 beads | ⚠️ PARTIAL | Only age-based cleanup |

**Current behavior:**
- Deletes `.agent.jsonl` files older than `retention_days`
- Skips logs for in-progress beads
- No special handling for workers that never processed beads

**Gap:** If a worker crashes immediately (before processing any beads), its log file (if any) will persist for `retention_days`. This is minor — the log is likely empty or minimal.

---

### 6. Bead State Cleanup ✅ (Implemented)

| Scenario | Status | Implementation |
|----------|--------|----------------|
| In-progress beads with dead assignee | ✅ YES | `cleanup_orphaned_in_progress()` lines 49-121 |
| Dependency links to closed beads | ✅ YES | `cleanup_stale_dependencies()` lines 556-620 |
| Peer monitoring crash cleanup | ✅ YES | `PeerMonitor::handle_crashed_peer()` in peer/mod.rs |

**Notes:** Well-covered across multiple modules. The qualified ID collision handling is comprehensive.

---

### 7. Telemetry Coverage ⚠️ (Mostly Complete)

| Action | Telemetry Event | Status |
|--------|-----------------|--------|
| Bead released | `BeadReleased`, `StuckReleased` | ✅ |
| Lock removed | `MendOrphanedLockRemoved` | ✅ |
| Dependency cleaned | `MendDependencyCleaned` | ✅ |
| Worker deregistered | `MendWorkerDeregistered` | ✅ |
| Heartbeat removed | `MendOrphanedHeartbeatRemoved` | ✅ |
| DB repaired | `MendDbRepaired` | ✅ |
| DB rebuilt | `MendDbRebuilt` | ✅ |
| Trace cleanup | `MendTraceCleanup` | ✅ |
| Log cleanup | `MendCycleSummary` includes count | ✅ |
| **Rate limit cleanup** | `MendRateLimitCleaned` | ⚠️ Event defined but not emitted (cleanup not implemented) |
| Error on lock remove | Warning log only | ⚠️ No telemetry |

**Gap:** Several failure paths log warnings but don't emit telemetry:
- `cleanup_orphaned_locks()`: lock file removal failure → warn only
- `cleanup_orphaned_in_progress()`: release failure → warn only
- `cleanup_stale_dependencies()`: dependency removal failure → warn only

**Note:** These are non-fatal failures (the operation is best-effort), but telemetry would aid observability.

---

## Root Cause Analysis: workers.json Deadlock

The recent workers.json deadlock was caused by:

1. **Not the root cause:** Registry cleanup is well-implemented
2. **Actual issue:** The `list()` method uses shared lock → read → check PIDs → unlock → write_cleaned() (no lock). The write_cleaned can race with other writers.

3. **Mend's role:** Mend's `cleanup_dead_workers()` directly reads the raw file and deregisters entries, which is safer.

**Mend correctly handles:** Dead worker deregistration, orphaned bead release, heartbeat cleanup.

**What caused the deadlock:** Likely a file system or flock edge case, not a gap in mend's cleanup responsibilities.

---

## Recommendations

### High Priority
1. **Implement rate limiter state cleanup** — Remove orphaned/old `rate_limits/*.json` files
2. **Add telemetry for error paths** — Emit events when cleanup operations fail

### Medium Priority
3. **Dead PID lock release** — Check lock holder PID and release immediately if dead
4. **Idle worker detection** — Flag workers with `beads_processed == 0` for > `idle_timeout`

### Low Priority
5. **Agent log early cleanup** — Delete logs from workers that processed 0 beads (minor)

---

## Conclusion

The mend strand is **substantially complete** for its core responsibilities. The main gap is rate limiter state cleanup, which is a new category of state not previously considered. The workers.json deadlock was not caused by missing cleanup in mend.

**Next steps:**
1. Create follow-up bead for rate limiter state cleanup
2. Create follow-up bead for dead PID lock file release
3. Consider telemetry enhancement bead for error path observability
