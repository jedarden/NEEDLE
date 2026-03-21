# Concurrency

Multiple NEEDLE workers operate in the same environment simultaneously. This document specifies how they coordinate without a central orchestrator.

---

## Coordination Model

NEEDLE uses **decentralized coordination through shared state**. There is no coordinator process, no leader election, no message passing between workers. All coordination happens through:

1. **Atomic bead claims** (SQLite transactions via `br update --claim`)
2. **Workspace-level flock** (POSIX file locks for claim serialization)
3. **File-based heartbeats** (health monitoring and stale detection)
4. **Worker state registry** (shared JSON file for fleet awareness)

This is approach #1 (SQLite transactions) from `docs/research/concurrency-approaches-compared.md`, augmented with file-based serialization to address the thundering herd problem.

---

## Claiming Protocol

### The Thundering Herd Problem

Without serialization, all workers compute the same priority ordering and race to claim the same top bead. N-1 workers lose, retry, compute the same ordering again, and race for the second bead. This wastes O(N^2) claim attempts.

### Solution: Per-Workspace Flock

```
┌─────────────┐      ┌─────────────┐      ┌─────────────┐
│  Worker A    │      │  Worker B    │      │  Worker C    │
│  SELECT bead │      │  SELECT bead │      │  SELECT bead │
└──────┬──────┘      └──────┬──────┘      └──────┬──────┘
       │                     │                     │
       ▼                     ▼                     ▼
   ┌───────────────────────────────────────────────────┐
   │          flock(/tmp/needle-claim-<workspace>.lock) │
   │                                                    │
   │  A enters ─► claim bead-1 ─► success ─► release   │
   │  B enters ─► claim bead-2 ─► success ─► release   │
   │  C enters ─► claim bead-3 ─► success ─► release   │
   └───────────────────────────────────────────────────┘
```

**Protocol:**

1. Worker computes candidate list (deterministic ordering)
2. Worker acquires flock on workspace lock file (blocking, with timeout)
3. Worker verifies top candidate is still claimable
4. Worker executes `br update <id> --claim --actor <worker-id>`
5. Worker releases flock
6. If claim failed (race with non-NEEDLE claimer), retry with next candidate

**Lock file path:** `/tmp/needle-claim-{workspace_hash}.lock` where `workspace_hash` is a deterministic hash of the workspace absolute path.

**Lock timeout:** 10 seconds. If the lock cannot be acquired within this time, the worker skips this workspace and moves to the next strand.

**Lock scope:** The lock is held only during the claim attempt (steps 2-5), not during bead execution. This means the lock is held for milliseconds, not minutes.

---

## Heartbeat Protocol

Every worker emits a heartbeat file to enable peer monitoring and stale claim detection.

### Heartbeat File

```
~/.needle/state/heartbeats/<worker-id>.json
```

Contents:

```json
{
  "worker_id": "needle-claude-anthropic-sonnet-alpha",
  "pid": 12345,
  "state": "EXECUTING",
  "current_bead": "nd-a3f8",
  "workspace": "/home/coder/project",
  "last_heartbeat": "2026-03-20T15:30:00Z",
  "started_at": "2026-03-20T14:00:00Z",
  "beads_processed": 7,
  "session": "needle-claude-anthropic-sonnet-alpha"
}
```

### Emission

- Heartbeat is emitted every `heartbeat_interval` (default: 30 seconds)
- Emitted from a dedicated thread/task, independent of the main worker loop
- Updates `last_heartbeat` timestamp and `state` field
- File write is atomic (write to temp file, rename)

### TTL and Stale Detection

A heartbeat is **stale** if `now - last_heartbeat > heartbeat_ttl` (default: 5 minutes).

A stale heartbeat means the worker has stopped updating — it has crashed, hung, or been killed.

### Peer Monitoring

The Mend strand (Strand 3) checks peer heartbeats:

1. Read all heartbeat files in `~/.needle/state/heartbeats/`
2. For each stale heartbeat:
   a. Check if the PID is still alive (`kill -0 <pid>`)
   b. If PID is dead: worker crashed. Clean up.
   c. If PID is alive but heartbeat is stale: worker is stuck. Log warning.
