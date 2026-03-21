# Multi-Worker Concurrency Approaches Across the Beads Ecosystem

## Research Date: 2026-03-20

## The Core Problem

When multiple autonomous agents process beads from a shared queue, they must coordinate to avoid:
1. **Double-claiming**: Two agents work on the same bead simultaneously
2. **Write contention**: Concurrent writes corrupt the bead database
3. **Stale reads**: An agent reads the queue, but by the time it claims, the queue has changed
4. **Zombie claims**: A crashed agent holds a claim forever, blocking the bead

Different projects solve these problems with fundamentally different approaches.

## Approach 1: SQLite Transaction Isolation (beads_rust / NEEDLE)

**Used by**: NEEDLE, ralph (sasha-incorporated), beads-rust-skill

**Mechanism**: Each `br update` runs within a SQLite transaction. SQLite guarantees serializable isolation for individual transactions.

**Claim sequence**:
```
Worker reads br ready --json
Worker issues br update bd-xxx --status in_progress --assignee "worker-name"
SQLite serializes the update
If another worker already claimed, the status is already in_progress
Worker must detect this and retry with a different bead
```

**Strengths**:
- No additional infrastructure (no server, no lock files)
- Works with unmodified beads_rust
- Multiple concurrent readers via WAL mode

**Weaknesses**:
- No true atomic claim (must check status after update)
- FrankenSQLite corruption under concurrent access (issue #171)
- Auto-import/auto-flush conflicts with 3+ processes (issue #191)
- Thundering herd at 11+ workers (all competing for SQLite writes)
- No lock expiry for zombie claims

**NEEDLE's mitigations**:
- Deterministic priority ordering (all workers compute the same order, reducing races)
- Retry loop on claim failure (back to SELECT with exclusion)
- SQLite corruption recovery via JSONL rebuild

## Approach 2: POSIX Advisory Locking (beads-polis)

**Used by**: beads-polis (Perttulands)

**Mechanism**: All writes acquire an exclusive POSIX `flock()` on `events.jsonl.lock`. One writer at a time; others block.

**Claim sequence**:
```
Worker acquires flock on events.jsonl.lock (blocks if held)
Worker appends claim event to events.jsonl
Worker releases flock
SQLite index is rebuilt from JSONL on next read
```

**Strengths**:
- Corruption-proof (JSONL is append-only, SQLite is disposable)
- Simple mental model (one writer at a time)
- Built-in heartbeat and lock expiry for zombie claims
- Operator role for orchestrator override

**Weaknesses**:
- Write serialization (throughput bottleneck with many workers)
- All workers block on the lock holder
- Not compatible with unmodified beads_rust
- Smaller community, less maintained

## Approach 3: Coordination Server (bead-forge, proposed)

**Used by**: bead-forge (jedarden, research phase)

**Mechanism**: A dedicated coordination server handles all claim operations. Workers connect to the server instead of accessing SQLite directly.

**Claim sequence**:
```
Worker sends claim request to server
Server atomically assigns the bead
Server responds with assignment
No SQLite contention at the worker level
```

**Strengths**:
- Zero contention at any worker count
- Atomic claiming by design
- Can implement sophisticated scheduling
- Eliminates thundering herd

**Weaknesses**:
- Additional infrastructure (server process to run/monitor)
- Single point of failure
- Not yet implemented (research phase only)
- Network dependency (workers must reach the server)

## Approach 4: Central Orchestrator Assignment (Perles, beads-orchestration-claude)

**Used by**: Perles (Coordinator/Worker model), beads-orchestration-claude (Opus orchestrator)

**Mechanism**: A dedicated Coordinator agent manages all work assignment. Workers do not claim beads; the Coordinator assigns them.

**Claim sequence**:
```
Coordinator queries bd ready
Coordinator selects beads for each worker
Coordinator dispatches work to workers
Workers execute without knowing about each other
No claim races (only the Coordinator touches the queue)
```

**Strengths**:
- No claim races by design
- Sophisticated scheduling (Coordinator can consider worker specialization, load, etc.)
- Clean separation of concerns

**Weaknesses**:
- Single point of failure (Coordinator crash stops everything)
- Coordinator consumes context/tokens for management overhead
- Lower parallelism (Coordinator is a bottleneck)
- Coordinator must track all worker state

## Approach 5: Cooperative File Claims (OBC)

**Used by**: obc_agent_workflow

**Mechanism**: Agents claim files (not beads) via `.coordination/` YAML files. No enforcement -- agents must obey the protocol.

**Claim sequence**:
```
Agent checks .coordination/ for claimed files
Agent writes its own claims to agent-<name>.yaml
Other agents check before starting work
Conflicts detected by reading YAML, not by locking
```

**Strengths**:
- Simple, human-readable
- Works across git worktrees
- Prevents file-level conflicts (which bead-level claiming misses)
- No infrastructure requirements

**Weaknesses**:
- No enforcement (agents can ignore claims)
- Race condition (two agents read simultaneously, both see no claims, both claim)
- Manual process (not automated)
- Claims are on files, not beads

## Approach 6: No Coordination (Ralph)

**Used by**: ralph-beads, ralph, Initializer

**Mechanism**: Single-worker sequential processing. No concurrency, so no coordination needed.

**Claim sequence**:
```
Worker runs bd ready | head -1
Worker processes the bead
Worker checks if bead was closed
Repeat
```

**Strengths**:
- Zero complexity
- No bugs possible (no concurrency)
- Works with any beads backend
- Maximum simplicity

**Weaknesses**:
- Single-threaded (10-20x slower than multi-worker)
- Cannot scale to large projects
- No resilience (one worker dies, all work stops)

## Summary Matrix

| Approach | Throughput | Correctness | Complexity | Resilience | Infrastructure |
|----------|-----------|-------------|------------|------------|---------------|
| SQLite transactions | Medium | Fragile | Low | High | None |
| POSIX flock | Low | Strong | Low | High | None |
| Coordination server | High | Strong | High | Low (SPOF) | Server |
| Central orchestrator | Medium | Strong | Medium | Low (SPOF) | Orchestrator agent |
| Cooperative claims | N/A | Weak | Low | High | None |
| No coordination | Low | Perfect | Zero | Low | None |

## Recommendations for NEEDLE

NEEDLE currently uses Approach 1 (SQLite transactions) and suffers from its known limitations. Options for improvement:

1. **Short-term**: Add heartbeat-like liveness detection. If a worker has not updated a bead in N minutes, consider the claim expired. This requires no beads_rust changes.

2. **Medium-term**: Evaluate beads-polis as a backend swap. The POSIX flock model is simpler and more correct, with built-in heartbeat/expiry. The write serialization may be acceptable if claim operations are fast (the long part is agent execution, which is not serialized).

3. **Long-term**: Watch bead-forge. If a coordination server materializes, it could eliminate all concurrency issues at scale. But it does not exist yet.

4. **Hybrid**: Use file-based claiming (like OBC's coordination YAMLs) as a supplement to bead-level claiming. Workers claim both the bead (via `br update`) and the files they will modify (via a coordination file). This prevents file-level conflicts that bead-level claiming misses.
