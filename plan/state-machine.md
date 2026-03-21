# State Machine

The worker loop is a finite state machine. Every state has defined entry conditions, actions, and exit transitions. There are no implicit states or fallthrough paths.

---

## Worker States

```
                    ┌──────────┐
                    │  BOOTING │
                    └────┬─────┘
                         │ config loaded, health check passed
                         ▼
                    ┌──────────┐
              ┌────►│ SELECTING│◄──────────────────────────────┐
              │     └────┬─────┘                                │
              │          │ candidate found                      │
              │          ▼                                      │
              │     ┌──────────┐  race lost (retry < max)      │
              │     │ CLAIMING │──────────────────────────┐    │
              │     └────┬─────┘                           │    │
              │          │ claimed                         │    │
              │          ▼                                 ▼    │
              │     ┌──────────┐                     ┌────────┐│
              │     │ BUILDING │                     │RETRYING││
              │     └────┬─────┘                     └────┬───┘│
              │          │ prompt ready                   │     │
              │          ▼                                └─────┘
              │     ┌────────────┐
              │     │DISPATCHING │
              │     └────┬───────┘
              │          │ agent process started
              │          ▼
              │     ┌──────────┐
              │     │ EXECUTING│
              │     └────┬─────┘
              │          │ agent exited
              │          ▼
              │     ┌──────────┐
              │     │ HANDLING │
              │     └────┬─────┘
              │          │ outcome processed
              │          ▼
              │     ┌──────────┐
              │     │ LOGGING  │
              │     └────┬─────┘
              │          │ telemetry emitted
              └──────────┘
```

### Terminal States

```
    ┌───────────┐       ┌───────────┐       ┌───────────┐
    │ EXHAUSTED │       │  STOPPED  │       │  ERRORED  │
    └───────────┘       └───────────┘       └───────────┘
    all strands empty   graceful shutdown   unrecoverable
```

---

## State Definitions

### BOOTING

**Entry:** Worker process started.

**Actions:**
1. Load configuration (global → workspace → CLI overrides)
2. Validate bead store connectivity (`br doctor` or equivalent)
3. Register in worker state registry
4. Emit `worker.started` telemetry event
5. Start heartbeat emitter

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Config loaded, bead store healthy | SELECTING |
| Config invalid | ERRORED |
| Bead store unreachable | ERRORED (after retry with backoff) |

---

### SELECTING

**Entry:** Worker is ready for next bead. This is the strand waterfall entry point.

**Actions:**
1. Emit heartbeat
2. Evaluate strands in sequence (see [strands.md](strands.md))
3. First strand that yields a candidate bead wins

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Candidate bead found | CLAIMING |
| All strands exhausted | EXHAUSTED |
| Shutdown signal received | STOPPED |

---

### CLAIMING

**Entry:** A candidate bead has been selected.

**Actions:**
1. Acquire workspace claim lock (flock, per-workspace)
2. Verify bead is still claimable (`br show --json`, check status + assignee)
3. Attempt atomic claim: `br update <id> --claim --actor <worker-id>`
4. Release workspace claim lock
5. Emit `bead.claim.attempted` telemetry

**Transitions:**
| Condition | Exit Code | Next State |
|-----------|-----------|-----------|
| Claim succeeded | 0 | BUILDING |
| Race lost (already claimed) | 4 | RETRYING |
| Bead no longer claimable (closed, deferred) | 1 | SELECTING |
| Bead store error | >0 | SELECTING (after backoff) |
| Max retries exceeded | — | SELECTING (exclude this bead, reset retry counter) |

---

### RETRYING

**Entry:** A claim attempt failed due to race condition.

**Actions:**
1. Increment retry counter for this selection cycle
2. Add failed bead ID to exclusion set
3. Emit `bead.claim.race_lost` telemetry

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Retry count < max_retries (default: 5) | CLAIMING (with next candidate from same strand) |
| Retry count >= max_retries | SELECTING (reset, move to next strand) |

---

### BUILDING

**Entry:** Bead is claimed by this worker.

**Actions:**
1. Read full bead context: title, body, dependencies, labels, workspace path
2. Read workspace context: CLAUDE.md, AGENTS.md, .beads/config.yaml
3. Construct prompt from template (deterministic: same bead → same prompt)
4. Include bead ID and `br close <id>` instruction in prompt
5. Emit `prompt.built` telemetry (bead ID, prompt hash, token estimate)

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Prompt built successfully | DISPATCHING |
| Bead context unreadable | HANDLING (outcome: failure, release bead) |

---

### DISPATCHING

**Entry:** Prompt is ready.

**Actions:**
1. Load agent adapter configuration (YAML)
2. Resolve invoke template with prompt, workspace path, environment
3. Start agent process via rendered command
4. Record process PID, start time
5. Emit `agent.dispatched` telemetry (agent name, model, workspace)

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Agent process started | EXECUTING |
| Agent binary not found | HANDLING (outcome: failure, release bead) |
| Adapter config invalid | HANDLING (outcome: failure, release bead) |

---