3. For crashed workers:
   a. Release any claimed bead
   b. Remove heartbeat file
   c. Deregister from worker state registry
   d. Emit `peer.crashed` telemetry

---

## Worker State Registry

A shared file tracks all active workers for fleet-level awareness.

```
~/.needle/state/workers.json
```

Contents:

```json
{
  "workers": [
    {
      "id": "needle-claude-anthropic-sonnet-alpha",
      "pid": 12345,
      "workspace": "/home/coder/project",
      "agent": "claude",
      "model": "sonnet",
      "started_at": "2026-03-20T14:00:00Z",
      "beads_processed": 7
    }
  ],
  "updated_at": "2026-03-20T15:30:00Z"
}
```

**Access pattern:**
- Workers register on startup, deregister on shutdown
- Registry is updated via flock-protected read-modify-write
- Used by `needle list`, `needle status`, and fleet-level telemetry
- Not used for coordination — heartbeats handle that

---

## Concurrency Limits

### Provider/Model Limits

Rate limiting prevents API throttling and controls cost:

```yaml
# ~/.needle/config.yaml
limits:
  max_workers: 20                    # hard ceiling on total workers
  launch_stagger_seconds: 2          # delay between worker launches
  providers:
    anthropic:
      max_concurrent: 10             # max workers using Anthropic simultaneously
      requests_per_minute: 60
    openai:
      max_concurrent: 5
      requests_per_minute: 40
  models:
    claude-sonnet:
      max_concurrent: 8
    claude-opus:
      max_concurrent: 3              # expensive model, limit concurrency
```

**Enforcement:**
- Before dispatching to an agent, the worker checks the provider/model concurrency counters
- If at limit, the worker waits with backoff (not the same as strand exhaustion — there is work, just rate limited)
- Counters are maintained in the worker state registry
- RPM limits are enforced via a token bucket per provider (stored in `~/.needle/state/rate_limits/`)

### Fleet Sizing Guidance

From `docs/notes/operational-fleet-lessons.md`:

- **EX44 (20 cores):** ~20 workers max. 40+ workers drove CPU load to 35+ from explore strand overhead alone.
- **Rule of thumb:** workers ≤ CPU cores. The agent process dominates CPU, but NEEDLE overhead (lock contention, heartbeat I/O, strand evaluation) adds up.
- The `max_workers` config is a hard ceiling enforced at launch time. `needle run --count=25` with `max_workers: 20` will launch 20 and log a warning.

---

## Race Condition Prevention

Lessons from `docs/notes/claim-race-conditions.md`, applied to the new design:

| Race Condition | v1 Impact | v2 Prevention |
|---------------|-----------|---------------|
| **Thundering herd** | All workers claim same bead | Per-workspace flock serializes claims |
| **TOCTOU on closed beads** | Worker claims bead that was just closed | Verify bead status inside flock before claiming |
| **Stale claims from crashed workers** | Beads stuck `in_progress` forever | Heartbeat TTL + Mend strand auto-release |
| **Lock file leaks** | Orphaned locks block claims | Lock TTL + Mend strand cleanup |
| **Concurrent bead creation** | (Weave/Pulse/Unravel) create duplicates | Seen-issue deduplication + creation cooldowns |

---

## Invariants

1. **One claim at a time per workspace.** The flock guarantees this. Two workers cannot execute `br update --claim` simultaneously in the same workspace.

2. **One bead per worker.** A worker holds at most one claimed bead. It releases or verifies closure before claiming another.

3. **Claims have a TTL.** If a worker holds a claim for longer than `heartbeat_ttl` without updating its heartbeat, the claim is considered stale and eligible for release by Mend.

4. **No implicit locking.** Labels are not locks. Bead status is not a lock. Only flock and `br update --claim` provide mutual exclusion.

5. **Lock scope is minimal.** The workspace flock is held for milliseconds (duration of the `br` CLI call), never for the duration of bead execution.
