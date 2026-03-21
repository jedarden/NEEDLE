# Self-Healing

NEEDLE workers must detect and recover from failures without human intervention. This document specifies the failure modes, detection mechanisms, and recovery procedures.

---

## Failure Taxonomy

| Failure | Scope | Detection | Recovery |
|---------|-------|-----------|----------|
| Worker crash | Worker | Heartbeat TTL expiry | Peer cleanup via Mend |
| Worker stuck | Worker | Heartbeat stale + PID alive | Alert via Knot |
| Agent hang | Bead | Execution timeout | Kill process, release bead |
| Stale claim | Bead | in_progress + no heartbeat | Mend releases bead |
| Orphaned lock | Workspace | Lock file age > TTL | Mend removes lock |
| Database corruption | Workspace | `br doctor` detects | Auto-repair from JSONL |
| Stale dependency | Bead | Closed bead still blocks open bead | Mend cleans dependency |
| Disk full | System | Write failure | Emit alert, graceful stop |
| Bead store unreachable | System | `br` command fails | Retry with backoff, then stop |

---

## Heartbeat-Based Detection

### How It Works

```
Worker A (alive)          Worker B (alive)         Worker C (crashed)
┌──────────────┐         ┌──────────────┐         ┌──────────────┐
│ heartbeat:   │         │ heartbeat:   │         │ heartbeat:   │
│ 15:30:00     │         │ 15:30:10     │         │ 15:20:00     │ ← stale
│ state: EXEC  │         │ state: SEL   │         │ state: EXEC  │
│ bead: nd-a3f │         │ bead: null   │         │ bead: nd-x7y │ ← orphaned claim
└──────────────┘         └──────────────┘         └──────────────┘

Worker B runs Mend strand:
  1. Reads all heartbeat files
  2. Detects C's heartbeat is stale (10 min old, TTL is 5 min)
  3. Checks PID: dead
  4. Releases nd-x7y: br update nd-x7y --status open --unassign
  5. Removes C's heartbeat file
  6. Deregisters C from worker registry
  7. Emits peer.crashed telemetry
```

### Detection Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `heartbeat_interval` | 30s | How often a worker writes its heartbeat |
| `heartbeat_ttl` | 5min | How long before a heartbeat is considered stale |
| `peer_check_interval` | 60s | How often Mend checks peer heartbeats |

**Relationship:** `heartbeat_ttl` should be at least `3 × heartbeat_interval` to tolerate transient delays.

### Stuck vs. Crashed

| Signal | Diagnosis | Action |
|--------|-----------|--------|
| Stale heartbeat + PID dead | **Crashed.** Worker terminated unexpectedly. | Release claim, clean up, emit `peer.crashed` |
| Stale heartbeat + PID alive | **Stuck.** Worker is hung (deadlock, infinite loop, blocked I/O). | Emit `peer.stale` warning. Do NOT kill — may be legitimately slow. Alert via Knot after threshold. |
| Fresh heartbeat + PID alive | **Healthy.** Normal operation. | No action. |

**NEEDLE does not auto-kill stuck workers.** A stuck worker with a live PID may be executing a legitimately slow agent. Killing it would interrupt work. Instead, NEEDLE alerts via Knot and lets the human decide.

---

## Database Recovery

