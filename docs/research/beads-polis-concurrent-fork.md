# beads-polis: Event-Sourced Fork for Multi-Agent Concurrency

## Research Date: 2026-03-20
## Source: https://github.com/Perttulands/beads-polis

## What It Is

beads-polis is a fork of beads_rust that fundamentally restructures the storage model around event sourcing. Built for "Polis" -- a multi-agent city simulation -- it replaces beads_rust's SQLite-primary model with an append-only JSONL event log as the single source of truth. The SQLite database becomes a derived, disposable index that auto-rebuilds on corruption.

~3,300 LOC vs. beads_rust's ~20,000 LOC.

## Core Architecture Change

| Aspect | beads_rust (upstream) | beads-polis |
|--------|----------------------|-------------|
| Write target | SQLite primary | JSONL append-only |
| Source of truth | SQLite (JSONL is export) | JSONL (SQLite is cache) |
| Concurrency | SQLite transactions (broken under load) | POSIX flock on lock file |
| Recovery | Manual `br sync --import` | Auto-rebuild from JSONL |
| Index corruption | Catastrophic (issue #171) | Delete and rebuild |

This inversion solves both major beads_rust bugs:
- **Issue #171 (FrankenSQLite corruption)**: SQLite is disposable. Corrupt? Delete it. Next read auto-rebuilds.
- **Issue #191 (concurrent SyncConflict)**: No auto-import/auto-flush conflict because JSONL is always authoritative.

## POSIX flock Concurrency Model

All writes acquire an exclusive POSIX advisory lock on `events.jsonl.lock`:

- **Writes are serialized**: One writer at a time, others block (not fail)
- **Reads are unblocked**: Via SQLite WAL mode on the derived index
- **Index rebuilds** also acquire the lock to prevent concurrent reconstruction

This is a simpler model than SQLite transactions or Dolt server mode. It trades throughput (one writer at a time) for correctness (no corruption, no false conflicts).

## Claim/Heartbeat/Unclaim Lifecycle

beads-polis adds proper claim semantics that beads_rust lacks:

### Claim
```
br claim bd-xxx --lock-for 2h
```
Atomically sets `status=in_progress`, `assignee=<agent>`, and `deadline=now+2h`.

### Heartbeat
```
br heartbeat bd-xxx
```
Extends the deadline by 1 hour. Only the current holder or an `operator` role can heartbeat.

### Unclaim
```
br unclaim bd-xxx
```
Releases the claim. Only the holder or operator can unclaim.

### Lock Expiry
Expired claims can be re-claimed by anyone. This handles agent crashes without manual intervention -- a critical feature for multi-worker orchestration.

### Permission Model
"Only the holder (or operator) can heartbeat, unclaim, or close a claimed bead. Expired claims can be re-claimed by anyone."

## Event Types

Five mutation types append to the event log:
1. **create**: New bead with full initial state
2. **update**: Field-level changes only
3. **close**: Mark complete with reason
4. **reopen**: Reverse a closure
5. **snapshot**: Full state (from compaction, to enable JSONL truncation)

## Cross-Project Aggregation

For multi-project Polis deployments:
- `br city ready`: Ready beads across all projects
- `br city list`: Cross-project filtering
- Enables global task scheduling for agent fleets spanning multiple repos

## Recovery and Resilience

- Corrupt index? Delete it. Next read auto-rebuilds from JSONL.
- Truncated JSONL lines are discarded on read
- Corrupted lines in the middle are skipped (other events preserved)
- `br backup` and `br restore <bundle> --verify --force` for full backup/restore
- `br health` checks JSONL validity, index freshness, SQLite integrity, sync metadata

## Key Design Decisions

1. **JSONL is king**: By making the append-only log authoritative, they eliminate an entire class of corruption bugs. The tradeoff is write serialization (one writer at a time via flock).

2. **Heartbeats over timeouts**: Instead of NEEDLE's approach (detect timeout after the fact, release bead), beads-polis has agents actively signal liveness. Failed heartbeats enable faster recovery.

3. **Operator role**: A supervisor identity that can override claims. This enables an orchestrator (like NEEDLE) to manage worker claims without impersonating the worker.

4. **Minimal codebase**: 3,300 LOC vs. 20,000 LOC. They stripped features to focus on correctness.

## Relevance to NEEDLE

### What NEEDLE Should Adopt

1. **Heartbeat pattern**: NEEDLE currently waits for agent completion or timeout. With heartbeats, NEEDLE could detect stuck agents faster (heartbeat stops -> agent is dead, don't wait for full timeout).

2. **Lock expiry**: Automatic claim release on expiry eliminates the "zombie claim" problem where a crashed worker holds a bead forever.

3. **Operator role**: If NEEDLE is the orchestrator, it should be the operator -- able to manage any claim regardless of which worker holds it.

4. **JSONL-as-truth**: NEEDLE already knows to recover from SQLite corruption via JSONL. beads-polis makes this the default posture rather than an emergency procedure.

### What Would Not Work for NEEDLE

1. **Write serialization**: POSIX flock means one writer at a time. With 10-20 NEEDLE workers all trying to claim beads, this creates a bottleneck. SQLite's transaction isolation (despite its bugs) allows more concurrent readers.

2. **Polis-specific features**: Cross-project aggregation via `city` commands is specific to the Polis simulation. NEEDLE's multi-workspace exploration (Explore strand) serves a similar purpose but differently.

### Migration Consideration

Switching NEEDLE from beads_rust to beads-polis would solve the FrankenSQLite corruption problem and add proper claim semantics. The tradeoffs are write serialization overhead and dependency on a smaller, less-maintained fork. The operator role and heartbeat features alone might justify evaluation.