### EXECUTING

**Entry:** Agent process is running.

**Actions:**
1. Wait for agent process to exit
2. Capture stdout, stderr, exit code
3. Monitor execution timeout
4. Continue emitting heartbeats while waiting
5. Emit `agent.executing` heartbeat telemetry periodically

**Transitions:**
| Condition | Exit Code | Next State |
|-----------|-----------|-----------|
| Agent exited normally | any | HANDLING |
| Execution timeout exceeded | — | Kill process → HANDLING (outcome: timeout) |
| Shutdown signal received | — | Kill process → HANDLING (outcome: interrupted) |

---

### HANDLING

**Entry:** Agent has exited (or was killed). This is where outcome routing happens.

**Actions:**
1. Classify outcome by exit code
2. Execute the handler for that outcome class
3. Emit `bead.outcome` telemetry

**Outcome Table:**

| Outcome | Exit Code | Handler | Bead Action |
|---------|-----------|---------|-------------|
| **Success** | 0 | Verify bead was closed by agent. If not, log warning (do not auto-close). | None (agent owns closure) |
| **Failure** | 1 | Release bead (`br update --status open --unassign`). Increment bead failure count via label. | Released |
| **Timeout** | 124 | Release bead. Add `deferred` label. | Released + deferred |
| **Crash** | >128 (signal) | Release bead. Create alert bead in workspace. | Released + alert |
| **Race Lost** | 4 | (Handled at CLAIMING, should not reach here) | N/A |
| **Interrupted** | — | Release bead. Clean shutdown. | Released |
| **Agent Not Found** | 127 | Release bead. Emit error. Do not retry (config issue). | Released |
| **Build Failure** | — | Release bead. Emit error. | Released |

**Agent-owned closure:** NEEDLE does not close beads. The agent is instructed (via prompt) to run `br close <id>` upon successful completion. If the bead is still open after a success exit code, NEEDLE logs a warning but does not intervene. This is a deliberate design choice based on operational experience (see `docs/notes/bead-lifecycle-bugs.md`).

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Outcome processed | LOGGING |

---

### LOGGING

**Entry:** Outcome has been handled.

**Actions:**
1. Record effort: elapsed time, exit code, token count (if extractable), estimated cost
2. Emit `bead.completed` or `bead.released` telemetry
3. Update worker state registry (beads processed, current streak)
4. Reset retry counter and exclusion set

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Logging complete | SELECTING |

---

### EXHAUSTED

**Entry:** All strands returned no work.

**Actions:**
1. Emit `worker.exhausted` telemetry
2. If `idle_timeout` configured, start countdown
3. If `idle_action` is `wait`, sleep with exponential backoff (max 60s)
4. If `idle_action` is `exit`, terminate

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Backoff expired, retry strands | SELECTING |
| Idle timeout exceeded | STOPPED |
| Shutdown signal received | STOPPED |

---

### STOPPED

**Entry:** Graceful shutdown.

**Actions:**
1. Release any claimed bead
2. Deregister from worker state registry
3. Stop heartbeat emitter
4. Emit `worker.stopped` telemetry
5. Exit process

**Transitions:** None (terminal).

---

### ERRORED

**Entry:** Unrecoverable error.

**Actions:**
1. Release any claimed bead (best-effort)
2. Emit `worker.errored` telemetry with error details
3. Deregister from worker state registry (best-effort)
4. Exit process with non-zero code

**Transitions:** None (terminal).

---

## Error Model

Errors are classified into three tiers:

### Tier 1: Transient (retry)

Temporary failures that resolve on their own. The worker retries with backoff.

- Bead store temporarily unreachable
- Claim race lost
- Lock contention timeout
- Agent timeout (may succeed on re-dispatch)

### Tier 2: Bead-scoped (release and continue)

Failures specific to one bead. Release it and move on.

- Agent exited with failure
- Prompt build failed (bead context unreadable)
- Agent binary missing

### Tier 3: Worker-scoped (exit)

Failures that affect the worker's ability to function. Exit and let the fleet manager handle it.

- Configuration invalid
- Bead store persistently unreachable
- Filesystem full
- Heartbeat file unwritable

---

## Invariants

These must hold at all times. Violation of any invariant is a bug.

1. **A worker holds at most one claimed bead.** There is no pipelining or parallel execution within a single worker.

2. **A claimed bead is always released.** Every path through HANDLING releases the bead unless the agent closed it. There is no path where a bead remains claimed after the worker moves to SELECTING.

3. **Heartbeat is continuous.** From BOOTING to STOPPED/ERRORED, the worker emits heartbeats. A gap in heartbeats means the worker is stuck or dead.

4. **Telemetry is emitted for every state transition.** Silent transitions do not exist.

5. **The exclusion set is bounded.** It is cleared on every transition to SELECTING. It cannot grow unboundedly within a selection cycle because max_retries is finite.

6. **Shutdown is always graceful when possible.** SIGTERM triggers STOPPED, not ERRORED. Only SIGKILL causes ungraceful termination, and heartbeat TTL handles that case.
