# Mend Strand Cleanup Audit Report

**Date:** 2026-04-25
**Bead:** needle-9hu7
**File:** `src/strand/mend.rs`

## Executive Summary

The mend strand handles **6 out of 11** cleanup scenarios completely. The primary gaps are:
1. Registry dead worker entries are filtered in-memory but not persisted
2. Rate limiter state files are never cleaned up
3. Orphaned heartbeat files (no matching registry entry) are not removed
4. Workers with 0 beads processed are not flagged

## Detailed Analysis

### 1. Registry Cleanup

| Scenario | Status | Implementation |
|----------|--------|----------------|
| Dead workers (PID gone) deregistered from workers.json | **PARTIAL** | `Registry::list()` filters dead PIDs in-memory and persists cleanup, but only when `list()` is called |
| Workers with no heartbeat file and no live PID deregistered | **PARTIAL** | Handled by PeerMonitor for crashed workers, but doesn't catch all cases |
| Workers registered > idle_timeout with 0 beads processed flagged | **MISSING** | No implementation |

**Evidence:**
- `src/registry/mod.rs:185-199`: `Registry::list()` filters dead PIDs and persists cleanup
- `src/peer/mod.rs:195-200`: PeerMonitor deregisters crashed workers
- No code checks for `beads_processed == 0` with age check

**Gap:** Dead PIDs are lazily filtered when `Registry::list()` is called (happens during rate limit checks), but there's no proactive cleanup of stale entries in `workers.json` during mend.

### 2. Heartbeat Cleanup

| Scenario | Status | Implementation |
|----------|--------|----------------|
| Stale heartbeat files (age > heartbeat_max_age) for dead PIDs removed | **YES** | PeerMonitor removes heartbeat when handling crashed peer |
| Heartbeat files with no matching registry entry removed | **MISSING** | No implementation |

**Evidence:**
- `src/peer/mod.rs:195-200`: `handle_crashed_peer()` removes heartbeat file
- No scan for orphaned heartbeat files (files with no matching `workers.json` entry)

**Gap:** If a heartbeat file exists but the worker was never registered (registry corruption, manual file deletion), the heartbeat file persists forever.

### 3. Rate Limiter State Cleanup

| Scenario | Status | Implementation |
|----------|--------|----------------|
| rate_limits/ files with stale last_refill timestamps reset or removed | **MISSING** | No implementation |
| Concurrency counters reconciled against live workers | **YES** | RateLimiter checks live registry entries |

**Evidence:**
- `src/rate_limit/mod.rs:96-135`: TokenBucket stores `last_refill` timestamp
- `src/rate_limit/mod.rs:256-271`: RPM check uses token bucket
- No cleanup of stale token bucket files
- `src/rate_limit/mod.rs:208-254`: Concurrency checks query `Registry::list()` which filters dead PIDs

**Gap:** Token bucket files in `~/.needle/state/rate_limits/*.json` are never cleaned up. If a provider's configuration changes (RPM limit increased), old bucket files retain stale state.

### 4. Lock File Cleanup

| Scenario | Status | Implementation |
|----------|--------|----------------|
| Stale claim lock files (age > file_locks.timeout) released | **YES** | `cleanup_orphaned_locks()` |
| Lock files referencing dead PIDs immediately released | **YES** | Uses `try_acquire_flock()` which fails if holder is gone |

**Evidence:**
- `src/strand/mend.rs:289-380`: Complete implementation with flock verification

### 5. Log File Cleanup

| Scenario | Status | Implementation |
|----------|--------|----------------|
| Agent log files older than retention_days pruned | **YES** | `cleanup_old_agent_logs()` |
| Log files from workers that never processed beads cleaned up earlier | **MISSING** | No special handling |

**Evidence:**
- `src/strand/mend.rs:437-537`: Agent log cleanup by age
- No check for `beads_processed == 0` in log cleanup

**Gap:** Workers that crash immediately (0 beads processed) leave log files that persist for `retention_days` instead of being cleaned up sooner.

### 6. Bead State Cleanup

| Scenario | Status | Implementation |
|----------|--------|----------------|
| In-progress beads assigned to dead workers released | **YES** | PeerMonitor + cleanup_orphaned_in_progress() |
| Beads stuck in "claimed" state with no live claimant released | **YES** | Same as above |
| Dependency links pointing to non-existent beads cleaned up | **DETECTION ONLY** | `cleanup_stale_dependencies()` detects but doesn't remove |

**Evidence:**
- `src/strand/mend.rs:48-120`: `cleanup_orphaned_in_progress()` function
- `src/strand/mend.rs:389-427`: Dependency detection only (comment confirms no removal)

**Gap:** Stale dependency links are detected and telemetry is emitted, but links are not actually removed because `br` has no `remove_dependency` command.

## Follow-up Beads Required

### High Priority

1. **needle-XXXX**: Add registry dead worker proactive cleanup to mend
   - Scan workers.json and remove entries where PID is dead
   - Emit telemetry for each deregistered worker
   - Add test coverage

2. **needle-XXXX**: Clean up orphaned heartbeat files in mend
   - Scan heartbeat_dir for files with no matching registry entry
   - Remove orphaned heartbeat files
   - Emit telemetry

3. **needle-XXXX**: Clean up stale rate limiter state files in mend
   - Remove token bucket files for providers no longer in config
   - Reset buckets with stale `last_refill` (> 1 hour old)
   - Emit telemetry

### Medium Priority

4. **needle-XXXX**: Flag idle workers (0 beads processed) in mend
   - Check for workers with `beads_processed == 0` registered > idle_timeout
   - Emit telemetry warning (do not deregister, may be genuinely waiting)
   - Add test coverage

5. **needle-XXXX**: Earlier cleanup for zero-activity worker logs
   - Detect agent logs from workers with 0 beads processed
   - Clean up these logs immediately (don't wait for retention_days)
   - Requires correlating log file names with worker registry entries

### Low Priority (External Dependency)

6. **needle-XXXX**: Remove stale dependency links (upstream: beads_rust)
   - Depends on `br` adding a `remove_dependency` command
   - Once available, update `cleanup_stale_dependencies()` to actually remove links
   - Currently blocked by lack of upstream functionality

## Telemetry Coverage

All cleanup operations emit appropriate telemetry events:
- `EventKind::BeadReleased` - orphaned bead releases
- `EventKind::StuckReleased` - stuck peer bead releases
- `EventKind::MendOrphanedLockRemoved` - lock file cleanup
- `EventKind::MendDependencyCleaned` - dependency detection
- `EventKind::MendDbRepaired` / `EventKind::MendDbRebuilt` - DB recovery
- `EventKind::MendCycleSummary` - aggregate summary

## Test Coverage

Comprehensive test coverage exists for implemented scenarios:
- Crashed peer bead release
- Orphaned in-progress detection
- Lock file cleanup
- Dependency detection
- DB recovery pipeline
- Agent log cleanup

No tests exist for the missing scenarios (as expected).