beads_rust uses SQLite with a known corruption issue (FrankenSQLite, upstream #171). NEEDLE must handle this.

### Detection

Database corruption is detected when:
- `br` commands return "database disk image is malformed"
- `br doctor` reports integrity errors
- JSON output from `br` is truncated or invalid

### Recovery Procedure

```
Corruption detected
       │
       ▼
  Run br doctor --repair
       │
       ├── Success ──► Resume operation, emit health.db_repaired
       │
       └── Failure ──► Full rebuild:
                          1. rm .beads/beads.db
                          2. br sync --import
                          3. Verify: br doctor
                          │
                          ├── Success ──► Resume, emit health.db_rebuilt
                          │
                          └── Failure ──► ERRORED state (JSONL itself may be corrupt)
```

**Key insight from `docs/notes/mitosis-explosion-postmortem.md`:** The JSONL file is always the authoritative data source. It is append-only and immune to SQLite corruption. Recovery always rebuilds from JSONL.

### Proactive Health Checks

The Mend strand runs `br doctor` (without `--repair`) periodically:
- After every N beads processed (configurable, default: 50)
- On every Mend strand evaluation
- If doctor reports warnings, escalate to `--repair` immediately rather than waiting for failure

---

## Stale Claim Recovery

### The Problem

A worker crashes while holding a claimed bead. The bead is stuck in `in_progress` with a dead assignee. Without cleanup, the bead is permanently blocked.

### The Solution

```
Mend strand evaluates:
  1. Query beads with status=in_progress
  2. For each:
     a. Read assigned worker ID from bead
     b. Check worker's heartbeat file
     c. If heartbeat is stale AND PID is dead:
        - br update <bead_id> --status open --unassign
        - Emit bead.released telemetry (reason: stale_claim)
     d. If heartbeat is stale AND PID is alive:
        - Emit peer.stale warning (do not release — worker may be slow)
     e. If heartbeat is fresh:
        - Normal operation, skip
```

### Safety: No Premature Release

A claimed bead is only released if the owning worker is **confirmed dead** (stale heartbeat AND dead PID). If the PID is alive, the bead is not released, even if the heartbeat is stale. This prevents:
- Releasing a bead while the agent is still working on it
- Releasing a bead from a worker whose heartbeat thread is delayed

---

## Lock File Recovery

### Orphaned Locks

Workspace claim locks can be orphaned if a worker crashes while holding a flock. On Linux, flock is automatically released when the process exits, so this is primarily a defense against:
- Lock files left behind after crashes (the file exists but no flock is held)
- Manual lock files used by other subsystems

### Cleanup

Mend strand checks lock file age:
1. Read lock files in `/tmp/needle-claim-*.lock`
2. If file modification time > `lock_ttl` (default: 10 minutes):
   - Attempt to acquire flock (non-blocking)
   - If acquired: no one holds it, delete the file, release flock
   - If not acquired: someone is actively holding it, skip

---

## Dependency Link Recovery

### The Problem

Bead A blocks Bead B via a dependency link. Bead A is closed, but the dependency link is not automatically cleaned. Bead B remains blocked forever.

### The Solution

Mend strand checks for stale dependencies:
1. Query beads with status `open` that have blockers
2. For each blocker, check if the blocking bead is closed
3. If blocker is closed, remove the dependency link
4. Emit `bead.dependency.cleaned` telemetry

This is a necessary compensating mechanism because `br` does not automatically resolve dependency links on bead closure (documented in `docs/notes/bead-lifecycle-bugs.md`).

---

## Graceful Degradation

When subsystems fail, NEEDLE degrades gracefully rather than crashing:

| Subsystem Failure | Degradation |
|-------------------|-------------|
| Telemetry file sink unwritable | Buffer events in memory, retry. If buffer full, drop events. Worker continues. |
| Heartbeat file unwritable | Log error. Worker continues but cannot be monitored by peers. If persistent, ERRORED. |
| Worker registry unwritable | Log error. Worker continues but invisible to `needle list`. |
| Database corrupt | Auto-repair. If repair fails, ERRORED for that workspace only. |
| Single workspace unreachable | Skip workspace in Explore, continue with others. |
| Config file unreadable mid-session | Use cached config from boot. Emit warning. |

---

## Self-Modification Protection

From `docs/notes/self-modification-risks.md`: NEEDLE workers must not modify the NEEDLE binary or configuration during a session.

### Rules

1. **Binary immutability.** The NEEDLE binary is not modified while any worker is running. Updates are applied between sessions only (via `needle upgrade` when no workers are active).

2. **Configuration immutability.** Config is loaded at boot and cached for the session. Changes to config files take effect on next worker restart, not mid-session. No hot-reload.

3. **Workspace exclusion.** If NEEDLE's source code lives in a workspace with beads, workers do not process beads for NEEDLE itself unless explicitly configured. This prevents the self-referential bug cycles documented in v1.

4. **Version pinning.** All workers in a fleet run the same NEEDLE version. `needle run` checks that the binary version matches the registry version and refuses to start if mismatched.
