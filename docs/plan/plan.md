# NEEDLE Implementation Plan

> **N**avigates **E**very **E**nqueued **D**eliverable, **L**ogs **E**ffort

## Design Principles

These eight principles are non-negotiable. Every design decision in this plan traces back to one or more of them.

1. **Deterministic order.** Given the same queue state, every worker computes the same bead ordering. There is no randomness in selection. Ties are broken by creation time.

2. **Explicit outcome paths.** Every possible result of every operation has a named handler. If an outcome can happen, it has a handler. If it doesn't have a handler, it cannot happen. The type system enforces exhaustiveness.

3. **Platform and model agnostic.** NEEDLE wraps any headless CLI that accepts a prompt and exits. It runs on any POSIX system. It does not depend on any specific AI provider, model, or API.

4. **Observable by default.** Every state transition, claim attempt, dispatch, and outcome emits structured telemetry. A silent worker is a broken worker. Telemetry is structured from origin (JSONL) and exportable as OpenTelemetry (OTLP) so any compliant backend — Tempo, Jaeger, Grafana, Honeycomb, Datadog, FABRIC — can consume NEEDLE's signals without a custom adapter.

5. **Self-healing.** Workers detect and recover from stuck states, stale claims, crashed peers, and corrupted databases without human intervention. Recovery paths are explicit, not heuristic.

6. **Separation of concerns.** The orchestrator does not execute work. The agent does not manage state. The bead store does not enforce workflow. Each component has one job.

7. **The human is the absolute last resort.** Every stuck state has a named, automatic next step, and a human is reached only after every automatic step has been tried and its evidence recorded. Retry, decompose, quarantine with backoff, re-analyze against the plan, *then* label `human` — never before. A bead that reaches a human without that trail is a NEEDLE defect, not an operator task. The fleet-wide count of beads at the human rung is a health metric whose target is zero (see Phase 19).

8. **The workspace plan is the complete plan.** `docs/plan/plan.md` in a workspace is the whole specification an agent works from: every fork is decided in it, and an agent that cannot act from the plan and the bead together is missing plan text, not a human. Escalation therefore re-reads the plan before it asks anyone; work that exists only in a conversation, an ADR nobody wired into the plan, or an operator's head is not work the fleet can do. ADRs record *why* a decision changed; the plan carries the decision itself.

---

## Architecture Overview

NEEDLE is composed of five layers:

```
┌──────────────────────────────────────────────────────────────┐
│                        CLI Layer                              │
│  needle run | stop | list | attach | status | config          │
├──────────────────────────────────────────────────────────────┤
│                     Worker Layer                              │
│  Worker loop, strand waterfall, session management            │
├──────────────────────────────────────────────────────────────┤
│                  Coordination Layer                            │
│  Claiming, locking, heartbeats, peer awareness                │
├──────────────────────────────────────────────────────────────┤
│                    Agent Layer                                 │
│  Adapter interface, dispatch, result capture                  │
├──────────────────────────────────────────────────────────────┤
│                   Foundation Layer                             │
│  Telemetry, configuration, bead store interface, self-healing │
└──────────────────────────────────────────────────────────────┘
```

### Component Map

| Component | Responsibility | Inputs | Outputs |
|-----------|---------------|--------|---------|
| **CLI** | Parse commands, manage sessions | User commands | Worker processes |
| **Worker** | Execute the strand waterfall loop | Bead queue state | Dispatch requests, state transitions |
| **StrandRunner** | Evaluate strands in sequence | Queue state, config | Next bead or escalation |
| **Claimer** | Atomic bead claiming with serialization | Candidate bead ID | Claimed bead or race-lost signal |
| **PromptBuilder** | Construct deterministic prompts from bead context | Claimed bead | Prompt string |
| **Dispatcher** | Load adapter, render template, execute agent | Prompt, adapter config | Agent process handle |
| **OutcomeHandler** | Route exit code to explicit handler | Exit code, stdout/stderr | State transition |
| **Telemetry** | Structured event emission | Any component event | JSONL records |
| **HealthMonitor** | Heartbeat, stuck detection, peer awareness | Worker state | Recovery actions |
| **ConfigLoader** | Hierarchical config resolution | Files, env, CLI args | Resolved config |
| **BeadStore** | Abstract interface to bead backend | CRUD operations | Bead records |
| **Mitosis** | Split multi-task beads into children with dedup | Failed bead, parent's existing children | Child beads or no-op |

---

## Language Decision

The implementation language must provide:

| Requirement | Why | Source |
|-------------|-----|--------|
| Exhaustive pattern matching | Every outcome must be handled; compiler enforces it | Principle 2, bead-lifecycle-bugs.md |
| Real module system | 45K single-file bash was unmaintainable | bash-at-scale-problems.md |
| Structured error types | Silent failures caused cascading bugs | claim-race-conditions.md |
| Native JSON support | Fragile jq parsing corrupted state | worker-starvation-lessons.md |
| Proper concurrency primitives | flock/trap/PID files were inadequate | concurrency-approaches-compared.md |
| Single binary distribution | NEEDLE must be trivially installable | Principle 3 |
| Cross-platform | Linux, macOS at minimum | Principle 3 |
| Static analysis | Catch undefined functions, unused variables at compile time | bundler-build-integrity.md |

**Recommended: Rust.** Exhaustive `match`, `Result<T, E>` error handling, `serde_json`, `tokio` for async, single binary via static linking, cross-compilation. The beads ecosystem already has Rust precedent (beads_rust, beads-polis).

**Acceptable alternative: Go.** Simpler learning curve, good concurrency, single binary. Lacks exhaustive matching (requires discipline instead of compiler enforcement).

**Not acceptable: Bash, Python, Node.** Bash failed at scale (documented). Python/Node require runtime dependencies, violating single-binary distribution.

---

## Key Decisions from Research

These decisions are informed by the 14 research files in `docs/research/`:

| Decision | Chosen Approach | Alternative Considered | Why |
|----------|----------------|----------------------|-----|
| Bead store interface | Abstract trait over `br` CLI | Direct SQLite access | Platform agnostic; works with future bead backends |
| Claim atomicity | `br update --claim` + workspace flock | Central coordinator (Perles) | No SPOF; works with decentralized workers |
| Heartbeat model | File-based heartbeat with TTL (from beads-polis) | Shared memory | Survives worker crashes; observable by peers |
| Validation gates | Pluggable gate system (inspired by bg-gate) | Hardcoded checks | Different workspaces need different validation |
| Work decomposition | Built-in mitosis with child-aware dedup | External only (spec2beads) | Mitosis is valid when the split criteria are semantic (multi-task detection) and dedup checks the parent's existing children |
| Self-modification | Allowed with release channel promotion (testing → stable → fleet hot-reload) | Prohibited entirely | v1 failures came from untested changes deploying directly to the fleet. Canary testing with defined inputs/outputs prevents this. |
| Workspace discovery | Explicit configuration | Filesystem scanning | Explore strand's unbounded find caused 35+ load |
| Alert system | Verify-then-alert with rate limiting | Alert-on-empty | 100% false positive rate from naive alerting |

## Key Decisions from Operational Learnings

These decisions are informed by the 9 notes files in `docs/notes/`:

| Learning | Design Response |
|----------|----------------|
| Mitosis explosion (5,741 duplicate beads) | Mitosis checks parent's existing children before creating new ones. Duplicate splits are structurally impossible. Split criteria are semantic (multi-task detection), not numeric. |
| 100% false positive starvation alerts | Three-state model: no beads exist / all claimed / invisible. Verify independently before alerting. |
| Bundler shipped undefined functions | Compiled language eliminates this class entirely. |
| Agent-owned closure most reliable | NEEDLE does not close beads. Agent receives `br close <id>` instruction in prompt. |
| stdout/stderr corruption | Telemetry is a structured system, never interleaved with agent output. |
| Workers modifying their own orchestrator | Self-modification allowed via release channels. New builds must pass canary tests in isolation before promotion to `:stable`. Fleet hot-reloads from `:stable`, never from `:testing`. |
| ~20 worker practical limit (EX44) | Fleet sizing is bounded by three runtime factors: provider inference throughput, available CPU, and available RAM. NEEDLE monitors these and warns when saturated. Staggered launch is default. |
| Bead granularity affects success rate | Document guidelines but don't enforce — this is a bead authoring concern, not orchestration. |

---

# State Machine

The worker loop is a finite state machine. Every state has defined entry conditions, actions, and exit transitions. There are no implicit states or fallthrough paths.

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

### SELECTING

**Entry:** Worker is ready for next bead. This is the strand waterfall entry point.

**Actions:**
1. Emit heartbeat
2. Evaluate strands in sequence
3. First strand that yields a candidate bead wins

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Candidate bead found | CLAIMING |
| All strands exhausted | EXHAUSTED |
| Shutdown signal received | STOPPED |

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

### HANDLING

**Entry:** Agent has exited (or was killed). This is where outcome routing happens.

**Actions:**
1. Classify outcome by exit code
2. Execute the handler for that outcome class
3. Re-read the bead after the agent process has terminated
4. If the bead is still `in_progress` and is still assigned to this dispatching
   worker, invoke the dedicated `resolve` prompt to determine the attempt's
   semantic outcome
5. Apply and verify the resulting bead transition
6. Emit `bead.outcome` telemetry

**Outcome Table:**

| Outcome | Exit Code | Handler | Bead Action |
|---------|-----------|---------|-------------|
| **Success** | 0 | Verify bead state. If it remains claimed by this worker, run Resolve. | Closed, released, blocked, or split |
| **Failure** | 1 | Evaluate for mitosis (see Mitosis section). If splittable, split and block parent. If not, release bead (`br update --status open --unassign`). Increment failure count via label. | Split or released |
| **Timeout** | 124 | Release bead. Add `deferred` label. | Released + deferred |
| **Crash** | >128 (signal) | Release bead. Create alert bead in workspace. | Released + alert |
| **Race Lost** | 4 | (Handled at CLAIMING, should not reach here) | N/A |
| **Interrupted** | — | Release bead. Clean shutdown. | Released |
| **Agent Not Found** | 127 | Release bead. Emit error. Do not retry (config issue). | Released |
| **Build Failure** | — | Release bead. Emit error. | Released |

**Agent-owned closure remains the normal path:** the Pluck agent is instructed
to close the bead itself. Resolve is an exception-recovery path and runs only
when the agent process has ended while the bead remains `in_progress` and
assigned to the same worker. It is not invoked for beads the agent already
closed, released, or blocked.

**Post-dispatch invariant:** once the dispatched agent process and any Resolve
pass have ended, that dispatch must not leave its bead `in_progress`. A Resolve
failure, timeout, invalid response, or failed state mutation falls back to a
best-effort release with an explicit `resolution_failed` reason. Ownership is
checked again before every mutation so a stale dispatch cannot alter a bead
that has since changed hands.

**Resolve decisions:**

| Decision | NEEDLE action |
|----------|---------------|
| `complete` | Run configured gates and shipped-work verification; close only if they pass, otherwise release as retryable |
| `retry` | Record a concise attempt result and retry guidance, increment the failure count, and release with normal retry/backoff policy |
| `blocked` | Record the concrete external prerequisite and move the bead to the backend's blocked state |
| `split` | Send the structured child proposal through Mitosis validation/deduplication and apply the parent/child dependency policy |

Resolve is advisory: NEEDLE owns validation and all bead mutations. The
resolver must not edit files, commit, push, or mutate bead state.

**Non-commit deliverables — decided 2026-08-29.** A bead's deliverable may be
something other than a commit: a GitHub comment, an external API call, a
provisioning step, a verification. Such beads are permitted, and they are
declared, never inferred:

- **Declaration is the label `deliverable:external`** on the bead
  (`bead create --label deliverable:external`, or `bead label add <id>
  deliverable:external` mid-dispatch). *Because* the implicit notes-hash
  fallback was invisible in practice: 14 consecutive dispatches of
  `needle-0fbf5145` closed with `--reason` only, the gate reopened it each
  time, and every reopen repeated the side effect (18 comments on GitHub #16).
- **Gate for labeled beads:** the git check is skipped; the gate PASSES iff the
  bead's notes changed during the dispatch **and** contain a line beginning
  `evidence:` (a URL, an identifier, or command output proving the effect).
  `close --reason` alone never satisfies it, and a commit is neither necessary
  nor sufficient. *Because* an unattended retry loop must be decided by a
  machine-checkable artifact, not prose.
- **Gate for unlabeled beads is unchanged:** a substantial commit, or the
  existing changed-note fallback for verification-only / already-done /
  blocked outcomes.
- **Prompt:** a labeled bead gets a dedicated block — check whether the effect
  already exists before acting (idempotency), act once, record `evidence:` with
  `bead update --notes`, then close. The default block tells an agent whose
  work turned out to be external to add the label rather than close without it.
  *Because* the label is the contract the gate reads; an agent that discovers
  the shape of the work mid-dispatch must be able to opt into it.
- **The reopen bound applies regardless of label:** a shipped-work failure
  counts toward the existing quarantine threshold and quarantine sets the bead
  `deferred` with a note (needle-b39fe1b6). *Because* a labeled bead whose
  agent never records evidence still loops, and its side effect repeats every
  cycle.
- **Exposure:** documented in `docs/configuration.md` and in the AGENTS.md
  section `needle init --backend` writes into a user's repo (needle-553bcb95).

**Transitions:**
| Condition | Next State |
|-----------|-----------|
| Outcome processed | LOGGING |

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

### STOPPED

**Entry:** Graceful shutdown.

**Actions:**
1. Release any claimed bead
2. Deregister from worker state registry
3. Stop heartbeat emitter
4. Emit `worker.stopped` telemetry
5. Exit process

**Transitions:** None (terminal).

### ERRORED

**Entry:** Unrecoverable error.

**Actions:**
1. Release any claimed bead (best-effort)
2. Emit `worker.errored` telemetry with error details
3. Deregister from worker state registry (best-effort)
4. Exit process with non-zero code

**Transitions:** None (terminal).

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

## Invariants

These must hold at all times. Violation of any invariant is a bug.

1. **A worker holds at most one claimed bead.** There is no pipelining or parallel execution within a single worker.

2. **A claimed bead is always released.** Every path through HANDLING releases the bead unless the agent closed it. There is no path where a bead remains claimed after the worker moves to SELECTING.

3. **Heartbeat is continuous.** From BOOTING to STOPPED/ERRORED, the worker emits heartbeats. A gap in heartbeats means the worker is stuck or dead.

4. **Telemetry is emitted for every state transition.** Silent transitions do not exist.

5. **The exclusion set is bounded.** It is cleared on every transition to SELECTING. It cannot grow unboundedly within a selection cycle because max_retries is finite.

6. **Shutdown is always graceful when possible.** SIGTERM triggers STOPPED, not ERRORED. Only SIGKILL causes ungraceful termination, and heartbeat TTL handles that case.

---

# Architecture

## Module Boundaries

NEEDLE is organized into crates (Rust) or packages (Go) with strict dependency rules. No circular dependencies. Each module has a single responsibility.

```
needle (binary)
├── cli/              CLI parsing, session management
├── worker/           Worker loop, state machine
├── strand/           Strand waterfall evaluation
│   ├── pluck.rs      Primary bead selection
│   ├── mend.rs       Stale claim cleanup, dependency repair
│   ├── explore.rs    Multi-workspace discovery
│   ├── weave.rs      Gap analysis, bead creation
│   ├── unravel.rs    Alternative proposals for HUMAN beads
│   ├── pulse.rs      Codebase health scans
│   ├── reflect.rs    Learning consolidation
│   ├── splice.rs     Worker failure documentation
│   └── knot.rs       Exhaustion alerting
├── claim/            Atomic claiming, lock management
├── prompt/           Prompt construction from bead context
├── dispatch/         Agent adapter loading, process execution
├── outcome/          Exit code classification, outcome handlers
├── commit_hook.rs    Bead-Id trailer injection for git commits
├── telemetry/        Structured event emission, sinks
│   └── otlp.rs       OpenTelemetry exporter
├── health/           Heartbeat, stuck detection, peer monitoring
├── config/           Hierarchical configuration loading
├── bead_store/       Abstract bead backend interface
├── types/            Shared types, error definitions
├── learning/         Retrospective extraction, learnings management
├── skill/            Skill library, retrieval, promotion
├── trace/            Trace capture, storage, retention
├── transcript/       Session JSONL parsing, action-outcome extraction
├── drift/            Session similarity, clustering, divergence detection
├── decision/         Decision point detection, ADR management
├── placement/        CLAUDE.md lowest-common-ancestor resolution
├── stats/            Aggregation engine, A/B comparison
├── supervisor/       Fleet supervisor daemon (auto-scale)
├── canary/           Release channel promotion, canary tests
├── upgrade/          Self-update, hot-reload, rollback
├── registry/         Worker state registry
├── rate_limit/       Provider/model concurrency and rate limiting
├── sanitize/         Trace sanitization (gitleaks)
├── validation/       Pluggable pre-closure validation gates
├── peer/             Peer monitoring and stale detection
├── mitosis/          Child-aware bead splitting
├── span/             W3C trace context utilities
├── cost/             Token extraction, pricing, cost tracking
├── agent_event.rs    Agent event telemetry utilities
├── claude_md_placement.rs  CLAUDE.md placement logic
└── routing.rs        Model-based adapter routing
```

### Dependency Graph

```
cli ──► worker ──► strand ──► bead_store
                │           │
                │           ├──► (all strand modules) ──► bead_store
                │           │                              ├──► telemetry
                │           │                              └──► health
                │           │
                │           └──► learning ──► bead_store
                │                             ├──► telemetry
                │                             ├──► transcript
                │                             ├──► drift
                │                             ├──► decision
                │                             ├──► skill
                │                             └──► trace
                │
                ├──► claim ──► bead_store, health
                │
                ├──► prompt ──► bead_store, skill, learning
                │
                ├──► dispatch ──► routing
                │
                ├──► outcome ──► bead_store, mitosis
                │
                ├──► commit_hook ──► types
                │
                ├──► telemetry ──► otlp
                │
                ├──► health ──► peer, registry
                │
                ├──► config ──► types
                │
                ├──► canary ──► worker, upgrade
                │
                ├──► upgrade ──► registry, health
                │
                ├──► sanitize ──► config
                │
                ├──► validation ──► bead_store
                │
                ├──► stats ──► telemetry
                │
                ├──► supervisor ──► registry, health, worker
                │
                ├──► cost ──► telemetry
                │
                └──► (other modules) ──► types

config ◄── (all modules)
types  ◄── (all modules)
```

**Rule:** Arrows point from dependent to dependency. No module depends on `cli` or `worker` except through the binary entry point. `telemetry`, `config`, and `types` are leaf dependencies available to all modules.

## Data Flow

### Primary Loop

```
bead_store ──[candidates]──► strand ──[bead_id]──► claim ──[claimed_bead]──►
prompt ──[prompt_string]──► dispatch ──[process]──► worker(wait) ──►
outcome ──[result]──► bead_store + telemetry
```

### Telemetry Flow

```
                    ┌─────────────────────────┐
                    │    Telemetry Collector   │
                    └────────────┬────────────┘
                                 │
              ┌──────────────────┼──────────────────┐
              ▼                  ▼                   ▼
        ┌──────────┐     ┌────────────┐      ┌──────────┐
        │ File Sink│     │ Stdout Sink│      │ Hook Sink│
        │ (JSONL)  │     │ (human)    │      │ (webhook)│
        └──────────┘     └────────────┘      └──────────┘
```

### Configuration Flow

```
  CLI args ──► env vars ──► workspace .needle.yaml ──► global ~/.needle/config.yaml ──► defaults
  (highest)                                                                              (lowest)
```

## Module Specifications

### bead_store

Abstract interface to any bead backend. The primary implementation wraps the `br` CLI, but the trait allows future backends (direct SQLite, HTTP API, etc.).

```
trait BeadStore {
    fn ready(workspace: &Path, filters: &Filters) -> Result<Vec<Bead>>
    fn show(id: &BeadId) -> Result<Bead>
    fn claim(id: &BeadId, actor: &str) -> Result<ClaimResult>
    fn release(id: &BeadId) -> Result<()>
    fn labels(id: &BeadId) -> Result<Vec<String>>
    fn add_label(id: &BeadId, label: &str) -> Result<()>
    fn doctor_repair() -> Result<RepairReport>
}

enum ClaimResult {
    Claimed(Bead),
    RaceLost { claimed_by: String },
    NotClaimable { reason: String },
}
```

**Design notes:**
- All methods return `Result`. Silent failures do not exist.
- `ClaimResult` is an enum, not a boolean. The caller must handle each variant.
- `ready()` accepts filters (status, assignee, labels, workspace) to push filtering to the backend.
- The `br` CLI implementation shells out to `br` with `--json` and parses output via `serde_json`.
- JSON parsing failures are explicit errors, not empty results (learned from starvation false positives).

### claim

Wraps `bead_store.claim()` with workspace-level serialization and retry logic.

```
struct Claimer {
    bead_store: Box<dyn BeadStore>,
    lock_dir: PathBuf,       // per-workspace flock directory
    max_retries: u32,        // default: 5
    retry_backoff_ms: u64,   // default: 100
}

impl Claimer {
    fn claim_next(
        &self,
        candidates: &[Bead],
        actor: &str,
        exclusions: &HashSet<BeadId>,
    ) -> Result<ClaimOutcome>
}

enum ClaimOutcome {
    Claimed(Bead),
    AllRaceLost,
    NoCandidates,
    StoreError(Error),
}
```

**Design notes:**
- The flock is per-workspace, not per-bead. This serializes all claim operations within a workspace, preventing thundering herd (learned from `docs/notes/claim-race-conditions.md`).
- The lock is held only for the duration of the `br update --claim` call, not for the entire bead execution.
- Retry logic is internal to the Claimer. The caller receives a final `ClaimOutcome`.

### strand

Evaluates the strand waterfall and returns the next action.

```
trait Strand {
    fn name(&self) -> &str
    fn enabled(&self, config: &Config) -> bool
    fn evaluate(&self, context: &WorkerContext) -> Result<StrandResult>
}

enum StrandResult {
    BeadFound(Vec<Bead>),    // candidates for claiming
    WorkCreated,              // strand created new beads (e.g., weave)
    NoWork,                   // fall through to next strand
    Error(StrandError),       // strand failed, fall through
}
```

Each strand implements the trait. The runner evaluates them in order:

```
fn run_strands(strands: &[Box<dyn Strand>], ctx: &WorkerContext) -> StrandWaterfallResult {
    for strand in strands {
        if !strand.enabled(&ctx.config) { continue; }
        match strand.evaluate(ctx)? {
            StrandResult::BeadFound(candidates) => return Ok(candidates),
            StrandResult::WorkCreated => return Ok(/* re-evaluate from strand 1 */),
            StrandResult::NoWork => continue,
            StrandResult::Error(e) => { emit_telemetry(e); continue; }
        }
    }
    StrandWaterfallResult::Exhausted
}
```

### dispatch

Loads agent adapters and executes the agent process.

```
struct Dispatcher {
    adapters: HashMap<String, AgentAdapter>,
}

struct AgentAdapter {
    name: String,
    invoke_template: String,   // e.g., "cd {workspace} && claude --print"
    input_method: InputMethod,  // Stdin, File, Args
    timeout: Duration,
    environment: HashMap<String, String>,
}

enum InputMethod {
    Stdin,                      // pipe prompt to stdin
    File { path_template: String },  // write prompt to file, pass path
    Args { flag: String },      // pass prompt as --flag value
}

struct ExecutionResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
    elapsed: Duration,
    pid: u32,
}
```

**Design notes:**
- Adapters are loaded from YAML config files. Adding a new agent requires only a new YAML file.
- The invoke template is rendered with variables: `{workspace}`, `{prompt_file}`, `{bead_id}`, `{model}`.
- The dispatcher does not interpret agent output. It captures raw exit code, stdout, and stderr, then passes them to the outcome handler.
- Timeout is enforced by the dispatcher, not the agent. If the agent exceeds the timeout, the dispatcher kills the process and returns exit code 124.

### Model-based adapter routing

NEEDLE supports dynamic adapter selection based on model names. This enables policy-driven routing such as directing Anthropic subscription models to a specific adapter.

**Historical context:** Anthropic's Agent SDK credit split occurred on June 15, 2026. Before this date, `claude -p` commands used subscription credits; after, they consumed API credits. To maximize subscription value before the deadline, Anthropic Claude models (sonnet, opus, fable, haiku) were routed to `claude-print`, while other models defaulted to `claude-code-glm-4.7`. The routing feature shipped prior to this deadline (tracked by bead bf-2xi) and remains available for similar policy-driven use cases.

**Configuration schema:**

```yaml
agent:
  default: claude  # fallback when no routing rules match
  routing:
    rules:
      - match_model: "(claude-)?(sonnet|opus).*"
        adapter: claude-print
      - match_model: "(claude-)?(fable|haiku).*"
        adapter: claude-print
      - match_model: "glm-4.*"
        adapter: claude-code-glm-4.7
    default_adapter: claude-code-glm-4.7  # fallback when no rule matches
    strict: false  # if true, fail dispatch when no rule matches
```

**Routing logic:**
1. Rules are evaluated in order; first match wins.
2. Pattern matching uses regex against the model name only (no provider prefix).
3. If no rule matches and `strict: false`, use `default_adapter` or fall back to `agent.default`.
4. If no rule matches and `strict: true`, emit `RoutingFailed` telemetry and fail the dispatch.

**Telemetry events:**
- `RoutingDecision`: emitted when a routing rule matches (bead_id, model, matched_rule, chosen_adapter).
- `RoutingFailed`: emitted in strict mode when no rule matches (bead_id, model, rules_tried).

**Workspace overrides:**
Routing rules can be overridden per-workspace via `.needle.yaml`:

```yaml
# workspace-specific routing
agent:
  routing:
    rules:
      - match_model: "claude-sonnet.*"
        adapter: workspace-sonnet-adapter
    default_adapter: workspace-fallback
```

**Default behavior:**
The built-in default routes Anthropic Claude models to `claude-print` and everything else to `claude-code-glm-4.7`. Workspaces can override this by defining their own `agent.routing` section.

### Anthropic Subscription Billing Policy (Pre-June 15, 2026)

**Historical context:** On June 15, 2026, Anthropic's credit split changed. Before this date, the `claude -p` flag (which enables the `--print` adapter) consumed subscription credits. After the deadline, `-p` switched to consuming API credits.

**Routing policy rationale:** To maximize subscription credit value before the June 15 deadline, the default routing configuration was designed to route Anthropic Claude subscription models to the `claude-print` adapter:

```yaml
agent:
  default: claude
  routing:
    rules:
      # Route all Anthropic Claude subscription models to claude-print
      # Patterns match: claude-sonnet-4-6, claude-opus-4-6, claude-fable-5, claude-haiku-4-5-20251001
      # Also matches without prefix: sonnet-4-6, opus-4-6, fable-5, haiku-4-5
      - match_model: "(claude-)?(sonnet|opus|fable|haiku).*"
        adapter: claude-print
    default_adapter: claude-code-glm-4.7  # Non-Anthropic models
    strict: false
```

**Why this matters:**
- **Subscription value maximization:** Anthropic Claude models (Sonnet, Opus, Fable, Haiku) used subscription credits when invoked via `claude-print` before June 15, 2026.
- **Cost optimization:** Non-Anthropic models (GLM, GPT-4, Gemini, etc.) default to `claude-code-glm-4.7` to use API credits or other billing mechanisms.
- **Workspace flexibility:** Individual workspaces can override these defaults by defining their own `agent.routing` section in `.needle.yaml`.

**Example .needle.yaml configuration:**

```yaml
# .needle.yaml workspace configuration
agent:
  default: claude
  timeout: 3600
  routing:
    rules:
      - match_model: "(claude-)?(sonnet|opus).*"
        adapter: claude-print
      - match_model: "(claude-)?(fable|haiku).*"
        adapter: claude-print
      - match_model: "glm-.*"
        adapter: claude-code-glm-4.7
    default_adapter: claude-code-glm-4.7
    strict: false
```

**Post-June 15 behavior:** After the deadline, workspaces may want to update their routing configuration since `claude-print` no longer provides subscription billing advantages. The routing system remains flexible to accommodate whatever billing optimization strategies emerge.

### outcome

Classifies the agent's exit and routes to the appropriate handler.

```
enum Outcome {
    Success,           // exit 0
    Failure,           // exit 1
    Timeout,           // exit 124 (set by dispatcher)
    Crash(i32),        // exit >128 (signal)
    AgentNotFound,     // exit 127
    Interrupted,       // shutdown signal during execution
}

fn classify(result: &ExecutionResult, was_interrupted: bool) -> Outcome {
    if was_interrupted { return Outcome::Interrupted; }
    match result.exit_code {
        0   => Outcome::Success,
        1   => Outcome::Failure,
        124 => Outcome::Timeout,
        127 => Outcome::AgentNotFound,
        c if c > 128 => Outcome::Crash(c),
        _   => Outcome::Failure,  // treat unknown codes as failure
    }
}
```

**Design notes:**
- The match is exhaustive. Every exit code maps to exactly one outcome.
- The `Outcome` enum is the sole input to the handler. There is no ad-hoc exit code checking elsewhere.
- `Outcome::Success` does NOT mean the bead is closed. It means the agent exited cleanly.
- Exit classification and semantic resolution are separate. Semantic Resolve
  is considered only after the process exits and only if the bead remains
  `in_progress` under the dispatching worker's ownership.

### telemetry

```
fn emit(event: TelemetryEvent)

struct TelemetryEvent {
    timestamp: DateTime<Utc>,
    worker_id: String,
    event_type: String,       // e.g., "bead.claim.attempted"
    bead_id: Option<BeadId>,
    workspace: Option<PathBuf>,
    data: serde_json::Value,  // event-specific payload
    duration_ms: Option<u64>,
    trace_id: Option<TraceId>,   // W3C trace ID of enclosing span, if OTLP sink enabled
    span_id: Option<SpanId>,     // W3C span ID of enclosing span, if OTLP sink enabled
}

trait Sink: Send + Sync {
    fn accept(&self, event: &TelemetryEvent) -> Result<()>;
    fn flush(&self, deadline: Duration) -> Result<()>;
}

// Built-in sinks: FileSink, StdoutSink, HookSink, OtlpSink.
// OtlpSink wraps the OpenTelemetry SDK (traces + metrics + logs providers)
// and translates TelemetryEvent into the appropriate signal per the
// Semantic Mapping table in the Telemetry chapter.
```

### health

```
struct HealthMonitor {
    heartbeat_interval: Duration,    // default: 30s
    heartbeat_ttl: Duration,         // default: 5min
    heartbeat_dir: PathBuf,          // ~/.needle/state/heartbeats/
    peer_check_interval: Duration,   // default: 60s
}

impl HealthMonitor {
    fn emit_heartbeat(&self, state: &WorkerState) -> Result<()>
    fn check_peers(&self) -> Result<Vec<PeerStatus>>
    fn cleanup_stale_claims(&self, store: &dyn BeadStore) -> Result<u32>
}

enum PeerStatus {
    Alive { last_seen: DateTime<Utc>, current_bead: Option<BeadId> },
    Stale { last_seen: DateTime<Utc>, claimed_bead: Option<BeadId> },
    Dead { heartbeat_file: PathBuf },
}
```

### commit_hook

Injects `Bead-Id:` trailers into git commits after successful bead closure, for HOOP bead_commit_index integration.

```
async fn inject_bead_id_trailer(
    workspace: &Path,
    bead_id: &BeadId,
    pre_dispatch_head: &str,
) -> Result<()>

fn git_head(workspace: &str) -> Result<String>
```

**Functionality:**
- When a bead closes with a commit artifact (agent made commits), NEEDLE amends the latest commit to include a `Bead-Id: <id>` trailer
- Only acts when HEAD moved since `pre_dispatch_head` (i.e. the agent made at least one commit)
- Idempotent: checks if trailer is already present before injecting
- Returns `Ok(())` in all no-op cases (not a git repo, no new commits, trailer already present)

**Trailer format:**
```
Bead-Id: nd-a3f8
```

**Integration context:**
- HOOP's `bead_commit_index` picks up the trailer via `git log --format=%(trailers:key=Bead-Id,valueonly,separator=,)`
- Enables bidirectional traceability: beads → commits and commits → beads
- Executed after successful bead close, before returning to SELECTING state

**Timeouts:**
- `git rev-parse HEAD`: 10s timeout
- `git commit --amend`: 30s timeout

**Design notes:**
- Errors are logged as non-fatal warnings; they do not fail the bead processing cycle
- The trailer is appended via `git commit --amend --no-edit --trailer "Bead-Id: <id>"`
- Multiple beads can be tracked in a single commit (comma-separated in trailer value)

## Binary Structure

**Do not cp/mv onto spawn-path binaries while workers run.** Replacing `~/.local/bin/needle` or `~/.needle/bin/needle-stable` in-place while any worker is running is unsupported. This produces two failure modes: session disruption (active worker crashes) or permanent hot-reload stall (new binary never loads, fixed by P11.1). Use `needle upgrade` for atomic updates instead.

NEEDLE is a single binary with subcommands:

```
needle run [--workspace PATH] [--agent NAME] [--count N] [--identifier NAME] [--timeout SECONDS] [--resume] [--hot-reload]
needle stop [--all | --identifier NAME]
needle cleanup [--all | --identifier NAME]
needle list [--format table|json]
needle attach <identifier>
needle status [--format table|json] [--by-worker] [--cost] [--since TIME] [--until TIME] [--idle-strands]
needle logs [--follow] [--filter EXPR] [--since TIME] [--until TIME] [--format table|json]
needle config [--get KEY] [--set KEY=VALUE] [--dump] [--show-source]
needle doctor [--repair] [--workspace PATH]
needle init
needle version
needle test-agent <name>
needle canary [--status]
needle upgrade [--check]
needle rollback
needle reflect [--workspace PATH] [--force]
needle update-rules [--output PATH]
needle stats [--by template_version|task_type|worker] [--since TIME] [--until TIME] [--format table|json]
needle supervise [--workspace PATH]
```

### Session Management

`needle run` creates tmux sessions for each worker. Session naming follows the pattern:

```
needle-{agent}-{provider}-{model}-{identifier}
```

Examples:
```
needle-claude-anthropic-sonnet-alpha
needle-opencode-alibaba-qwen-bravo
needle-codex-openai-gpt4-charlie
```

`--count=N` launches N workers with sequential NATO alphabet identifiers (alpha, bravo, charlie, ...). Workers are launched with staggered delay (default: 2s between launches) to prevent thundering herd on startup (learned from `docs/notes/operational-fleet-lessons.md`).

### CLI Help System

Every subcommand and flag is discoverable via `--help` or `-h`. Help text is embedded in the binary and generated from the same source as the CLI parser (e.g., `clap` derive macros in Rust).

**Top-level help:**

```
$ needle --help

NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort

Deterministic bead processing with explicit outcome paths.

Usage: needle <COMMAND>

Commands:
  run          Launch worker(s) to process beads
  stop         Stop running worker(s)
  cleanup      Remove orphaned tmux sessions
  list         List active workers
  attach       Attach to a worker's tmux session
  status       Show fleet status, bead counts, and cost summary
  logs         View and query telemetry logs
  config       View or inspect configuration
  doctor       Check system health and repair
  init         Initialize v2 config with optional v1 migration
  version      Show version information
  test-agent   Validate an agent adapter
  canary       Run canary tests against a :testing binary
  upgrade      Check for and install updates from GitHub releases
  rollback     Roll back to the previous :stable binary
  reflect      Run learning consolidation on demand
  update-rules Fetch the latest gitleaks rules and update the vendored config
  stats        Show outcome statistics aggregated from telemetry logs
  supervise    Run the fleet supervisor daemon (auto-scale workers based on queue depth)
  help         Print this message or the help of a subcommand

Options:
  -h, --help     Print help
  -V, --version  Print version
```

**Subcommand help (example):**

```
$ needle run --help

Launch worker(s) to process beads

Usage: needle run [OPTIONS]

Options:
  -w, --workspace <PATH>     Workspace to process beads from [default: config value]
  -a, --agent <NAME>         Agent adapter to use [default: config value]
  -c, --count <N>            Number of workers to launch [default: 1]
  -i, --identifier <NAME>    Worker identifier (overrides NATO naming)
  -t, --timeout <SECONDS>    Agent execution timeout [default: config value]
      --resume               Resume an existing worker session (used by hot-reload)
  -h, --help                 Print help
```

**Design notes:**
- Every flag has a one-line description
- Default values shown in brackets (sourced from config)
- Subcommand grouping follows the lifecycle: launch → monitor → operate → maintain
- `needle help <command>` and `needle <command> --help` are equivalent
- Help output is plain text, no colors, suitable for piping to other tools or agents

---

# Strand Waterfall

Strands are NEEDLE's strategy for finding work. They are evaluated in strict sequence — the first strand that yields actionable work wins. When a strand returns `NoWork`, the worker falls through to the next.

The waterfall is the answer to "what does a worker do when it has no beads?" It is not a priority system for beads (that's handled by deterministic ordering within each strand). It is a priority system for *strategies*.

## Waterfall Sequence

```
  Strand 1: PLUCK ──── primary work from assigned workspace
       │ no work
       ▼
  Strand 2: MEND ───── cleanup: stale claims, orphaned locks, health
       │ nothing to clean
       ▼
  Strand 3: EXPLORE ── look for work in other configured workspaces
       │ no work
       ▼
  Strand 4: WEAVE ──── create beads from documentation gaps (opt-in)
       │ no gaps or disabled
       ▼
  Strand 5: UNRAVEL ── propose alternatives for HUMAN-blocked beads (opt-in)
       │ none or disabled
       ▼
  Strand 6: PULSE ──── codebase health scan, auto-generate beads (opt-in)
       │ no issues or disabled
       ▼
  Strand 7: REFLECT ── consolidate learnings from recent beads
       │ consolidation complete or not needed
       ▼
  Strand 8: SPLICE ──── worker failure documentation
       │ no failures
       ▼
  Strand 9: KNOT ───── alert human, enter backoff
       │
       ▼
  → EXHAUSTED (backoff and retry from Strand 1)
```

## Strand 1: Pluck

**Purpose:** Process beads from the worker's assigned workspace. This is the primary work strand and will handle >90% of all bead processing.

**Invokes agent:** Yes.

**Entry condition:** Worker has an assigned workspace with a `.beads/` directory.

**Algorithm:**
1. Query bead store: `br ready --unassigned --json` in workspace
2. Filter: exclude beads with labels `deferred`, `human`, `blocked`
3. Filter: exclude beads in the current retry exclusion set
4. Sort: priority (ascending, 0 = highest), then creation time (ascending, oldest first)
5. Return sorted candidates for claiming

**Exit conditions:**
| Result | Action |
|--------|--------|
| Candidates found | Return `BeadFound(candidates)` → worker proceeds to CLAIMING |
| No candidates (queue empty) | Return `NoWork` → fall through to Strand 2 |
| Bead store error | Emit telemetry, return `Error` → fall through to Strand 2 |

**Determinism guarantee:** The sort key `(priority, created_at)` produces the same ordering for all workers viewing the same queue state. Workers will compete for the same top-priority bead, and the claim mechanism resolves contention.

### Post-Pluck Resolve pass

Resolve is a bounded follow-up dispatch attached to Pluck outcome handling,
not a general waterfall strand. It is triggered only when all three conditions
hold after the Pluck agent terminates:

1. the bead is still `in_progress`;
2. its assignee still matches the dispatching worker; and
3. no agent subprocess is still working on that bead.

Before invoking an agent, NEEDLE performs deterministic inspection. Outcomes
that are already unambiguous (for example, an interrupted process that should
be released) do not require Resolve. For an ambiguous claimed bead, the
resolver receives the bead and acceptance criteria, the prior agent's final
output, exit reason, pre/post-dispatch Git state, commits and diff summary,
validation results, failure history, and a bounded trace tail.

The resolver returns exactly one structured decision: `complete`, `retry`,
`blocked`, or `split`, with evidence and decision-specific fields. NEEDLE
parses this response strictly, validates it against current repository and
bead state, and performs the transition. Resolve runs at most once per Pluck
dispatch. If it cannot produce a valid decision within its configured timeout,
NEEDLE records the failure and releases the bead.

## Strand 2: Mend

**Purpose:** Maintenance and cleanup operations that keep the bead store healthy. Runs before Explore because cleaning up stale claims or broken dependencies in the home workspace may unblock beads here — no need to roam if local work is just stuck.

**Invokes agent:** No.

**Entry condition:** Strand 1 returned no work.

**Algorithm:**
1. **Stale claim cleanup:** Find beads with status `in_progress` where the assigned worker has no active heartbeat (TTL expired). Release them.
2. **Orphaned lock cleanup:** Find workspace lock files older than TTL. Remove them.
3. **Dependency cleanup:** Find closed beads that are still listed as blockers on open beads. Remove the stale dependency links.
4. **Database health:** Run `br doctor` (not `--repair` unless errors found).

**Exit conditions:**
| Result | Action |
|--------|--------|
| Cleanup performed | Return `WorkCreated` → restart from Strand 1 (released beads may now be claimable) |
| Nothing to clean | Return `NoWork` → fall through to Strand 3 |

**Design notes (from `docs/notes/bead-lifecycle-bugs.md`):**
- Stale dependency links caused permanent blocking in NEEDLE-deprecated. Mend must clean these.
- Distinguish "did work" from "found nothing" — v1 had an infinite loop where mend returned success on failed releases.

## Strand 3: Explore

**Purpose:** Discover work in other configured workspaces when the home workspace is empty and clean.

**Invokes agent:** No. Explore only finds candidates — execution happens back through the standard CLAIMING → DISPATCHING flow.

**Entry condition:** Strands 1-2 returned no work. Explore is enabled in config. At least one additional workspace is configured.

**Algorithm:**
1. Read configured workspace list from config (explicit paths, no filesystem scanning)
2. For each workspace (in configured order):
   a. Check `.beads/` directory exists
   b. Query `br ready --unassigned --json`
   c. If candidates found, return them with workspace context
3. If no workspace has work, return `NoWork`

**Exit conditions:**
| Result | Action |
|--------|--------|
| Candidates found in another workspace | Return `BeadFound(candidates)` with workspace override |
| No candidates in any workspace | Return `NoWork` → fall through to Strand 4 |

**Design notes (from `docs/notes/explore-strand-bugs.md`):**
- **No filesystem scanning.** NEEDLE-deprecated's `find`-based discovery caused 35+ CPU load with 40 workers. Workspaces must be explicitly configured.
- **No upward traversal.** The v1 explore strand walked up parent directories to `/home`, then `/`. This is eliminated.
- **Workspace list is static** for the duration of a session. It is read from config at boot and not re-evaluated.
- **Workers do not permanently relocate.** If a worker finds work in another workspace, it processes that bead and returns to its home workspace for the next cycle.

## Strand 4: Weave (opt-in)

**Purpose:** Analyze workspace documentation for gaps and create new beads to address them.

**Invokes agent:** Yes — uses the agent to analyze documentation and propose beads.

**Entry condition:** Strands 1-3 (Pluck, Mend, Explore) returned no work. Weave is explicitly enabled in workspace config (`strands.weave.enabled: true`).

**Algorithm:**
1. Identify documentation files (README, AGENTS.md, docs/, etc.)
2. Dispatch agent with gap-analysis prompt
3. Agent proposes new beads (as structured output)
4. Create beads via bead store
5. Return `WorkCreated` → restart from Strand 1

**Guardrails (from `docs/notes/self-modification-risks.md`):**
- **Max beads per weave run:** Configurable, default 5. Prevents unbounded bead creation.
- **Cooldown period:** Minimum time between weave runs per workspace, default 24h.
- **Seen-issues deduplication:** Track previously created weave beads to prevent duplicates.
- **Workspace exclusion:** Weave is disabled for NEEDLE's own workspace by default. Workers must not create work for their own orchestrator without human approval.
- **Human review label:** Weave-created beads are labeled `weave-generated` for easy filtering.

**Exit conditions:**
| Result | Action |
|--------|--------|
| Beads created | Return `WorkCreated` → restart from Strand 1 |
| No gaps found | Return `NoWork` → fall through to Strand 5 |
| Disabled | Return `NoWork` → fall through to Strand 5 |

## Strand 5: Unravel (opt-in)

**Purpose:** For beads labeled `human` (requiring human decision), propose alternative approaches that an agent could execute instead.

**Invokes agent:** Yes — uses the agent to analyze the blocked bead and propose workarounds.

**Entry condition:** Strands 1-4 returned no work. Unravel is explicitly enabled. There are beads with `human` label in the workspace.

**Algorithm:**
1. Query beads with `human` label
2. For each (up to `max_unravel_per_run`, default 3):
   a. Dispatch agent with the bead context and a prompt asking for alternative approaches
   b. If agent proposes viable alternatives, create child beads with `alternative` label
   c. Do NOT close or modify the original `human` bead
3. Return `WorkCreated` if alternatives were created

**Guardrails:**
- Original `human` bead is never modified or closed
- Alternative beads are linked as children (informational, not blocking)
- Max alternatives per `human` bead: configurable, default 2
- Cooldown: don't re-analyze a `human` bead that was analyzed within the last 7 days

**Exit conditions:**
| Result | Action |
|--------|--------|
| Alternatives created | Return `WorkCreated` → restart from Strand 1 |
| No `human` beads or no alternatives viable | Return `NoWork` → fall through to Strand 6 |
| Disabled | Return `NoWork` → fall through to Strand 6 |

## Strand 6: Pulse (opt-in)

**Purpose:** Scan the codebase for health issues (stale TODOs, missing tests, dependency drift, linting) and create beads for significant findings.

**Invokes agent:** Yes — uses the agent (or external tools) to scan the codebase.

**Entry condition:** Strands 1-5 returned no work. Pulse is explicitly enabled. Cooldown has expired.

**Algorithm:**
1. Run configured scanners (linters, test coverage, dependency checkers, TODO scanners)
2. Compare results against previous scan (stored in `~/.needle/state/pulse/`)
3. For new issues exceeding severity threshold, create beads
4. Update last-scan state

**Guardrails:**
- **Max beads per pulse run:** Configurable, default 10
- **Cooldown:** Default 48h between scans
- **Severity threshold:** Only create beads for issues above configured severity
- **Deduplication:** Track seen issues to prevent duplicate beads across scans
- **Workspace exclusion:** Same as Weave — disabled for NEEDLE's own workspace by default

**Exit conditions:**
| Result | Action |
|--------|--------|
| Beads created | Return `WorkCreated` → restart from Strand 1 |
| No new issues | Return `NoWork` → fall through to Strand 7 |
| Disabled | Return `NoWork` → fall through to Strand 7 |

## Strand 7: Reflect (opt-in)

**Purpose:** Consolidate learnings from recent bead work into a shared knowledge base (`.beads/learnings.md`), promoting recurring patterns to reusable skill files. This meta-analysis strand ensures that insights from completed work are captured and made available to future work.

**Invokes agent:** Yes — uses a consolidation-specific prompt.

**Entry condition:** Strands 1-6 returned no work. Reflect is explicitly enabled in workspace config (`strands.reflect.enabled: true`). At least N beads have been closed since last consolidation (configurable, default: 10). At least T hours since last consolidation (configurable, default: 24).

**Algorithm (KAIROS-inspired four-phase cycle):**

1. **Orient:** Read current `.beads/learnings.md` and existing skill files in `.beads/skills/`. Check file sizes to ensure they're within bounds.
2. **Gather:** Read bead close bodies from `.beads/issues.jsonl` for beads closed since last consolidation. Read available traces for failed beads to capture failure patterns.
3. **Consolidate:**
   - Extract retrospective blocks from close bodies
   - Identify patterns across multiple beads (same failure mode, same codebase quirk, workaround that works)
   - Merge new learnings into `.beads/learnings.md`, deduplicating against existing entries
   - Convert relative references to absolute (bead IDs, dates)
   - If a learning appears 3+ times across different beads, promote to a skill file in `.beads/skills/`
   - If a learning contradicts an existing entry, resolve in favor of the newer evidence
4. **Prune:**
   - Remove entries older than 90 days without reinforcement
   - Compress similar entries into single entries
   - Ensure total learnings stay under 80 entries (configurable)

**Guardrails:**
- **Cooldown:** Minimum 24 hours between consolidation runs (configurable)
- **Max learnings created per run:** 10 (configurable)
- **Max skills promoted per run:** 3 (configurable)
- **Read-only on CLAUDE.md:** The consolidation agent receives the workspace CLAUDE.md as context but MUST NOT modify it. CLAUDE.md changes require explicit human approval.
- **Workspace exclusion:** Reflect is disabled for NEEDLE's own workspace by default. Workers must not modify their own orchestrator's knowledge base without human approval.

**Exit conditions:**
| Result | Action |
|--------|--------|
| Consolidation performed | Return `WorkCreated` → restart from Strand 1 (learnings may unblock new work) |
| Not enough data since last run | Return `NoWork` → fall through to Strand 8 |
| Disabled or cooldown active | Return `NoWork` → fall through to Strand 8 |

**Telemetry:**
| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `reflect.started` | Consolidation starts | `beads_since_last`, `current_learnings_count` |
| `reflect.consolidated` | Consolidation completes | `learnings_added`, `learnings_pruned`, `skills_promoted`, `contradictions_resolved` |
| `reflect.skipped` | Consolidation skipped | `reason` (cooldown, insufficient data, disabled) |

**Learning persistence:** `.beads/learnings.md` is the authoritative source. Workers automatically append it to `context_files` during prompt building (via the Config module's `context_files` discovery). No manual config required — workers always see the latest learnings.

**Session transcript analysis:** Reflect analyzes both bead close bodies (structured outcomes) and Claude Code session transcripts (decision-making process) when available. Transcripts are stored as JSONL in `.claude/projects/<project-hash>/<session-uuid>.jsonl` and contain richer signal: failed attempts, tool call sequences, recovery strategies, and decision points.

**Integration with prompt building:** If `.beads/learnings.md` exists in a workspace, NEEDLE automatically appends it to `context_files` during prompt building. Workers always see the latest learnings without manual configuration.

## Strand 8: Splice

**Purpose:** Document worker failures (crashed workers and live-but-looping workers) by creating failure beads in a configured report workspace.

**Invokes agent:** No.

**Entry condition:** Strand 7 returned no work. Splice is enabled in config (default: true). Heartbeat files exist in the heartbeat directory.

**Algorithm:**

1. **Scan for dead workers (stale heartbeat + dead tmux session):**
   - Read all heartbeat files from the heartbeat directory
   - For each heartbeat older than `stale_threshold_secs` (default: 300s):
     - Check if the tmux session is still alive
     - If session is dead, this is a failed worker

2. **Scan for live-but-looping workers (optional, when `detect_live_loops: true`):**
   - For each worker with a fresh heartbeat:
     - Read the tail of their JSONL log file
     - Detect three loop patterns:
       - **Claim churn:** Repeated `bead.claim.race_lost` events for the same bead
       - **State ping-pong:** Rapid cycling between a small set of states without forward progress
       - **Log runaway:** Excessive JSONL growth without `agent.completed` events

3. **Create failure beads:**
   - For each undocumented failure or loop pattern:
     - Create a bead in the configured `report_workspace`
     - Include worker details, session info, and evidence in the bead body
   - Track documented sessions/loops in `splice_state.json` to prevent duplicates

4. **Return `WorkCreated` if any beads were created, otherwise `NoWork`**

**Exit conditions:**
| Result | Action |
|--------|--------|
| Failure beads created | Return `WorkCreated` → restart from Strand 1 |
| No new failures detected | Return `NoWork` → fall through to Strand 9 |

**State persistence:** `splice_state.json` tracks which session IDs and loop patterns have already been documented, preventing duplicate failure beads for the same dead/looping worker.

**Loop detection thresholds:**
- `claim_churn_threshold`: Number of race-lost events for same bead before flagging (default: 20)
- `log_runaway_bytes`: Max JSONL growth in `live_loop_window_secs` without completion (default: 10 MiB)
- `live_loop_window_secs`: Time window for log runaway check (default: 300s)

## Strand 9: Knot

**Purpose:** All work-finding strategies are exhausted. Alert the human and enter backoff.

**Invokes agent:** No.

**Entry condition:** Strands 1-8 all returned `NoWork`.

**Algorithm:**
1. Determine alert state:
   - **First time exhausted:** Emit `worker.idle` telemetry. Start backoff timer.
   - **Repeated exhaustion (>N cycles):** Create alert bead (if not already created within cooldown).
2. Verify before alerting (three-state check):
   a. **No beads exist:** Queue is genuinely empty. Normal idle.
   b. **All beads claimed:** Other workers are busy. Normal contention. Wait.
   c. **Beads invisible:** Configuration error (wrong workspace, broken filter). Alert.
3. Return `NoWork` → worker enters EXHAUSTED state with backoff

**Guardrails (from `docs/notes/worker-starvation-lessons.md`):**
- **Verify independently before alerting.** The v1 system had 100% false positive rate because it used the same broken code path for verification.
- **Three-state model.** "No work" is three different conditions with different responses. Conflating them caused the false positive spiral.
- **Rate limit alerts:** Max 1 alert bead per workspace per hour.
- **Alert includes diagnostics:** Bead counts, worker count, claimed count, config snapshot.

**Exit conditions:**
| Result | Action |
|--------|--------|
| Always | Return `NoWork` → EXHAUSTED state |

## Strand Configuration

```yaml
# ~/.needle/config.yaml or .needle.yaml
strands:
  pluck:
    enabled: true           # always on, cannot be disabled
  explore:
    enabled: true
    workspaces:             # explicit list, no auto-discovery
      - /home/coder/project-a
      - /home/coder/project-b
  mend:
    enabled: true
    stale_claim_ttl: 300    # seconds before a claimed bead is considered stale
    lock_ttl: 600           # seconds before an orphaned lock is removed
  weave:
    enabled: false          # opt-in
    max_beads_per_run: 5
    cooldown_hours: 24
    exclude_workspaces: []  # workspaces where weave is forbidden
  unravel:
    enabled: false          # opt-in
    max_per_run: 3
    cooldown_days: 7
  pulse:
    enabled: false          # opt-in
    max_beads_per_run: 10
    cooldown_hours: 48
    severity_threshold: warning
    scanners:
      - name: todo-scanner
        command: "grep -rn 'TODO\\|FIXME' {workspace}/src"
      - name: test-coverage
        command: "cargo tarpaulin --skip-clean -o json"
  reflect:
    enabled: true           # on by default (unlike weave/unravel/pulse)
    min_beads_since_last: 10   # minimum closed beads before consolidation
    cooldown_hours: 24
    max_learnings_per_run: 10
    max_skills_per_run: 3
    learning_retention_days: 90
    max_learnings: 80
  splice:
    enabled: true           # always on, cannot be disabled
    stale_threshold_secs: 300   # seconds before heartbeat considered stale
    report_workspace: null      # workspace for failure beads (null = current store)
    detect_live_loops: true     # scan for stuck workers in JSONL tail
    live_loop_scan_events: 200  # max events to scan per worker
    claim_churn_threshold: 20   # race-lost events for same bead before flagging
    log_runaway_bytes: 10485760 # 10 MiB, max growth without completion
    live_loop_window_secs: 300   # time window for log runaway check
  knot:
    enabled: true           # always on, cannot be disabled
    alert_cooldown_minutes: 60
    exhaustion_threshold: 3 # cycles before creating alert bead
```

---

# Mitosis

Mitosis is NEEDLE's mechanism for splitting a bead that represents multiple tasks into smaller, focused child beads. It is triggered on failure — when a bead fails execution, NEEDLE evaluates whether it should be decomposed before retrying.

## Split Criteria

A bead is splittable when it describes **multiple independent tasks**. This is a semantic determination, not a numeric one. The agent analyzes the bead and answers one question: "Does this bead ask for more than one independent unit of work?"

**Valid reasons to split:**
- The bead describes multiple distinct deliverables ("add endpoint AND write migration AND update tests")
- The deliverables have a dependency relationship (migration before endpoint)
- Each deliverable is independently closable

**Not valid reasons to split:**
- The bead is long (a single complex task can be long and still atomic)
- The bead failed once (failure means the task is hard, not composite)
- The bead has many acceptance criteria (criteria validate one task, not separate tasks)

If the agent determines the bead is a single task, mitosis does not apply. The bead is released for retry or deferred.

## Child-Aware Deduplication

Before creating any child bead, NEEDLE reads the parent's existing children. If a previous mitosis pass already created children for this parent, the proposed children are compared against them. Matching children are skipped; only novel tasks are created.

```
Bead fails
    │
    ▼
Agent analyzes: "Multiple independent tasks?"
    │
    ├── No → Release for retry or defer
    │
    └── Yes → Propose N children with dependencies
                │
                ▼
          Read parent's existing children
          (br show <parent> --json → dependencies)
                │
          For each proposed child:
                │
                ├── Parent already has a child covering this? → Skip
                │
                └── Novel task → Create child, link as blocking parent

          If any children created:
                └── Parent remains in_progress, blocked by children
          If all children already existed:
                └── No-op (split already happened)
```

This makes duplicate splits structurally impossible. The parent's child list is the single source of truth. A second worker encountering the same parent sees the existing children and creates nothing new.

## Concurrency Safety

Mitosis uses the same per-workspace flock as the claiming protocol. The flock is held for the entire mitosis operation: read existing children → create new children → update parent dependencies. This serializes mitosis across workers within a workspace.

If two workers both hold a failed bead and attempt mitosis on the same parent simultaneously, the flock ensures one completes first. The second worker enters the flock, reads the children just created by the first, and skips all proposed children as duplicates.

## When Mitosis Runs

Mitosis is evaluated in the HANDLING state when the outcome is **Failure** (exit code 1):

1. Check if mitosis is enabled for the workspace (configurable, default: true)
2. Check if this is the bead's **first failure** (mitosis runs once, not on every retry)
3. Acquire workspace flock
4. Dispatch agent with mitosis analysis prompt: bead context + "Does this describe multiple tasks?"
5. If agent proposes children:
   a. Read parent's existing children
   b. Create only novel children with appropriate dependencies
   c. Parent is blocked by children (remains claimed, status changes to blocked)
6. Release workspace flock
7. Emit `bead.mitosis` telemetry (children proposed, children created, children skipped)

If mitosis produces children, the parent is not released — it is blocked until its children complete. When all children are closed, the parent becomes unblocked and re-enters the queue for a final pass (or the mend strand clears the stale dependency and it resolves naturally).

If mitosis determines the bead is a single task, normal failure handling applies: release and increment failure count.

## Mitosis Configuration

```yaml
# ~/.needle/config.yaml or .needle.yaml
mitosis:
  enabled: true                # enable/disable per workspace
  first_failure_only: true     # only evaluate on first failure, not retries
```

## Telemetry

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `bead.mitosis.evaluated` | Agent analyzed bead for splitting | `bead_id`, `splittable` (bool), `proposed_children` (count) |
| `bead.mitosis.split` | Children created | `parent_id`, `children_created` (count), `children_skipped` (count), `child_ids` |
| `bead.mitosis.skipped` | All proposed children already exist | `parent_id`, `existing_children` (count) |

---

# Concurrency

Multiple NEEDLE workers operate in the same environment simultaneously. This section specifies how they coordinate without a central orchestrator.

## Coordination Model

NEEDLE uses **decentralized coordination through shared state**. There is no coordinator process, no leader election, no message passing between workers. All coordination happens through:

1. **Atomic bead claims** (SQLite transactions via `br update --claim`)
2. **Workspace-level flock** (POSIX file locks for claim serialization)
3. **File-based heartbeats** (health monitoring and stale detection)
4. **Worker state registry** (shared JSON file for fleet awareness)

This is approach #1 (SQLite transactions) from `docs/research/concurrency-approaches-compared.md`, augmented with file-based serialization to address the thundering herd problem.

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

The Mend strand (Strand 2) checks peer heartbeats:

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

## Concurrency Limits

### Provider/Model Limits

Rate limiting prevents API throttling and controls cost:

```yaml
limits:
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

### Fleet Sizing

The practical worker limit is not an arbitrary number. It is driven by three runtime constraints:

1. **Provider inference throughput.** Each worker waiting on an LLM response is idle CPU but an active API slot. If the provider rate-limits or queues requests, adding workers produces no additional throughput. NEEDLE tracks RPM per provider and warns when request latency exceeds a threshold.

2. **Available CPU.** Each agent process (Claude Code, OpenCode, etc.) consumes CPU during tool execution, file I/O, and git operations. NEEDLE's own overhead (strand evaluation, heartbeat I/O, lock contention) also scales with worker count. When system load exceeds a configurable threshold, NEEDLE emits a `fleet.cpu_saturated` warning.

3. **Available RAM.** Each agent process holds context in memory. Agent processes with large context windows or many tool calls can consume significant RAM. NEEDLE monitors system memory and warns when free memory drops below a threshold.

`max_workers` is a configurable ceiling, not a recommendation. The right value depends on the environment:

```yaml
worker:
  max_workers: 0              # 0 = no hard ceiling, rely on runtime monitoring
  cpu_load_warn: 0.8          # warn when system load average > 80% of cores
  memory_free_warn_mb: 512    # warn when free memory < 512MB
```

If `max_workers` is set, it is enforced at launch time. `needle run --count=25` with `max_workers: 20` will launch 20 and log a warning. If set to 0, NEEDLE launches the requested count and relies on runtime monitoring to signal saturation.

## Race Condition Prevention

Lessons from `docs/notes/claim-race-conditions.md`, applied to the new design:

| Race Condition | v1 Impact | v2 Prevention |
|---------------|-----------|---------------|
| **Thundering herd** | All workers claim same bead | Per-workspace flock serializes claims |
| **TOCTOU on closed beads** | Worker claims bead that was just closed | Verify bead status inside flock before claiming |
| **Stale claims from crashed workers** | Beads stuck `in_progress` forever | Heartbeat TTL + Mend strand auto-release |
| **Lock file leaks** | Orphaned locks block claims | Lock TTL + Mend strand cleanup |
| **Concurrent bead creation** | (Weave/Pulse/Unravel) create duplicates | Seen-issue deduplication + creation cooldowns |

## Concurrency Invariants

1. **One claim at a time per workspace.** The flock guarantees this. Two workers cannot execute `br update --claim` simultaneously in the same workspace.

2. **One bead per worker.** A worker holds at most one claimed bead. It releases or verifies closure before claiming another.

3. **Claims have a TTL.** If a worker holds a claim for longer than `heartbeat_ttl` without updating its heartbeat, the claim is considered stale and eligible for release by Mend.

4. **No implicit locking.** Labels are not locks. Bead status is not a lock. Only flock and `br update --claim` provide mutual exclusion.

5. **Lock scope is minimal.** The workspace flock is held for milliseconds (duration of the `br` CLI call), never for the duration of bead execution.

---

# Telemetry

Every state transition, claim attempt, dispatch, and outcome emits structured telemetry. A silent worker is a broken worker.

## Telemetry Design Principles

1. **Structured from origin.** Events are typed structs, not log strings. They are serialized to JSONL for storage and consumption. There is no string parsing.

2. **Separate from agent output.** Telemetry is written to NEEDLE's own sinks. It is never interleaved with agent stdout/stderr. This eliminates the stdout corruption bug class from v1 (see `docs/notes/bash-at-scale-problems.md`).

3. **Non-blocking.** Telemetry emission never blocks the worker loop. If a sink is slow or failing, events are buffered and dropped after a threshold, not retried.

4. **Complete.** Every state transition produces an event. If you reconstruct events for a worker, you can replay its entire session.

## Event Schema

All events share a common envelope:

```json
{
  "timestamp": "2026-03-20T15:30:00.123Z",
  "event_type": "bead.claim.attempted",
  "worker_id": "needle-claude-anthropic-sonnet-alpha",
  "session_id": "a1b2c3d4",
  "sequence": 42,
  "bead_id": "nd-a3f8",
  "workspace": "/home/coder/project",
  "data": { }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `timestamp` | ISO 8601 with milliseconds | When the event occurred |
| `event_type` | Dotted string | Event classification |
| `worker_id` | String | Unique worker identifier |
| `session_id` | String | Unique session identifier (random per boot) |
| `sequence` | u64 | Monotonically increasing per session (enables ordering) |
| `bead_id` | String? | Bead ID if applicable |
| `workspace` | Path? | Workspace path if applicable |
| `data` | Object | Event-specific payload |

## Event Catalog

### Worker Lifecycle

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `worker.started` | Worker boots successfully | `agent`, `model`, `config_hash`, `version` |
| `worker.stopped` | Graceful shutdown | `beads_processed`, `uptime_seconds`, `reason` |
| `worker.errored` | Unrecoverable error | `error_type`, `error_message`, `beads_processed` |
| `worker.exhausted` | All strands empty | `cycle_count`, `last_strand_evaluated` |
| `worker.idle` | Entering backoff after exhaustion | `backoff_seconds` |

### Strand Evaluation

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `strand.evaluated` | Strand returns a result | `strand_name`, `result` (`bead_found`, `work_created`, `no_work`, `error`), `duration_ms` |
| `strand.skipped` | Strand is disabled | `strand_name`, `reason` |

### Bead Operations

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `bead.claim.attempted` | Claim attempt starts | `bead_id`, `retry_number` |
| `bead.claim.succeeded` | Claim won | `bead_id`, `priority`, `title_hash` |
| `bead.claim.race_lost` | Claim lost to another worker | `bead_id`, `claimed_by` |
| `bead.claim.failed` | Claim failed (not race) | `bead_id`, `reason` |
| `bead.released` | Bead released back to queue | `bead_id`, `reason` (`failure`, `timeout`, `crash`, `interrupted`) |
| `bead.completed` | Bead closed by agent (detected) | `bead_id`, `duration_ms` |
| `bead.orphaned` | Agent process ended while its bead remained claimed; Resolve will run | `bead_id`, `worker_id`, `exit_code` |
| `bead.resolution.applied` | Resolve decision was validated and applied | `bead_id`, `decision`, `reason`, `attempt` |
| `bead.resolution.failed` | Resolve failed or returned invalid output and NEEDLE released the bead | `bead_id`, `failure`, `release_succeeded` |

### Agent Dispatch

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `agent.dispatched` | Agent process started | `agent_name`, `model`, `pid`, `prompt_hash`, `prompt_tokens_est` |
| `agent.executing` | Periodic during execution | `pid`, `elapsed_ms`, `still_alive` |
| `agent.completed` | Agent process exited | `exit_code`, `elapsed_ms`, `stdout_bytes`, `stderr_bytes` |
| `agent.timeout` | Agent killed for timeout | `timeout_ms`, `pid` |

### Outcome Handling

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `outcome.classified` | Exit code mapped to outcome | `outcome` (`success`, `failure`, `timeout`, `crash`, `agent_not_found`, `interrupted`), `exit_code` |
| `outcome.handled` | Handler executed | `outcome`, `action` (`released`, `deferred`, `alerted`, `none`) |

#### Verification Gates Judge Committed State

**Status:** Accepted — See [ADR-020](../adr/020-verification-gates-judge-committed-state.md) for full rationale and implementation details.

**Core Principle:** Verification gates MUST evaluate committed git state, not uncommitted working tree files.

**Architecture:**
- **Clean extraction:** Before running gates, NEEDLE extracts HEAD using `git archive HEAD | tar -x -C <tmp>` into a per-dispatch temp directory
- **Execution modes:** `GateConfig::Command` gains a `run_in` field:
  - `clean` (default): Run in the extracted committed state
  - `workspace`: Run in the shared checkout (for gates that must see uncommitted state, e.g., build cache validation)
- **Lifecycle:** Temp directories are removed on success, retained on failure for diagnosis
- **Shipped-work check:** Already operates on git commits only (no extraction needed)

**Why this is necessary:**
1. **Reproducibility:** Committed state is the only durable artifact that replicates across environments
2. **CI parity:** Gates should pass/fail the same in NEEDLE as they would in CI or on a fresh clone
3. **Shared checkout safety:** Multiple workers sharing a workspace can verify independently without interference
4. **False positive prevention:** Prevents gates from passing locally against uncommitted files but failing on committed code

**Relationship to ADR-015 (No Worktrees Policy):**
ADR-015 explicitly rejected per-worker git worktrees to avoid disk/build-cache explosion and merge-back complexity. Clean extraction provides isolation at verification time without worktree overhead:
- Lightweight temp directories (deleted on success)
- No git management overhead
- Simple, clear lifecycle

**Configuration Example:**
```yaml
gates:
  - type: command
    commands:
      - cargo test
      - cargo clippy -- -D warnings
    run_in: clean  # Default, can be omitted

  - type: command
    commands:
      - make build-check
    run_in: workspace  # For gates that must see build cache
```

**Failure Handling and Uncommitted-Dependency Detection:**
When a clean-extraction gate fails that would have passed in workspace mode:
1. The extraction is retained (not deleted) for diagnosis
2. The bead receives label `uncommitted-dependency` (planned enhancement)
3. The reopen reason includes the workspace diff for context

This makes the shared-checkout failure mode (which ADR-015 accepts as operational risk) detectable and actionable.

### Health

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `heartbeat.emitted` | Heartbeat file updated | `state`, `current_bead` |
| `peer.stale` | Stale peer detected | `peer_id`, `last_seen`, `claimed_bead` |
| `peer.crashed` | Dead peer cleaned up | `peer_id`, `released_bead` |
| `health.check` | Periodic health check | `db_healthy`, `disk_free_mb`, `peer_count` |
| `fleet.cpu_saturated` | System load exceeds threshold | `load_average` (f64), `threshold` (f64), `core_count` (usize) |
| `fleet.memory_low` | Free memory below threshold | `free_mb` (u64), `threshold_mb` (u64) |

### Effort Tracking

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `effort.recorded` | After each bead processing cycle | `bead_id`, `elapsed_ms`, `agent_name`, `model`, `tokens_in`, `tokens_out`, `estimated_cost_usd` |

## Sinks

Telemetry events are dispatched to one or more sinks. Sinks are configured independently.

### File Sink (default, always on)

Writes JSONL to per-worker log files:

```
~/.needle/logs/<worker-id>.jsonl
```

- One line per event
- File is append-only
- Rotation: new file per session (session ID in filename) or size-based (configurable)

### Stdout Sink (optional)

Writes human-readable summary to stdout for interactive monitoring:

```
15:30:00 [alpha] CLAIMED nd-a3f8 (p1: "Fix auth middleware")
15:30:02 [alpha] DISPATCHED claude-sonnet pid=12345
15:32:15 [alpha] SUCCESS nd-a3f8 (135s, ~2400 tokens)
15:32:15 [alpha] CLAIMED nd-b2c9 (p2: "Add rate limiting tests")
```

- Enabled when worker runs in foreground or via `needle attach`
- Format is configurable: `minimal`, `normal`, `verbose`
- Color-coded by event type

### Hook Sink (optional)

Dispatches events to external systems via webhook or command:

```yaml
telemetry:
  hooks:
    - event_filter: "outcome.*"
      command: "curl -X POST https://webhook.example.com/needle -d @-"
    - event_filter: "worker.errored"
      command: "/path/to/alert-script.sh"
    - event_filter: "effort.recorded"
      command: "/path/to/cost-tracker.sh"
```

- Events matching the filter are piped as JSON to the command's stdin
- Hook execution is fire-and-forget (non-blocking)
- Failed hooks emit a `telemetry.hook.failed` event to the file sink (not recursively to hooks)

### OTLP Sink (optional)

Exports telemetry as OpenTelemetry signals (traces, metrics, logs) over OTLP to any compliant collector (OpenTelemetry Collector, Jaeger, Tempo, Grafana Alloy, Honeycomb, Datadog, etc.). This is the canonical integration point for FABRIC and any downstream observability plane.

```yaml
telemetry:
  otlp_sink:
    enabled: true
    endpoint: "http://otel-collector.tailnet:4317"    # gRPC default; use 4318 for HTTP
    protocol: grpc                                     # grpc | http/protobuf
    headers:
      - "authorization: Bearer ${OTEL_TOKEN}"         # env interpolation; format: "key: value"
    timeout_ms: 5000
    compression: gzip                                  # none | gzip
    tls:
      insecure: false
      ca_file: ""
    signals:
      traces: true
      metrics: true
      logs: true
    resource_attributes:
      - "deployment.environment=production"           # format: "key=value"
      - "service.namespace=needle-fleet"
```

Design:

- **Non-blocking.** Uses a batch span/log/metric processor. If the collector is unreachable, events are buffered up to a bounded queue, then dropped (same policy as file sink). Drops emit a `telemetry.otlp.dropped` event to the file sink (never recursively to OTLP).
- **Additive.** The file sink is authoritative. OTLP is an export, not a replacement. If OTLP is disabled or misconfigured, NEEDLE behaves identically to a file-sink-only deployment.
- **Stdlib deps.** Rust crates: `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-semantic-conventions`. All support OTLP/gRPC and OTLP/HTTP.
- **W3C trace context.** Trace and span IDs are generated per the W3C Trace Context spec so they interop with any OTel backend.
- **Graceful shutdown.** On `worker.stopped`, the exporter is flushed with a deadline before process exit. Failure to flush is logged but never blocks shutdown.

## OpenTelemetry Semantic Mapping

NEEDLE's existing event catalog maps cleanly to OpenTelemetry's three signal types. This mapping is **normative** — the OTLP sink implementation must conform to it so dashboards and alerts remain stable across NEEDLE versions.

### Resource Attributes

Every exported signal carries these resource attributes (per OTel semantic conventions):

| Attribute | Value | Source |
|-----------|-------|--------|
| `service.name` | `"needle"` | Constant |
| `service.version` | build version | `env!("CARGO_PKG_VERSION")` |
| `service.instance.id` | `<worker_id>` | e.g., `needle-claude-anthropic-sonnet-alpha` |
| `service.namespace` | `"needle-fleet"` (default, configurable) | Config |
| `deployment.environment` | e.g., `"production"` | Config |
| `host.name` | hostname | OS |
| `process.pid` | worker PID | OS |
| `needle.agent` | e.g., `"claude-anthropic-sonnet"` | Worker config |
| `needle.model` | e.g., `"claude-sonnet-4-6"` | Worker config |
| `needle.session_id` | session ID | Per-boot random |
| `needle.workspace` | workspace path | Worker config |

### Traces

The NEEDLE state machine is naturally hierarchical, which maps directly to OTel spans.

```
worker.session                                          (root span, lifetime = worker process)
├── strand.pluck                                        (one per strand evaluation)
│   └── bead.lifecycle                                  (one per claimed bead)
│       ├── bead.claim                                  (ATOMIC phase)
│       ├── bead.prompt_build
│       ├── agent.dispatch                              (DISPATCHING + EXECUTING)
│       │   └── agent.execution                         (process alive; span.ok on exit 0)
│       └── bead.outcome                                (HANDLING)
│           └── bead.mitosis?                           (optional, if outcome = failure)
├── strand.mend
├── strand.explore
├── strand.weave
├── strand.unravel
├── strand.pulse
└── strand.knot                                         (terminal backoff / exhaustion)
```

Span naming follows OpenTelemetry conventions: lowercase dotted, verb-form where appropriate.

**Span attributes** follow OTel semantic conventions where applicable, plus a `needle.*` namespace:

| Span | Key Attributes |
|------|----------------|
| `worker.session` | `needle.beads_processed`, `needle.uptime_seconds`, `needle.exit_reason` |
| `strand.*` | `needle.strand.name`, `needle.strand.result`, `needle.strand.duration_ms` |
| `bead.lifecycle` | `needle.bead.id`, `needle.bead.priority`, `needle.bead.title_hash`, `needle.bead.outcome` |
| `bead.claim` | `needle.claim.retry_number`, `needle.claim.result` (`succeeded` / `race_lost` / `failed`) |
| `agent.dispatch` | `gen_ai.system` (e.g., `anthropic`), `gen_ai.request.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `needle.agent.pid`, `needle.agent.exit_code` |
| `bead.outcome` | `needle.outcome` (`success` / `failure` / `timeout` / `crash` / `agent_not_found` / `interrupted`), `needle.outcome.action` |

**GenAI semantic conventions.** The `agent.dispatch` span uses OTel's `gen_ai.*` conventions so NEEDLE's token/cost data shows up in GenAI dashboards out-of-the-box (Grafana GenAI app, Langfuse, Honeycomb AI, etc.).

**Span status.** Success outcomes set `Status::Ok`. All other outcomes set `Status::Error` with a description matching the `needle.outcome` value. This makes error-rate SLOs trivial.

**Context propagation.** The trace ID from `worker.session` is recorded in the file-sink event envelope as a new optional field `trace_id`, enabling correlation between the JSONL file sink and the OTel backend without ambiguity.

### Metrics

Metrics are emitted via the OTel Meter API, one `Meter` per worker. All metrics are prefixed `needle.*`.

| Metric | Instrument | Unit | Attributes | Description |
|--------|-----------|------|------------|-------------|
| `needle.workers.active` | UpDownCounter | `{worker}` | — | Current live worker count (incremented on `worker.started`, decremented on `worker.stopped`) |
| `needle.beads.claimed` | Counter | `{bead}` | `strand`, `priority` | Successful bead claims |
| `needle.beads.completed` | Counter | `{bead}` | `outcome` | Bead terminal outcomes (one per `bead.outcome`) |
| `needle.beads.duration` | Histogram | `ms` | `outcome` | End-to-end bead lifecycle time |
| `needle.claim.attempts` | Counter | `{attempt}` | `result` (`succeeded`/`race_lost`/`failed`) | Claim attempts |
| `needle.strand.duration` | Histogram | `ms` | `strand`, `result` | Strand evaluation time |
| `needle.agent.duration` | Histogram | `ms` | `agent`, `model`, `exit_code` | Agent process runtime |
| `needle.agent.tokens.input` | Counter | `{token}` | `agent`, `model` | Input tokens consumed |
| `needle.agent.tokens.output` | Counter | `{token}` | `agent`, `model` | Output tokens produced |
| `needle.cost.usd` | Counter | `USD` | `agent`, `model` | Estimated cost accumulator |
| `needle.heartbeat.age` | Gauge (observable) | `s` | `worker_id` | Seconds since last heartbeat emitted by this worker |
| `needle.peers.stale` | UpDownCounter | `{peer}` | — | Currently-stale peers observed by this worker |
| `needle.queue.depth` | Gauge (observable) | `{bead}` | `workspace`, `priority` | Open beads visible to this worker (sampled at strand evaluation) |
| `needle.mitosis.children_created` | Counter | `{bead}` | `parent_id` | Mitosis child creations |
| `needle.outcome.rate` | derived | — | — | Computed in the backend as `needle.beads.completed{outcome="success"} / needle.beads.completed` |

Metric aggregation temporality is **delta** (standard OTel default); backends that require cumulative (Prometheus via prometheusreceiver) convert upstream.

### Logs

Every NEEDLE telemetry event that isn't already represented as a span event is exported as an OTel LogRecord with:

- `severity_number` / `severity_text`: `INFO` for normal events, `WARN` for `peer.stale` / `telemetry.*.dropped`, `ERROR` for `worker.errored` / `bead.claim.failed` / `agent.timeout`.
- `body`: the existing event `data` object.
- `attributes`: flattened from the event envelope (`event_type`, `bead_id`, `workspace`, etc.).
  - `needle.agent` / `needle.model`: Present on events with dispatch context (e.g., `agent.dispatched`, `effort.recorded`). **Precedence**: when both Resource and record contain the same attribute, the record value wins. This allows the dashboard to show the actual adapter/model dispatched (which can differ from the configured default due to routing rules) while still having a process-invariant fallback when the worker is idle.
- `trace_id` / `span_id`: linked to the enclosing `bead.lifecycle` or `worker.session` span where applicable.

Events that ARE spans (e.g., `bead.claim.attempted` → a span, not a log) do not double-export as logs.

### Span Events vs. Logs

Intra-span state changes (e.g., `agent.executing` heartbeats, `heartbeat.emitted`) are recorded as OTel **span events** on the nearest enclosing span, not as separate logs. This keeps the signal count manageable and makes timelines in Tempo/Jaeger readable.

## Token and Cost Tracking

### Token Extraction

NEEDLE attempts to extract token usage from agent output. This is agent-specific and best-effort:

| Agent | Extraction Method |
|-------|-------------------|
| Claude Code | Parse `--output-format json` for `usage.input_tokens`, `usage.output_tokens` |
| OpenCode | Parse structured output (TBD) |
| Codex CLI | Parse structured output (TBD) |
| Aider | Parse cost summary line from stderr |
| Generic | No extraction; record elapsed time only |

If token extraction fails, the event is still emitted with `null` token fields. Missing tokens are not an error.

### Cost Estimation

Cost is estimated from tokens using configurable per-model pricing:

```yaml
pricing:
  claude-sonnet:
    input_per_million: 3.00
    output_per_million: 15.00
  claude-opus:
    input_per_million: 15.00
    output_per_million: 75.00
  gpt-4:
    input_per_million: 30.00
    output_per_million: 60.00
```

Cost is **estimated**, never authoritative. It is recorded in telemetry for trend analysis, not for billing.

## Querying Telemetry

NEEDLE includes built-in telemetry queries via the CLI:

```bash
# Summary of today's work
needle status

# Per-worker breakdown
needle status --by-worker

# Cost summary
needle status --cost --since 2026-03-20

# Event stream (tail -f equivalent)
needle logs --follow

# Filter by event type
needle logs --filter "bead.claim.*" --since 1h

# Export for external analysis
needle logs --format jsonl --since 24h > export.jsonl
```

---

# Self-Healing

NEEDLE workers must detect and recover from failures without human intervention. This section specifies the failure modes, detection mechanisms, and recovery procedures.

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

## Heartbeat-Based Detection

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

## Database Recovery

beads_rust uses SQLite with a known corruption issue (FrankenSQLite, upstream #171). NEEDLE must handle this.

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

## Stale Claim Recovery

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

A claimed bead is only released if the owning worker is **confirmed dead** (stale heartbeat AND dead PID). If the PID is alive, the bead is not released, even if the heartbeat is stale.

## Lock File Recovery

Mend strand checks lock file age:
1. Read lock files in `/tmp/needle-claim-*.lock`
2. If file modification time > `lock_ttl` (default: 10 minutes):
   - Attempt to acquire flock (non-blocking)
   - If acquired: no one holds it, delete the file, release flock
   - If not acquired: someone is actively holding it, skip

## Dependency Link Recovery

Mend strand checks for stale dependencies:
1. Query beads with status `open` that have blockers
2. For each blocker, check if the blocking bead is closed
3. If blocker is closed, remove the dependency link
4. Emit `bead.dependency.cleaned` telemetry

This is a necessary compensating mechanism because `br` does not automatically resolve dependency links on bead closure (documented in `docs/notes/bead-lifecycle-bugs.md`).

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

## Self-Modification and Release Channels

NEEDLE workers are allowed to modify NEEDLE itself. The v1 failures (see `docs/notes/self-modification-risks.md`) were not caused by self-modification as a concept — they were caused by untested changes deploying directly to the running fleet. The solution is not to ban self-modification but to gate promotion through release channels with canary testing.

### Release Channels

```
:testing ──► :stable ──► fleet hot-reload
                │
                └── rollback to previous :stable on failure
```

Three channels:

| Channel | Purpose | Who writes | Who reads |
|---------|---------|------------|-----------|
| `:testing` | Newly built binary, not yet validated | Worker that built it | Canary test harness only |
| `:stable` | Validated binary, approved for fleet use | Promotion pipeline (after canary passes) | Running fleet via hot-reload |
| `:latest` | Alias for the most recent `:stable` | Automatic on promotion | `needle upgrade` default target |

### Canary Testing Pipeline

When a worker modifies NEEDLE's source and builds a new binary:

```
Worker builds new binary
       │
       ▼
  Install as :testing
  (~/.needle/bin/needle-testing)
       │
       ▼
  Run canary test suite:
    1. Launch :testing binary with test .beads/ directory
    2. Test beads have defined inputs and expected outcomes
    3. :testing processes beads in isolation
    4. Compare actual outcomes against expected
       │
       ├── All pass → Promote :testing to :stable
       │                 │
       │                 ▼
       │              Fleet detects new :stable
       │              Workers hot-reload on next bead boundary
       │
       └── Any fail → Reject :testing
                        │
                        ▼
                     Mark bead as failed
                     :stable remains unchanged
                     Fleet continues on previous :stable
                     Emit canary.failed telemetry
```

### Canary Test Suite

The canary suite is a set of test beads with known-good outcomes stored in a dedicated test workspace:

```
~/.needle/canary/
├── .beads/                    # test bead store
│   ├── issues.jsonl
│   └── beads.db
├── test-workspace/            # mock workspace with source files
│   ├── src/
│   │   └── hello.py           # simple file for beads to modify
│   └── .beads/
└── expected/                  # expected outcomes per bead
    ├── bead-001.expected.json # { "exit_code": 0, "bead_closed": true }
    ├── bead-002.expected.json # { "exit_code": 0, "files_modified": ["src/hello.py"] }
    └── bead-003.expected.json # { "exit_code": 1, "bead_closed": false }
```

Test beads cover:
- **Happy path:** Simple bead that should succeed and close
- **Failure path:** Bead that should fail (tests outcome handling)
- **Timeout path:** Bead with intentionally slow agent (tests timeout enforcement)
- **State machine integrity:** Verify telemetry events match expected state transitions
- **Mitosis:** Multi-task bead that should split on failure

### Hot-Reload Protocol

Running workers check for a new `:stable` binary between bead processing cycles (after LOGGING, before SELECTING):

1. Compare current binary hash against `:stable` binary hash
2. If different:
   a. Emit `worker.upgrade.detected` telemetry
   b. Complete current bead cycle (never interrupt mid-execution)
   c. Re-exec with the new binary: `exec ~/.needle/bin/needle-stable run --resume`
   d. New binary picks up worker state from heartbeat file and registry
3. If same: continue normally

**`--resume` flag:** Tells the new binary to inherit the worker's identity (ID, session, tmux) rather than creating a new session. The worker continues from the SELECTING state with a fresh binary.

### Rollback

If a promoted `:stable` causes failures in the fleet:

1. Fleet workers emit `worker.errored` or repeated `outcome.failure` telemetry
2. Human (or automated watchdog) runs: `needle rollback`
3. Rollback restores the previous `:stable` from backup (`~/.needle/bin/needle-stable.prev`)
4. Workers hot-reload to the rolled-back binary on next cycle

Rollback is always available because promotion preserves the previous `:stable` as a backup.

### Binary Paths

```
~/.needle/bin/
├── needle-testing             # candidate under canary evaluation
├── needle-stable              # current fleet binary
├── needle-stable.prev         # previous stable (rollback target)
└── needle                     # symlink → needle-stable
```

### Configuration

```yaml
self_modification:
  enabled: true                     # allow workers to process NEEDLE beads
  canary_workspace: ~/.needle/canary  # test workspace with known-good beads
  auto_promote: true                # promote to :stable automatically if canary passes
  hot_reload: true                  # fleet hot-reloads from :stable between beads
```

### Telemetry

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `canary.started` | Canary test suite begins | `testing_binary_hash`, `test_count` |
| `canary.passed` | All canary tests passed | `testing_binary_hash`, `duration_ms` |
| `canary.failed` | One or more canary tests failed | `testing_binary_hash`, `failures` (list) |
| `promotion.completed` | :testing promoted to :stable | `old_hash`, `new_hash` |
| `worker.upgrade.detected` | Worker sees new :stable | `old_hash`, `new_hash` |
| `worker.upgrade.completed` | Worker re-exec'd with new binary | `new_hash` |
| `rollback.completed` | :stable rolled back to previous | `rolled_back_hash`, `restored_hash` |

---

# Configuration

NEEDLE uses a hierarchical configuration system. Values are resolved from highest to lowest precedence, with the first defined value winning.

## Precedence Order

```
CLI arguments          (highest — overrides everything)
       │
Environment variables
       │
Workspace config       (.needle.yaml in workspace root)
       │
Global config          (~/.needle/config.yaml)
       │
Built-in defaults      (lowest — always present)
```

**Rule:** A value set at a higher level completely replaces the lower level's value. There is no deep merging. For maps (like `strands`), the entire map is replaced, not merged key-by-key.

**Exception:** The `workspaces` list in Explore strand config is additive — workspace configs can add to the global list but not remove from it.

## Global Configuration

**Location:** `~/.needle/config.yaml`

```yaml
# ── Agent Configuration ──
agent:
  default: claude-anthropic-sonnet
  timeout: 600
  adapters_dir: ~/.needle/agents

# ── Worker Configuration ──
worker:
  max_workers: 0                      # 0 = no hard ceiling, rely on runtime monitoring
  launch_stagger_seconds: 2
  idle_timeout: 300
  idle_action: wait
  max_claim_retries: 5
  identifier_scheme: nato
  cpu_load_warn: 0.8                  # warn when load > 80% of cores
  memory_free_warn_mb: 512            # warn when free RAM < 512MB

# ── Workspace Configuration ──
workspace:
  default: ~/projects/main
  home: ~/projects/main

# ── Strand Configuration ──
strands:
  pluck:
    enabled: true
    exclude_labels: [deferred, human, blocked]
  explore:
    enabled: true
    workspaces:
      - ~/projects/api-server
      - ~/projects/frontend
  mend:
    enabled: true
    stale_claim_ttl: 300
    lock_ttl: 600
    db_check_interval: 50
  weave:
    enabled: false
    max_beads_per_run: 5
    cooldown_hours: 24
    exclude_workspaces: []
  unravel:
    enabled: false
    max_per_run: 3
    cooldown_days: 7
  pulse:
    enabled: false
    max_beads_per_run: 10
    cooldown_hours: 48
    severity_threshold: warning
    scanners: []
  knot:
    enabled: true
    alert_cooldown_minutes: 60
    exhaustion_threshold: 3

# ── Concurrency Limits ──
limits:
  providers:
    anthropic:
      max_concurrent: 10
      requests_per_minute: 60
    openai:
      max_concurrent: 5
      requests_per_minute: 40
  models: {}

# ── Health Monitoring ──
health:
  heartbeat_interval: 30
  heartbeat_ttl: 300
  peer_check_interval: 60

# ── Telemetry ──
telemetry:
  file_sink:
    enabled: true
    directory: ~/.needle/logs
    rotation: session
    retention_days: 30
  stdout_sink:
    enabled: false
    format: normal
    color: auto
  hooks: []
  otlp_sink:
    enabled: false
    endpoint: "http://localhost:4317"
    protocol: grpc              # grpc | http/protobuf
    headers: []                 # array of "key: value" strings, e.g., ["authorization: Bearer ${OTEL_TOKEN}"]
    timeout_ms: 5000
    compression: gzip           # none | gzip
    tls:
      insecure: false
      ca_file: ""
    signals:
      traces: true
      metrics: true
      logs: true
    resource_attributes:
      - "deployment.environment=development"      # format: "key=value"
      - "service.namespace=needle-fleet"

# ── Cost Tracking ──
pricing: {}
budget:
  warn_usd: 0
  stop_usd: 0

# ── Self-Modification & Release Channels ──
self_modification:
  enabled: true
  canary_workspace: ~/.needle/canary
  auto_promote: true
  hot_reload: true
```

## Workspace Configuration

**Location:** `.needle.yaml` in workspace root (next to `.beads/`)

Workspace-level configuration overrides global settings for that specific workspace. Only a subset of settings can be overridden at the workspace level.

```yaml
agent:
  default: claude-anthropic-opus
  timeout: 1200

strands:
  weave:
    enabled: true
    max_beads_per_run: 3
  pulse:
    enabled: true
    scanners:
      - name: rust-clippy
        command: "cargo clippy --message-format=json 2>/dev/null"

prompt:
  context_files:
    - AGENTS.md
    - docs/architecture.md
  instructions: |
    This workspace uses the repository pattern.
    All database access must go through src/repository/.
    Run `cargo test` before closing the bead.
```

### Overridable Settings

| Setting | Workspace Override | Why |
|---------|-------------------|-----|
| `agent.default` | Yes | Different projects may need different models |
| `agent.timeout` | Yes | Complex projects may need longer timeouts |
| `strands.weave` | Yes | Some projects want gap analysis, others don't |
| `strands.pulse` | Yes | Scanners are project-specific |
| `strands.unravel` | Yes | Per-project opt-in |
| `prompt.*` | Yes | Project-specific context and instructions |
| `worker.*` | **No** | Worker config is fleet-level, not per-workspace |
| `limits.*` | **No** | Rate limits are provider-level, not per-workspace |
| `health.*` | **No** | Health monitoring is fleet-level |
| `telemetry.*` | **No** | Telemetry config is fleet-level |

## Environment Variables

All configuration keys can be overridden via environment variables with the `NEEDLE_` prefix. Nested keys use `__` (double underscore) as separator.

| Config Key | Environment Variable |
|------------|---------------------|
| `agent.default` | `NEEDLE_AGENT__DEFAULT` |
| `agent.timeout` | `NEEDLE_AGENT__TIMEOUT` |
| `worker.max_workers` | `NEEDLE_WORKER__MAX_WORKERS` |
| `strands.weave.enabled` | `NEEDLE_STRANDS__WEAVE__ENABLED` |

## Configuration Validation

Configuration is validated at boot time. Invalid configuration causes the worker to enter ERRORED state immediately.

### Required Fields

- `agent.default` must reference a valid adapter (built-in or file exists in adapters dir)
- `workspace.default` or `--workspace` must be a directory containing `.beads/`
- Numeric fields must be positive
- Duration fields must be > 0

### Warnings (non-fatal)

- `worker.max_workers` > 0 and > CPU count (consider runtime monitoring instead)
- `health.heartbeat_ttl` < `3 * health.heartbeat_interval` (detection may be unreliable)
- `strands.explore.workspaces` contains paths that don't exist
- No pricing configured when `telemetry.effort.track_cost: true`

### Config Dump

```bash
needle config --dump
needle config --dump --show-source

# Example output:
# agent.default: claude-anthropic-sonnet (from: ~/.needle/config.yaml)
# agent.timeout: 1200 (from: /home/coder/project/.needle.yaml)
# worker.max_workers: 20 (from: NEEDLE_WORKER__MAX_WORKERS env var)
# worker.idle_timeout: 300 (from: built-in default)
```

---

# Agent Adapters

NEEDLE is agent-agnostic. It wraps any headless CLI that accepts a prompt and exits. The adapter system is the abstraction layer that makes this possible.

NEEDLE does not know how agents work. It knows how to:
1. Render an invoke template with variables
2. Pipe a prompt via the configured input method
3. Wait for the process to exit
4. Capture exit code, stdout, and stderr

Everything else — authentication, model selection, context handling, tool use — is the agent's responsibility.

## Adapter Interface

An adapter is a YAML file that describes how to invoke a specific agent:

```yaml
name: claude-anthropic-sonnet
description: Claude Code with Anthropic Sonnet model
agent_cli: claude
version_command: "claude --version"
input_method: stdin                   # stdin | file | args
invoke_template: >
  cd {workspace} &&
  claude --print
  --model claude-sonnet-4-6
  --max-turns 30
  --output-format json
  < {prompt_file}
environment:
  CLAUDE_CODE_MAX_TURNS: "30"
token_extraction:
  method: json_field                  # json_field | regex | none
  input_path: "usage.input_tokens"
  output_path: "usage.output_tokens"
provider: anthropic
model: claude-sonnet-4-6
```

## Input Methods

### stdin

Prompt is piped to the agent's stdin. Most common for Claude Code. NEEDLE writes the prompt to a temp file (`{prompt_file}`) and redirects it to stdin to avoid shell escaping issues.

### file

Prompt is written to a file and the file path is passed as an argument.

### args

Prompt is passed as a command-line argument. `{prompt_escaped}` is the prompt with shell metacharacters escaped. For long prompts, NEEDLE may fall back to file-based input.

## Template Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `{workspace}` | Absolute path to workspace | `/home/coder/project` |
| `{prompt_file}` | Path to temp file containing the prompt | `/tmp/needle-prompt-a1b2.txt` |
| `{prompt_escaped}` | Shell-escaped prompt string | `Fix the auth bug in src/auth.rs` |
| `{bead_id}` | Current bead ID | `nd-a3f8` |
| `{model}` | Model identifier from adapter config | `claude-sonnet-4-6` |
| `{worker_id}` | Worker identifier | `needle-claude-anthropic-sonnet-alpha` |
| `{timeout}` | Timeout in seconds | `600` |

## Built-in Adapters

NEEDLE ships with adapters for common agents, embedded in the binary. These can be overridden by placing a file with the same name in `~/.needle/agents/`.

### Claude Code (Sonnet)

```yaml
name: claude-anthropic-sonnet
agent_cli: claude
input_method: stdin
invoke_template: >
  cd {workspace} && claude --print --model claude-sonnet-4-6
  --max-turns 30 --output-format json --verbose < {prompt_file}
token_extraction:
  method: json_field
  input_path: "result.usage.input_tokens"
  output_path: "result.usage.output_tokens"
provider: anthropic
model: claude-sonnet-4-6
```

### Claude Code (Opus)

```yaml
name: claude-anthropic-opus
agent_cli: claude
input_method: stdin
invoke_template: >
  cd {workspace} && claude --print --model claude-opus-4-6
  --max-turns 50 --output-format json --verbose < {prompt_file}
token_extraction:
  method: json_field
  input_path: "result.usage.input_tokens"
  output_path: "result.usage.output_tokens"
provider: anthropic
model: claude-opus-4-6
```

### OpenCode

```yaml
name: opencode-default
agent_cli: opencode
input_method: file
invoke_template: >
  cd {workspace} && opencode run --prompt-file {prompt_file} --non-interactive
token_extraction:
  method: none
provider: configurable
model: configurable
```

### Codex CLI

```yaml
name: codex-openai-gpt4
agent_cli: codex
input_method: args
invoke_template: >
  cd {workspace} && codex --model gpt-4 --approval-mode full-auto "{prompt_escaped}"
token_extraction:
  method: none
provider: openai
model: gpt-4
```

### Aider

```yaml
name: aider-anthropic-sonnet
agent_cli: aider
input_method: args
invoke_template: >
  cd {workspace} && aider --model claude-sonnet-4-6 --yes --message "{prompt_escaped}"
token_extraction:
  method: regex
  pattern: "Tokens: ([\\d,]+) sent, ([\\d,]+) received"
  input_group: 1
  output_group: 2
provider: anthropic
model: claude-sonnet-4-6
```

## Prompt Templates

Prompts are configurable at both the global and workspace level. Every agent-invoking operation (Pluck execution, post-Pluck resolution, Weave gap analysis, Unravel alternatives, Pulse scanning, Mitosis splitting) uses a named prompt template. Templates are deterministic functions of their inputs — same bead state produces the same prompt.

### Template Variables

All templates have access to these variables:

| Variable | Available In | Description |
|----------|-------------|-------------|
| `{bead_id}` | All | Current bead ID |
| `{bead_title}` | All | Bead title |
| `{bead_body}` | All | Bead body/description |
| `{workspace_path}` | All | Absolute path to workspace |
| `{context_file_contents}` | All | Contents of configured context files |
| `{workspace_instructions}` | All | Instructions from `.needle.yaml` |
| `{worker_id}` | All | Worker identifier |
| `{existing_children}` | Mitosis | Parent's current children (titles + IDs) |
| `{attempt_evidence}` | Resolve | Bounded evidence from the completed Pluck dispatch |
| `{human_bead_context}` | Unravel | The HUMAN-blocked bead being analyzed |
| `{scan_results}` | Pulse | Output from configured scanners |
| `{doc_files}` | Weave | Documentation file listing and contents |

### Built-in Templates

**`pluck` — Bead execution (default):**

```markdown
## Task

{bead_title}

## Description

{bead_body}

## Workspace

{workspace_path}

## Context Files

{context_file_contents}

## Instructions

{workspace_instructions}

Complete the task described above. When finished:
- Commit your changes with a descriptive message
- Close the bead: `br close {bead_id} --body "Summary of what was done"`

If you cannot complete the task:
- Do NOT close the bead
- The bead will be automatically released for retry

Bead ID: {bead_id}
```

**`mitosis` — Split analysis:**

```markdown
## Bead Analysis

Title: {bead_title}
Body: {bead_body}

## Existing Children

{existing_children}

## Question

Does this bead describe more than one independent task? A task is independent
if it produces a distinct deliverable and could be completed without completing
the other tasks in this bead.

If yes, list each independent task as a structured child bead with:
- title: concise task description
- body: what needs to be done
- dependencies: which other children must complete first (by title)

If this bead describes a single task (even a complex one), respond with: SINGLE_TASK

Do not propose children that duplicate any existing children listed above.
```

**`resolve` — Post-Pluck outcome resolution:**

```markdown
## Outcome Resolution

The prior Pluck agent process has ended, but this bead is still in progress.
Determine what happened. Do not continue implementation. Do not modify files,
commit, push, or mutate bead state.

### Bead

ID: {bead_id}
Title: {bead_title}
Description: {bead_body}

### Attempt Evidence

{attempt_evidence}

Return exactly one structured decision: `complete`, `retry`, `blocked`, or
`split`. Include a concise reason and concrete evidence. A retry must include
retry guidance; blocked must name the external prerequisite; split must
propose independent child deliverables. Do not declare completion solely from
the prior agent's narrative.
```

**`weave` — Gap analysis:**

```markdown
## Workspace Documentation

{doc_files}

## Current Open Beads

{existing_beads}

## Question

Review the documentation above. Identify gaps where documented features,
APIs, or workflows are incomplete, missing tests, or have no corresponding
implementation bead.

For each gap found, propose a bead with:
- title: concise description of what's missing
- body: what needs to be done to close the gap
- priority: 1 (critical), 2 (important), or 3 (nice-to-have)

Do not propose beads that duplicate any existing open beads listed above.
If no gaps are found, respond with: NO_GAPS
```

**`unravel` — Alternative proposals:**

```markdown
## Blocked Bead

Title: {bead_title}
Body: {bead_body}
Status: Blocked (requires human decision)

## Question

This bead is blocked because it requires a human decision. Analyze the bead
and propose alternative approaches that could be executed by an automated
agent without the human decision.

For each alternative, provide:
- title: concise description of the alternative approach
- body: what would be done differently
- tradeoffs: what is gained and what is lost compared to the original approach

If no viable alternatives exist, respond with: NO_ALTERNATIVES
```

**`pulse` — Health scan bead creation:**

```markdown
## Scan Results

{scan_results}

## Current Open Beads

{existing_beads}

## Question

Review the scan results above. For issues that are significant enough to
warrant a fix, propose a bead with:
- title: concise description of the issue
- body: what needs to be fixed and how
- priority: based on severity (1=critical, 2=important, 3=minor)

Do not propose beads that duplicate any existing open beads listed above.
If no significant issues are found, respond with: NO_ISSUES
```

### Overriding Templates

Templates are overridable at the workspace level in `.needle.yaml`:

```yaml
prompt:
  context_files:
    - CLAUDE.md
    - AGENTS.md
  instructions: |
    This workspace uses the repository pattern.
    Run `cargo test` before closing the bead.

  templates:
    pluck: |
      {bead_title}

      {bead_body}

      Workspace: {workspace_path}
      {context_file_contents}
      {workspace_instructions}

      Close when done: br close {bead_id} --body "summary"
      Bead ID: {bead_id}

    mitosis: |
      ... custom mitosis prompt ...
```

If a template is not overridden at the workspace level, the built-in default is used. Templates can also be overridden globally in `~/.needle/config.yaml` under `prompt.templates`.

### Agent-Owned Closure

The Pluck template instructs the agent to close the bead via `bead close`. That
remains the normal completion path. NEEDLE may close a bead from a Resolve
`complete` decision only after configured gates and shipped-work verification
pass. This preserves the useful parts of agent-owned closure while preventing
a terminated dispatch from retaining a permanent claim:

- The agent knows whether the work is actually done
- NEEDLE's post-dispatch parsing of agent output was fragile
- Exit code 0 does not guarantee the work was completed correctly
- The agent can include a meaningful closure message
- Resolve is invoked only for the exceptional still-`in_progress` state
- NEEDLE, rather than the resolver, applies the validated transition

## Adapter Validation

```bash
needle test-agent claude-anthropic-sonnet

# Output:
#   Adapter: claude-anthropic-sonnet
#   CLI:     claude (found at /home/coder/.local/bin/claude)
#   Version: Claude Code v1.0.30
#   Input:   stdin
#   Probe:   echo hello → exit 0 (1.2s)
#   Tokens:  extraction working (in: 45, out: 12)
#   Status:  READY
```

## Adding a Custom Agent

1. Create a YAML file in `~/.needle/agents/`
2. Edit the file with the agent's invocation details
3. Test the adapter: `needle test-agent my-agent`
4. Use it: `needle run --agent my-agent`

No code changes required. No recompilation. No restart of other workers.

---

# Implementation Phases

NEEDLE is built in three phases. Each phase produces a usable tool. No phase depends on future phases — Phase 1 alone is a complete, working system.

## Phase 1: Core State Machine

**Goal:** A single binary that processes beads from one workspace using one agent. The state machine is complete. Telemetry is complete. The tool works end-to-end.

### Deliverables

| Component | Scope |
|-----------|-------|
| **CLI** | `needle run`, `needle stop`, `needle list`, `needle version` |
| **Worker** | Full state machine: BOOTING → SELECTING → CLAIMING → BUILDING → DISPATCHING → EXECUTING → HANDLING → LOGGING → (loop) |
| **Strand 1 (Pluck)** | Query, filter, sort beads from single workspace |
| **Strand 7 (Knot)** | Basic exhaustion handling (backoff, exit) |
| **Claimer** | Atomic claim via `br update --claim`, single workspace flock |
| **PromptBuilder** | Deterministic prompt from bead context |
| **Dispatcher** | Agent adapter loading, process execution, timeout enforcement |
| **OutcomeHandler** | All 6 outcomes handled (success, failure, timeout, crash, agent_not_found, interrupted) |
| **Telemetry** | File sink (JSONL), all events in catalog |
| **Config** | Global config file, CLI argument overrides |
| **Agent adapters** | Claude Code built-in, generic template |
| **BeadStore** | `br` CLI wrapper with JSON parsing |
| **Types** | All enums (State, Outcome, ClaimResult, StrandResult) with exhaustive matching |
| **tmux** | Session creation, naming, `needle run` self-invokes into tmux |

### Not in Phase 1

- Multi-worker coordination (flock is present but only one worker)
- Strands 2-6
- Heartbeat system
- Peer monitoring
- Workspace config (.needle.yaml)
- Multiple agent adapters
- Cost tracking
- Budget enforcement
- OTLP sink (Phase 2; Phase 1 ships JSONL file sink only)
- `needle attach`, `needle status`, `needle config`

### Success Criteria

- [x] `needle run --workspace /path --agent claude-anthropic-sonnet` launches a worker in tmux (src/cli/mod.rs, tmux session creation)
- [x] Worker claims a bead, dispatches to Claude Code, handles outcome (src/worker/mod.rs, full state machine)
- [x] All 6 outcome paths tested with mock agent (exit 0, 1, 124, 127, 130, timeout) (tests/outcome_tests.rs)
- [x] Telemetry JSONL file contains events for every state transition (src/telemetry/mod.rs, file sink)
- [x] `needle list` shows running workers (src/cli/mod.rs, cmd_list)
- [x] `needle stop` gracefully stops a worker (releases claimed bead) (src/cli/mod.rs, cmd_stop)
- [x] Worker loops: after handling one bead, it selects the next (src/worker/mod.rs, main loop)
- [x] Worker exhausts: when no beads remain, enters backoff and eventually exits (src/worker/mod.rs, EXHAUSTED state)
- [x] Binary compiles for Linux x86_64 and macOS arm64 (GitHub releases, CI workflow)

### Estimated Scope

~15 source files, ~3,000 LOC (Rust).

## Phase 2: Multi-Worker Fleet

**Goal:** Multiple workers operate in the same environment. They coordinate through shared state, detect failures, and self-heal. Workers roam across workspaces.

### Deliverables

| Component | Scope |
|-----------|-------|
| **Multi-worker launch** | `needle run --count N` with staggered startup |
| **Workspace flock** | Per-workspace claim serialization |
| **Heartbeat** | File-based heartbeat emission and monitoring |
| **Peer monitoring** | Stale/crashed worker detection |
| **Strand 2 (Mend)** | Stale claim cleanup, orphaned locks, dependency repair, db health |
| **Strand 3 (Explore)** | Roam configured workspaces for work |
| **Worker state registry** | Shared fleet state file |
| **Concurrency limits** | Provider/model max_concurrent, RPM limiting |
| **Workspace config** | `.needle.yaml` per-workspace overrides |
| **Additional adapters** | OpenCode, Codex, Aider built-in |
| **Cost tracking** | Token extraction, pricing config, effort events |
| **Budget enforcement** | Warn/stop at daily cost thresholds |
| **CLI extensions** | `needle attach`, `needle status`, `needle config` |
| **Database recovery** | Auto-detect corruption, repair from JSONL |
| **Mitosis** | Child-aware bead splitting on first failure, with dedup and flock serialization |
| **OTLP sink** | OpenTelemetry exporter emitting traces, metrics, and logs per the semantic mapping in the Telemetry chapter. gRPC + HTTP/protobuf transports. Non-blocking batch processor. Graceful shutdown flush. |

### Success Criteria

- [x] `needle run --count 5` launches 5 workers with staggered startup (src/cli/mod.rs, launch_workers)
- [x] Workers claim different beads (no thundering herd — verify via telemetry) (src/claim/mod.rs, workspace flock)
- [x] Crashed worker's claimed bead is released by peer within heartbeat_ttl (src/strand/mend.rs, peer cleanup)
- [x] Workers discover work in other configured workspaces (Explore strand) (src/strand/explore.rs)
- [x] Mend strand cleans stale claims and orphaned locks (src/strand/mend.rs)
- [x] Provider concurrency limits enforced (>N workers for same provider wait) (src/rate_limit/mod.rs)
- [x] `needle status` shows fleet summary with per-worker and per-bead stats (src/cli/mod.rs, cmd_status)
- [x] `needle attach alpha` connects to a running worker's tmux session (src/cli/mod.rs, cmd_attach)
- [x] Cost tracking produces accurate estimates (±20% of actual) (src/cost/mod.rs, pricing config)
- [x] Database corruption is detected and auto-repaired (src/bead_store/mod.rs, doctor_repair)
- [x] Workspace `.needle.yaml` overrides global config correctly (src/config/mod.rs, workspace overrides)
- [x] Mitosis splits multi-task beads into children on first failure (src/mitosis/mod.rs)
- [x] Duplicate mitosis on same parent creates no new children (child-aware dedup verified) (src/mitosis/mod.rs, existing children check)
- [x] With OTLP sink enabled against a local OpenTelemetry Collector, NEEDLE exports: a `worker.session` span per worker, `bead.lifecycle` child spans with `gen_ai.*` attributes, and `needle.beads.completed` / `needle.cost.usd` metrics (src/telemetry/otlp.rs, semantic mapping implemented)
- [x] OTLP collector unreachable does not block or crash workers (drops are recorded via `telemetry.otlp.dropped` in the file sink) (src/telemetry/otlp.rs, non-blocking exporter)
- [x] `trace_id` in JSONL file-sink events matches the corresponding span in the OTel backend (src/telemetry/mod.rs, trace_id propagation)

### Estimated Scope

~10 additional source files, ~4,000 additional LOC.

## Phase 3: Advanced Strands and Operations

**Goal:** NEEDLE can create work (not just process it), monitor codebase health, and integrate with external systems. Full operational tooling.

### Deliverables

| Component | Scope |
|-----------|-------|
| **Strand 4 (Weave)** | Gap analysis, bead creation from documentation |
| **Strand 5 (Unravel)** | Alternative proposals for HUMAN-blocked beads |
| **Strand 6 (Pulse)** | Codebase health scans, auto-bead creation |
| **Validation gates** | Pluggable pre-closure validation (inspired by bg-gate) |
| **Hook sink** | Telemetry dispatch to webhooks/commands |
| **Release channels** | :testing → :stable promotion with canary test suite, fleet hot-reload, rollback |
| **Self-update** | `needle upgrade` with version checking |
| **Doctor command** | `needle doctor` for full system health check |
| **Telemetry queries** | `needle logs --filter`, `needle status --cost` |
| **Installer** | One-liner install script, GitHub releases |

### Success Criteria

- [x] Weave strand creates valid beads from documentation gaps (with guardrails) (src/strand/weave.rs, max_beads_per_run, cooldown)
- [x] Unravel strand proposes alternatives for HUMAN beads without modifying originals (src/strand/unravel.rs)
- [x] Pulse strand detects codebase issues and creates beads (with deduplication) (src/strand/pulse.rs, scanner integration)
- [x] All opt-in strands respect cooldowns and max-bead limits (src/strand/*.rs, cooldown state)
- [x] Validation gates block bead closure when tests fail (src/validation/mod.rs, gate system)
- [x] Hook sink delivers events to configured webhooks (src/telemetry/mod.rs, hook sink)
- [x] `needle upgrade` downloads and installs new version (src/upgrade/mod.rs, GitHub releases)
- [x] `needle doctor` reports system health across all subsystems (src/cli/mod.rs, cmd_doctor, comprehensive checks)
- [x] One-liner install works on Linux and macOS (README.md, curl install script)
- [x] Worker modifies NEEDLE source → builds :testing → canary passes → promoted to :stable → fleet hot-reloads (src/canary/mod.rs, src/upgrade/mod.rs, release channels)
- [x] Canary failure rejects :testing, fleet continues on previous :stable (src/canary/mod.rs, canary tests)
- [x] `needle rollback` restores previous :stable and fleet hot-reloads (src/upgrade/mod.rs, rollback command)

### Estimated Scope

~10 additional source files, ~4,000 additional LOC.

## Migration from v1

NEEDLE v2 is a clean rewrite. There is no in-place upgrade path from v1.

### Migration Steps

1. Stop all v1 workers: `needle stop --all` (v1)
2. Back up v1 config: `cp -r ~/.needle ~/.needle-v1-backup`
3. Install v2 binary (overwrites v1)
4. Create v2 config: `needle init` (v2 detects and migrates compatible settings)
5. Test with single worker: `needle run --workspace /path --count 1`
6. Scale up: `needle run --count N`

### What Carries Over

- `.beads/` directories (unchanged, same `br` backend)
- Workspace structure
- Agent CLIs (same Claude Code, OpenCode, etc.)

### What Does Not Carry Over

- Config format (new YAML schema)
- Telemetry logs (new JSONL schema)
- Worker state files (new format)
- v1's build system, source files, and bash modules

## Test Strategy

### Unit Tests

| Module | Key Tests |
|--------|-----------|
| `outcome` | Every exit code maps to correct outcome variant |
| `strand` | Each strand returns correct StrandResult for each scenario |
| `claim` | Race lost, success, store error, max retries |
| `config` | Precedence: CLI > env > workspace > global > default |
| `telemetry` | Events serialized correctly, sequence monotonic |
| `health` | Stale detection, crashed vs stuck distinction |
| `bead_store` | JSON parsing handles all `br` output formats, including errors |
| `prompt` | Deterministic: same bead → same prompt hash |

### Integration Tests

| Test | What It Validates |
|------|-------------------|
| **End-to-end single worker** | Full loop: select → claim → build → dispatch (mock agent) → outcome → log |
| **Multi-worker claiming** | N workers, M beads: all beads claimed exactly once, no duplicates |
| **Crash recovery** | Kill worker mid-execution, verify peer releases claim |
| **Database corruption** | Corrupt SQLite, verify auto-repair and continued operation |
| **Timeout enforcement** | Agent that sleeps forever is killed after timeout |
| **Strand waterfall** | Empty workspace → explore → mend → knot progression |
| **Mitosis split** | Multi-task bead fails → agent proposes children → children created with correct dependencies |
| **Mitosis dedup** | Same parent split twice → second pass creates zero new children |
| **Mitosis concurrency** | Two workers attempt mitosis on same parent → flock serializes, no duplicates |

### Property Tests

| Property | Description |
|----------|-------------|
| **Deterministic ordering** | For any queue state, all workers compute the same candidate ordering |
| **Exhaustive outcomes** | The outcome enum covers all possible exit codes (no `_` wildcard) |
| **Claim exclusivity** | Given N concurrent claim attempts on 1 bead, exactly 1 succeeds |
| **Heartbeat liveness** | A healthy worker's heartbeat is always within TTL |

### No Mocking of `br`

From `docs/notes/mitosis-explosion-postmortem.md`: v1's tests mocked `br` output and missed that `br show --json` never included labels. v2 integration tests run against a real `br` instance with a test `.beads/` directory.

---

# Phase 4: Self-Learning

**Goal:** NEEDLE workers improve over time. The fleet closes the feedback loop between outcomes and future behavior. Workers learn from their own failures, from each other, and from structured meta-analysis.

**Research basis:** `docs/research/self-learning-agents.md` (2026-04-04). Key influences: AutoAgent (meta-agent harness optimization), KAIROS (memory consolidation daemon), Voyager (skill libraries), and Anthropic's eval roadmap.

## Design Principles (Phase 4-Specific)

These extend — not replace — the six core principles.

7. **Closed feedback loop.** Every outcome feeds forward into future behavior. A worker that fails today must not fail the same way tomorrow. The path from failure to learning to changed behavior is explicit and auditable.

8. **Separation of learning from execution.** Workers execute tasks. A separate process (reflect strand, meta-agent, or consolidation daemon) synthesizes learnings. This follows AutoAgent's key finding: being good at a domain and being good at improving at that domain are different capabilities.

9. **Traces over scores.** Binary success/failure is insufficient for improvement. Full execution traces (tool calls, agent reasoning, verifier output) are required for root cause analysis. AutoAgent demonstrated that improvement rate drops hard without traces.

10. **Model empathy.** The meta-agent or reflect agent should use the same model family as the task workers. Same-model pairing produces better harness edits because the meta-agent shares implicit understanding of the task model's reasoning patterns and limitations.

## Architecture Addition

Phase 4 adds a **Learning Layer** that sits alongside the existing five layers:

```
┌──────────────────────────────────────────────────────────────┐
│                        CLI Layer                              │
│  needle run | reflect | stats | ... (existing)                │
├──────────────────────────────────────────────────────────────┤
│                     Worker Layer                              │
│  Worker loop, strand waterfall, session management            │
├──────────────────────────────────────────────────────────────┤
│                   Learning Layer (NEW)                         │
│  Retrospectives, consolidation, trace capture, skill library  │
├──────────────────────────────────────────────────────────────┤
│                  Coordination Layer                            │
│  Claiming, locking, heartbeats, peer awareness                │
├──────────────────────────────────────────────────────────────┤
│                    Agent Layer                                 │
│  Adapter interface, dispatch, result capture, TRACE CAPTURE   │
├──────────────────────────────────────────────────────────────┤
│                   Foundation Layer                             │
│  Telemetry, configuration, bead store interface, self-healing │
└──────────────────────────────────────────────────────────────┘
```

### New Component Map (Phase 4)

| Component | Responsibility | Inputs | Outputs |
|-----------|---------------|--------|---------|
| **TraceCapture** | Capture full execution traces from agent runs | Agent stdout/stderr, tool call logs | Structured trace files (JSONL) |
| **Retrospective** | Extract learning from completed beads | Bead close body, execution trace, outcome | Structured retrospective entries |
| **Learnings** | Workspace-scoped knowledge store | Retrospective entries | `learnings.md` updates |
| **Consolidator** | Periodic pattern extraction and pruning | All retrospectives since last run | Updated learnings, CLAUDE.md proposals |
| **SkillLibrary** | Indexed store of proven procedures | Validated learnings, successful patterns | Skill files, context injection |
| **TemplateVersioner** | Track prompt template versions in telemetry | Template content, hash | Versioned telemetry tags |
| **StatsEngine** | Aggregate outcomes by template version, task type, worker | Telemetry JSONL | Success rates, comparisons |

## Trace Capture

### Problem

NEEDLE currently captures exit code, stdout, and stderr from agent processes. This is sufficient for outcome classification but insufficient for learning. To understand *why* a task failed, we need the full execution trace: every tool call, every decision point, every piece of agent reasoning.

### Design

Extend the `dispatch` module to capture structured traces alongside raw output:

```
struct ExecutionResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
    elapsed: Duration,
    pid: u32,
    trace_path: Option<PathBuf>,  // NEW: path to structured trace file
}
```

Trace capture is adapter-specific:

| Agent | Trace Source | Format |
|-------|-------------|--------|
| Claude Code | `--output-format json` or session transcript | Claude JSONL |
| Codex | Agent output stream | OpenAI JSONL |
| Aider | Chat history file | Aider markdown |
| Generic | stdout/stderr passthrough | Raw text |

Traces are stored in `.beads/traces/<bead-id>/`:

```
.beads/traces/nd-a3f8/
  trace.jsonl         # structured tool calls and reasoning
  stdout.txt          # raw stdout
  stderr.txt          # raw stderr
  metadata.json       # timing, tokens, cost, template version
```

### Trace Sanitization

Agent output routinely contains secrets: API keys, database connection strings, auth tokens, environment variables, and credentials. Storing these as plain JSONL in `.beads/traces/` is a data leak. All traces are sanitized before being written to disk.

**Rule source: gitleaks.** Rather than hand-rolling regex patterns, NEEDLE imports the rule database from [gitleaks](https://github.com/gitleaks/gitleaks) — the industry-standard secret detection tool. Gitleaks ships 222 rules in a single TOML file (`config/gitleaks.toml`) covering 120+ services across cloud providers, AI/ML platforms, CI/CD, payment processors, communication tools, and generic patterns. The regexes use RE2 syntax, which is directly compatible with Rust's `regex` crate.

**Sanitization pipeline (applied to all trace content before write):**

1. **Gitleaks rule set.** NEEDLE vendors a copy of `gitleaks.toml` at build time and parses the `[[rules]]` entries. Each rule provides:
   - `regex` — RE2 pattern matching the secret
   - `keywords` — fast pre-filter strings checked before the expensive regex (Aho-Corasick). If keywords are specified and none are found in the content, the regex is skipped.
   - `secretGroup` — which capture group contains the actual secret (only that portion is redacted, preserving surrounding context)
   - `entropy` — minimum Shannon entropy threshold. Distinguishes real secrets from placeholders like `your-api-key-here`.

   Key rule categories:
   | Category | Examples | Rule Count |
   |----------|----------|------------|
   | Cloud providers | AWS access keys, Azure AD secrets, GCP API keys | ~25 |
   | AI/ML | Anthropic, OpenAI, HuggingFace, Cohere API keys | ~8 |
   | Git/CI/CD | GitHub PATs, GitLab tokens, CircleCI, Drone | ~25 |
   | Communication | Slack (8 token types), Discord, Telegram, Twilio | ~15 |
   | Payment/Finance | Stripe, Plaid, Coinbase, Kraken | ~12 |
   | DevOps/Infra | HashiCorp Vault, Grafana, Datadog, Sentry | ~15 |
   | Generic | `generic-api-key`, JWT, private keys (RSA/EC), curl auth headers | ~10 |
   | Package registries | npm, PyPI, NuGet, RubyGems | ~8 |

   Redaction output includes the rule ID for auditability: `[REDACTED:aws-access-token]`

2. **Custom patterns.** Workspace-specific patterns configured in `.needle.yaml`, applied after the gitleaks rules:
   ```yaml
   learning:
     trace_sanitization:
       custom_patterns:
         - id: "kalshi-api-key"
           regex: "KALSHI_API_KEY=[^\\s]+"
           keywords: ["kalshi"]
         - id: "openbao-token"
           regex: "openbao\\s+token\\s+\\S+"
           keywords: ["openbao"]
       redaction_text: "[REDACTED]"    # default: "[REDACTED:<rule-id>]"
   ```

3. **Gitleaks allowlists and stopwords.** The gitleaks rule set includes per-rule allowlists (regexes, paths, stopwords) and ~1,480 global stopwords that suppress common false positives (e.g., `example`, `test`, `placeholder`). NEEDLE applies these to reduce over-redaction.

4. **Known-safe passthrough.** Structured fields that are never secrets (bead IDs, file paths, exit codes, timestamps, tool names) bypass sanitization for performance.

**Implementation approach:**

- **Option A (preferred): Vendor the TOML.** Include `gitleaks.toml` as a build-time asset. Parse it with `toml` crate into rule structs. Apply keyword pre-filter (Aho-Corasick via `aho-corasick` crate), then regex, then entropy check. This gives full control, no runtime dependency, and works offline.
- **Option B: Shell out to gitleaks.** Pipe trace content through `gitleaks stdin --no-git`. Simpler but adds a runtime dependency and process overhead per trace file.

Option A is preferred because trace sanitization is on the critical write path — it must be fast and cannot fail due to a missing binary.

**Updating the rule set:** `gitleaks.toml` is vendored, not dynamically fetched. To update, run `needle update-rules` which downloads the latest `config/gitleaks.toml` from the gitleaks repository and rebuilds. Rule updates are versioned in NEEDLE's git history.

**Design notes:**
- Sanitization is best-effort, not a security boundary. Traces should still be treated as sensitive and not committed to git or shared externally.
- The redaction is one-way — original content is not recoverable from sanitized traces. If a secret appears in a trace, it is replaced with `[REDACTED:<rule-id>]` and the original is lost.
- Sanitization runs synchronously before the trace file is written. There is no window where unsanitized content exists on disk.
- Over-redaction is preferred to under-redaction. A false positive (redacting a non-secret that looks like a key) is acceptable; a false negative (leaking a real key) is not.
- The keyword pre-filter is critical for performance. Without it, running 222 regexes against every line of trace output would be prohibitive. Keywords reduce the candidate set to ~5-10 rules per line on average.

### Trace Retention

Traces are large. Retention policy:

- **Failed beads:** Keep traces for 30 days (needed for failure analysis)
- **Successful beads:** Keep metadata.json only, delete trace.jsonl after 7 days
- **Configurable:** `learning.trace_retention_days` in `.needle.yaml`

## Bead Retrospectives

### Integration with Pluck Template

The pluck template is extended with retrospective instructions. Before closing a bead, the agent writes a structured learning block:

```
## Retrospective
- **What worked:** [approach that succeeded]
- **What didn't:** [approach that failed and why]
- **Surprise:** [anything unexpected about the codebase/tooling]
- **Reusable pattern:** [if this task type recurs, do X]
```

This block is included in the `br close --body` content. It lives in the JSONL log alongside all other bead data.

### Retrospective Extraction

The consolidator reads bead close bodies and extracts retrospective blocks into structured data:

```json
{
  "bead_id": "nd-a3f8",
  "worker_id": "needle-claude-anthropic-sonnet-alpha",
  "timestamp": "2026-04-04T15:30:00Z",
  "task_type": "bug-fix",
  "what_worked": "...",
  "what_didnt": "...",
  "surprise": "...",
  "reusable_pattern": "..."
}
```

## Workspace Learnings File

### Structure

Each workspace has a `.beads/learnings.md` loaded into all prompts via `context_files`:

```yaml
# .needle.yaml
prompt:
  context_files:
    - CLAUDE.md
    - plan.md
    - .beads/learnings.md    # <-- injected automatically when present
```

### Entry Format

```markdown
### 2026-04-04 | bead: nd-a3f8 | worker: alpha | type: bug-fix
- **Observation:** The kalshi API rate-limits at 5 req/s, not 10 as documented
- **Confidence:** high (verified empirically)
- **Source:** retrospective from bead nd-a3f8
```

### Size Management

- Maximum 80 active entries (configurable: `learning.max_learnings`)
- When exceeded, the consolidator runs automatically
- Entries older than 90 days without reinforcement are pruned
- Entries reinforced by multiple beads get higher retention priority

### Automatic Injection

If `.beads/learnings.md` exists in a workspace, NEEDLE automatically appends it to `context_files` during prompt building. No manual config required. Workers always see the latest learnings.

## Consolidation (Reflect Strand)

### New Strand: Reflect (Strand 7)

Positioned after Pulse and before Splice — the meta-analysis strategy before worker failure documentation. Reflect consolidates learnings from recent work; Splice documents worker failures. Reflect should never preempt actual task execution, remote work discovery, or bead creation strands. A worker only consolidates learnings when there is genuinely nothing else to do.

```
  Strand 6: PULSE
       │ no issues or disabled
       ▼
  Strand 7: REFLECT ── consolidate learnings from recent beads
       │ consolidation complete or not needed
       ▼
  Strand 8: SPLICE ─── worker failure documentation
       │ no failures
       ▼
  Strand 9: KNOT ───── alert human, enter backoff
```

(This renumbers Splice from Strand 7 to Strand 8, and Knot from Strand 8 to Strand 9.)

**Invokes agent:** Yes — uses a consolidation-specific prompt.

**Entry conditions:**
- Strands 1-6 returned no work (reflect only runs when the worker has exhausted all work-finding and work-creation strategies)
- At least N beads have been closed since last consolidation (default: 10)
- At least T hours since last consolidation (default: 24)

**Algorithm (KAIROS-inspired four-phase cycle):**

1. **Orient:** Read current `.beads/learnings.md` and existing skills. Check file sizes.
2. **Gather:** Read bead close bodies from `.beads/issues.jsonl` for beads closed since last consolidation. Read available traces for failed beads.
3. **Consolidate:**
   - Extract retrospective blocks from close bodies
   - Identify patterns across multiple beads (same failure mode, same codebase quirk)
   - Merge new learnings into `learnings.md`, deduplicating against existing entries
   - Convert relative references to absolute (bead IDs, dates)
   - If a learning appears 3+ times, promote to skill file in `.beads/skills/`
   - If a learning contradicts an existing entry, resolve in favor of the newer evidence
4. **Prune:**
   - Remove entries older than 90 days without reinforcement
   - Compress similar entries into single entries
   - Ensure total learnings stay under 80 entries

**Guardrails:**
- Cooldown: minimum 24 hours between consolidation runs (configurable)
- Max learnings created per run: 10
- Max skills promoted per run: 3
- The consolidation agent receives the workspace CLAUDE.md as context but MUST NOT modify it (read-only). CLAUDE.md changes require explicit human approval.

**Exit conditions:**
| Result | Action |
|--------|--------|
| Consolidation performed | Return `WorkCreated` → restart from Strand 1 (in case consolidation unblocked something) |
| Not enough data since last run | Return `NoWork` → fall through to Strand 8 (Splice) |
| Disabled or cooldown active | Return `NoWork` → fall through to Strand 8 (Splice) |

**Configuration:**
```yaml
strands:
  reflect:
    enabled: true              # on by default (unlike weave/unravel/pulse)
    min_beads_since_last: 10   # minimum closed beads before consolidation
    cooldown_hours: 24
    max_learnings_per_run: 10
    max_skills_per_run: 3
    learning_retention_days: 90
    max_learnings: 80
```

**Telemetry:**
| Event Type | Data Fields |
|------------|-------------|
| `reflect.started` | `beads_since_last`, `current_learnings_count` |
| `reflect.consolidated` | `learnings_added`, `learnings_pruned`, `skills_promoted`, `contradictions_resolved` |
| `reflect.skipped` | `reason` (cooldown, insufficient data) |

## Session Transcript Analysis

### Motivation

Bead close bodies are structured summaries written by the agent at the end of a task. They capture *what was done* but lose the process: failed attempts, tool call sequences, recovery strategies, and decision points. The full session transcript — stored by Claude Code as JSONL files in `.claude/projects/` — contains this richer signal.

Reflect should analyze both sources: closed bead bodies for structured outcomes, and session transcripts for the decision-making process that led to those outcomes.

### Transcript Discovery

Claude Code stores session transcripts as JSONL files under `.claude/projects/<project-hash>/<session-uuid>.jsonl`. Each line is a JSON object with role, content, tool calls, and timestamps.

```rust
struct TranscriptSession {
    path: PathBuf,
    workspace: PathBuf,
    mtime: DateTime<Utc>,
    entries: Vec<TranscriptEntry>,
}

struct TranscriptEntry {
    role: String,
    content: String,
    tool_calls: Vec<ToolCall>,
    timestamp: Option<DateTime<Utc>>,
}
```

**Discovery algorithm:**

1. Map workspace path to `.claude/projects/` subdirectory (hash-based mapping)
2. Enumerate all JSONL files, sorted by mtime descending
3. Filter to sessions within configurable recency window (default: 7 days)
4. Stream-parse each file, skipping malformed lines
5. Return `Vec<TranscriptSession>`

**Streaming:** Transcript files can be large. Parse line-by-line rather than loading entire files. Skip tool_result blocks containing base64 or binary content (truncate at 1KB).

### Action-Outcome Extraction

Raw transcript entries are too verbose for pattern extraction. Reflect distills them into structured action-outcome pairs:

```rust
struct ActionOutcome {
    action_type: String,       // tool name (Read, Edit, Bash, etc.)
    target: String,            // file path, command, or query
    outcome: Outcome,          // Success, Failure, Error, Retry, Workaround
    reasoning: String,         // agent's text between tool calls (truncated to 200 chars)
    timestamp: DateTime<Utc>,
}

enum Outcome { Success, Failure, Error, Retry, Workaround }
```

**Extraction algorithm:**

1. Walk transcript entries, identify tool_call → tool_result pairs
2. Classify outcomes: success (exit 0), failure (non-zero exit), retry (same tool, same target), workaround (different tool after failure)
3. Capture agent reasoning text between consecutive tool calls
4. Group consecutive related actions into logical "attempts"

**Key patterns to extract:**
- Failed tool calls followed by retries (friction points)
- Successful workarounds after failures (actionable learnings)
- Repeated tool call patterns (workflow habits)
- Error messages encountered (common failure modes)

### Pattern Merging

Reflect merges patterns from both sources — bead bodies and transcripts — into a unified set:

1. Run existing bead-body retrospective extraction (unchanged)
2. Run transcript action-outcome extraction (new)
3. Deduplicate on semantic similarity:
   - Exact match: same pattern text → merge counts
   - Near match: same tool + same outcome + similar reasoning → merge with combined context
   - Unique to one source: keep as-is with lower confidence score
4. Weight by frequency across both sources — a pattern seen in beads AND transcripts is higher confidence
5. Pass merged pattern set to existing promotion logic (learnings.md → skill files)

## Drift Detection

### Motivation

When multiple workers solve the same class of problem, they may converge on different approaches. Some drift is healthy (evolving better solutions over time), some is harmful (inconsistent behavior with no progression). Detecting drift turns scattered session data into actionable standardization signals.

### Session Similarity Matching

Before comparing approaches, reflect must identify which sessions solved comparable problems.

**Fingerprint per session:**

```rust
struct SessionFingerprint {
    file_paths: HashSet<PathBuf>,      // normalized, deduplicated by directory
    tool_outcomes: HashSet<(String, Outcome)>,  // (tool_name, outcome)
    bead_types: HashSet<String>,        // types of beads claimed/closed
    error_patterns: HashSet<String>,    // normalized error substrings
}
```

Similarity is computed as Jaccard overlap on these sets. Sessions sharing >60% overlap (configurable) are grouped into clusters.

### Approach Divergence Detection

For each session cluster, reflect extracts the solution approach per session and compares them:

| Divergence Category | Meaning | Action |
|---------------------|---------|--------|
| **Evolved** | Approaches improve over time (fewer retries, shorter paths) | Promote latest approach as learned pattern |
| **Inconsistent** | Approaches differ with no clear progression | Flag for human review, suggest standardizing |
| **Degraded** | Later sessions solve the same problem worse than earlier | Flag as regression, include earlier approach as reference |

**Output:** `DriftReport` per cluster, fed into the consolidation pipeline alongside normal pattern extraction.

**Telemetry:**

| Event Type | Data Fields |
|------------|-------------|
| `reflect.drift.detected` | `cluster_size`, `category`, `sessions` |
| `reflect.drift.promoted` | `pattern`, `category` |

## ADR Decision Records

### Motivation

Current learnings capture *what* the agent did. But "use `br doctor --repair` for corruption" is less useful than knowing *why*: "chose doctor --repair over `rm` + `sync --import-only` because the former preserves bead history." Reflect should preserve decision rationale alongside patterns.

Not all learnings are decisions. Repeated successful habits ("agent ran `cargo fmt` before committing") don't need ADR treatment. Only learnings that involve a choice between alternatives warrant the richer format.

### Decision Point Detection

Reflect detects decision points in transcript action-outcome sequences:

**Signals that indicate a decision:**
- Attempt → failure → different approach → success (implicit choice)
- Agent reasoning text contains "instead", "alternatively", "better approach", "let me try"
- Agent evaluated multiple options before acting (read two files, then chose one to edit)
- Failed tool call followed by a different tool call (not a retry)

```rust
struct DecisionPoint {
    attempted_first: String,    // what was tried
    failed_with: String,        // error or reason for failure
    chose_instead: String,      // what was chosen after failure
    rationale: String,          // agent's reasoning between failure and new approach
    succeeded: bool,
}
```

### ADR-Lite Format in CLAUDE.md

When a promoted learning has an associated DecisionPoint, it is written in ADR-lite format. Habit/workflow patterns use the simpler flat format. Both are wrapped in HTML comment markers for identification and future updates.

**Decision-type learning:**

```html
<!-- needle-learning:nd-a3f8 -->
- **Decision**: Use `br doctor --repair` before `rm` + `sync --import-only`
  **Context**: FrankenSQLite corruption in `.beads/` databases
  **Rationale**: `doctor --repair` preserves bead history; full rebuild loses in-progress state
  **ADR**: `.beads/decisions/nd-a3f8.md`
<!-- /needle-learning:nd-a3f8 -->
```

**Habit-type learning (unchanged from current format):**

```html
<!-- needle-learning:nd-b7c2 -->
- Always run `cargo fmt` before committing Rust code in this workspace
<!-- /needle-learning:nd-b7c2 -->
```

### ADR Condensation into CLAUDE.md

Full ADRs are stored in `.beads/decisions/<bead-id>.md` with complete context (alternatives considered, full reasoning, outcomes). CLAUDE.md entries are **condensed summaries** — the decision, context, and rationale in 2-3 lines — with a reference back to the full ADR via the `**ADR:**` line.

This keeps CLAUDE.md compact (it's loaded into every system prompt) while preserving the full decision record for deeper review.

**Full ADR file** (`.beads/decisions/nd-a3f8.md`):

```markdown
# ADR: Database Recovery Strategy

## Context
FrankenSQLite corruption in .beads/ databases causes "database disk image is malformed" errors during br operations.

## Alternatives Considered
1. `br doctor --repair` — reconstructs DB from JSONL, preserves in-progress state
2. `rm .beads/beads.db` + `br sync --import-only` — full rebuild, loses in-progress claims
3. Manual SQLite `PRAGMA integrity_check` + targeted repair — fragile, version-specific

## Decision
Use `br doctor --repair` as first-line recovery.

## Rationale
- Preserves bead history and in-progress claim state
- JSONL is always authoritative — repair reconstructs from source of truth
- `rm + sync` is a fallback only when repair itself fails

## Outcome
Resolved corruption for workers alpha, echo, foxtrot, hotel on 2026-04-26.
```

### Placement: Lowest Common Ancestor CLAUDE.md

Promoted learnings are placed in the CLAUDE.md at the **lowest common ancestor** directory covering all workspaces where the pattern was observed. This ensures the learning appears in the system prompt only when working in relevant projects.

**Resolution algorithm:**

1. Track which workspaces contributed each pattern during extraction
2. Find the deepest directory that is a parent of all contributing workspaces
3. Check for an existing CLAUDE.md at that directory
4. If no CLAUDE.md exists, create one with a `## NEEDLE Learnings` section
5. If a learning applies to a single workspace only, write to that workspace's CLAUDE.md
6. If a learning applies across all workspaces, write to `~/CLAUDE.md`

**Edge cases:**
- Pattern observed in repos under `~/ardenone-cluster/` → write to `~/ardenone-cluster/CLAUDE.md`
- Pattern observed in repos spanning multiple top-level directories → write to `~/CLAUDE.md`
- CLAUDE.md doesn't exist at target level → create it

**Deduplication:** Before appending, check for existing needle-learning entries with similar content (fuzzy match on first line). Update rather than duplicate.

**Telemetry:**

| Event Type | Data Fields |
|------------|-------------|
| `reflect.learning.promoted` | `learning_id`, `target_path`, `workspace_count`, `is_decision` |
| `reflect.learning.deduplicated` | `learning_id`, `existing_entry` |

## Skill Library

### Structure

```
.beads/skills/
  api-rate-limit-handling.md
  database-migration-pattern.md
  flaky-test-diagnosis.md
```

### Skill File Format

```markdown
---
task_types: [bug-fix, api-integration]
labels: [api, rate-limiting]
success_count: 7
last_used: 2026-04-03
source_beads: [nd-a3f8, nd-b7c2, nd-d1e5]
---

## API Rate Limit Handling

When hitting external APIs, implement exponential backoff with jitter...

### Steps
1. Check API documentation for stated limits
2. Implement retry with exponential backoff (base 2s, max 60s, jitter ±500ms)
3. Log rate limit responses for monitoring
4. Consider request batching if supported

### Known Limits
- Kalshi: 5 req/s (documented as 10, actual is 5)
```

### Skill Retrieval

During prompt building (BUILDING state), the PromptBuilder:

1. Reads the bead's labels and title
2. Matches against skill file `task_types` and `labels` fields
3. Injects top 3 matching skills (by `success_count`) into the prompt
4. Skills are appended after learnings, before the task instructions

### Skill Lifecycle

```
Observation (learnings.md entry)
    │ appears 3+ times across different beads
    ▼
Promoted to skill (.beads/skills/<name>.md)
    │ used by workers, success_count incremented
    ▼
Validated (success_count > threshold)
    │ optionally proposed as CLAUDE.md convention
    ▼
Convention (human approves, added to CLAUDE.md)
```

## Template Versioning and A/B Testing

### Version Tagging

Each prompt template gets a version string. The version is included in telemetry events:

```json
{
  "event_type": "agent.dispatched",
  "data": {
    "template_name": "pluck",
    "template_version": "pluck-v3",
    "prompt_hash": "sha256:a1b2c3..."
  }
}
```

### Stats Command

```
$ needle stats --by template_version --since 7d

Template Version  | Beads | Pass | Fail | Timeout | Pass Rate | Avg Tokens | Avg Cost
pluck-v2          |    45 |   38 |    5 |       2 |    84.4%  |     12,400 |   $0.42
pluck-v3          |    23 |   21 |    1 |       1 |    91.3%  |     10,800 |   $0.38

$ needle stats --by task_type --since 30d

Task Type    | Beads | Pass | Fail | Timeout | Pass Rate
bug-fix      |    89 |   78 |    8 |       3 |    87.6%
feature      |    45 |   32 |   10 |       3 |    71.1%
refactor     |    23 |   21 |    2 |       0 |    91.3%
test         |    34 |   31 |    3 |       0 |    91.2%

$ needle stats --by worker --since 7d

Worker   | Beads | Pass | Fail | Pass Rate | Total Cost
alpha    |    28 |   24 |    4 |    85.7%  |   $11.76
bravo    |    31 |   27 |    4 |    87.1%  |   $12.09
charlie  |    25 |   20 |    5 |    80.0%  |   $10.50
```

### A/B Testing

When modifying a template, assign workers to template variants:

```yaml
# .needle.yaml
prompt:
  templates:
    pluck:
      variants:
        - name: pluck-v3
          weight: 50        # 50% of workers get v3
          content_file: templates/pluck-v3.md
        - name: pluck-v4
          weight: 50        # 50% of workers get v4
          content_file: templates/pluck-v4.md
```

Worker assignment is deterministic: `hash(worker_id) % 100 < weight` determines which variant a worker uses. This ensures the same worker always uses the same variant within a session.

After sufficient beads (configurable threshold, default 50 per variant), `needle stats` shows a comparison. The operator promotes the winner.

## Cross-Workspace Knowledge

### Global Learnings

A global learnings file at `~/.config/needle/global-learnings.md` is loaded into all workspace prompts as supplementary context. Contains cross-cutting lessons:

- Infrastructure quirks (git, ssh, API behaviors)
- Tooling gotchas (br CLI edge cases, compiler warnings)
- General coding patterns (not workspace-specific)

**Population:** When the consolidator detects a learning that appears across 2+ workspaces, it promotes a copy to global learnings.

**Size limit:** 40 entries (cross-cutting lessons should be distilled).

### Label-Based Skill Sharing

Skills tagged with generic labels (`rust`, `kubernetes`, `api`, `testing`) are available to any workspace with matching labels in `.needle.yaml`:

```yaml
# .needle.yaml for kalshi-weather
workspace:
  labels: [rust, api, trading]
```

During prompt building, the PromptBuilder checks both workspace-local skills and global skills matching the workspace's label set.

## Future: Meta-Agent Harness Optimization (AutoAgent Pattern)

This section describes a potential Phase 5 capability. It is not part of Phase 4 but is documented here to inform Phase 4's design decisions (trace capture format, template versioning, stats infrastructure).

### Concept

A meta-agent that reads NEEDLE telemetry, execution traces, and bead outcomes, then modifies prompt templates, tool configurations, and orchestration logic to improve fleet-wide success rates.

Following AutoAgent's architecture:
- **Meta-agent** reads `needle stats`, execution traces, and failure patterns
- **Task agents** are the normal NEEDLE workers
- **The edit surface** is the prompt templates, tool configs, and `.needle.yaml` settings
- **The fixed boundary** is NEEDLE's core: state machine, claiming protocol, telemetry, strand waterfall

### Prerequisites (Built in Phase 4)

- Structured trace capture (traces must be machine-readable)
- Template versioning (must be able to create and track template variants)
- Stats infrastructure (must be able to measure improvement)
- Skill library (must have a place to store discovered tools/procedures)

### Key Design Constraints (from AutoAgent Learnings)

1. **Meta-agent is separate from task agents.** It runs as a distinct process, not within the worker loop.
2. **Same-model pairing.** Meta-agent should use the same model family as fleet workers.
3. **Git-versioned edits.** Every template modification is a git commit for traceability and rollback.
4. **Hill-climb on pass rate.** Keep/discard is strictly score-driven. Traces inform what to try; scores determine what to keep.
5. **The overfitting test.** "If this exact bead disappeared, would this still be a worthwhile template improvement?"
6. **Prompt tuning has diminishing returns.** The meta-agent should focus on tool design and orchestration improvements, not just prompt rewording.

## Phase 4 Deliverables

| Component | Scope |
|-----------|-------|
| **Trace capture** | Adapter-specific structured trace collection, storage in `.beads/traces/` |
| **Retrospective instructions** | Pluck template extension with learning block |
| **Workspace learnings** | `.beads/learnings.md` automatic injection, size management |
| **Reflect strand** | Consolidation daemon as strand 7 + `needle reflect` CLI |
| **Skill library** | `.beads/skills/` with promotion lifecycle, skill retrieval in PromptBuilder |
| **Template versioning** | Version tags in telemetry, A/B variant assignment |
| **Stats engine** | `needle stats` command with template/task-type/worker aggregation |
| **Global learnings** | Cross-workspace learning promotion |
| **Label-based skill sharing** | Cross-workspace skill retrieval by label match |
| **Trace retention** | Configurable cleanup of trace files |
| **Session transcript analysis** | Parse Claude Code JSONL transcripts, extract action-outcome pairs, merge with bead-body patterns |
| **Drift detection** | Session similarity clustering, approach divergence classification (evolved/inconsistent/degraded) |
| **ADR decision records** | Decision point detection from transcripts, ADR-lite format in CLAUDE.md, full ADRs in `.beads/decisions/` |
| **CLAUDE.md placement** | Lowest-common-ancestor directory resolution for promoted learnings, auto-create if missing |

### Success Criteria

- [x] Traces are sanitized before write using vendored gitleaks rules (222 patterns) — no unsanitized window on disk (src/sanitize/mod.rs, gitleaks integration)
- [x] Keyword pre-filter (Aho-Corasick) skips irrelevant rules; sanitization adds <10ms per trace file (src/sanitize/mod.rs, aho-corasick crate)
- [x] Custom sanitization patterns in `.needle.yaml` are applied alongside gitleaks rules (src/config/mod.rs, custom_patterns)
- [x] `needle update-rules` fetches latest `gitleaks.toml` from upstream (src/cli/mod.rs, cmd_update_rules)
- [x] Workers produce structured execution traces for all adapter types (src/trace/mod.rs, trace capture)
- [x] Pluck template includes retrospective instructions; >80% of closed beads contain a retrospective block (src/prompt/mod.rs, retrospective template)
- [x] `.beads/learnings.md` is automatically injected into prompts when present (src/config/mod.rs, context_files discovery)
- [x] Reflect strand runs after 10+ beads closed, consolidates learnings, prunes stale entries (src/strand/reflect.rs, consolidation logic)
- [x] Learnings that appear 3+ times are promoted to skills in `.beads/skills/` (src/learning/mod.rs, skill promotion)
- [x] Skills are retrieved by label/task-type match and injected into prompts (src/skill/mod.rs, skill retrieval)
- [x] `needle stats` shows pass rates by template version, task type, and worker (src/stats/mod.rs, aggregation engine)
- [x] A/B template variants assign workers deterministically and track outcomes separately (src/prompt/mod.rs, variant assignment)
- [x] Learnings appearing in 2+ workspaces are promoted to global learnings (src/learning/mod.rs, global promotion)
- [x] Trace retention automatically cleans old traces per configured policy (src/learning/mod.rs, trace retention cleanup)
- [x] A worker that encounters a previously-solved failure mode receives the relevant skill in its prompt (src/prompt/mod.rs, skill injection)
- [ ] Fleet-wide pass rate measurably improves over a 30-day period (tracked via `needle stats`) (operational metric, requires fleet data)
- [x] Reflect parses Claude Code session JSONL transcripts and extracts action-outcome pairs (src/transcript/mod.rs, transcript parsing)
- [x] Transcript-derived patterns are merged with bead-body patterns, deduplicated by semantic similarity (src/learning/mod.rs, pattern merging)
- [x] Reflect detects session clusters solving similar problems and classifies approach divergence (src/drift/mod.rs, similarity clustering)
- [x] Drift reports categorize as evolved (promote latest), inconsistent (flag for review), or degraded (flag regression) (src/drift/mod.rs, divergence classification)
- [x] Decision points are detected from transcripts (failure → different approach → success sequences) (src/decision/mod.rs, decision detection)
- [x] Promoted learnings with decision context are written in ADR-lite format in CLAUDE.md (src/learning/mod.rs, ADR-lite format)
- [x] Full ADRs stored in `.beads/decisions/<bead-id>.md`, CLAUDE.md entries reference them via `**ADR:**` line (src/decision/mod.rs, ADR storage)
- [x] Promoted learnings are placed in the CLAUDE.md at the lowest common ancestor of contributing workspaces (src/claude_md_placement.rs, LCA resolution)

### Estimated Scope

~12 additional source files, ~5,200 additional LOC.

New module additions:
```
needle (binary)
├── ... (existing modules)
├── learning/          Retrospective extraction, learnings management
├── skill/             Skill library, retrieval, promotion
├── trace/             Trace capture, storage, retention
├── transcript/        Session JSONL parsing, action-outcome extraction
├── drift/             Session similarity, clustering, divergence detection
├── decision/          Decision point detection, ADR management
├── placement/         CLAUDE.md lowest-common-ancestor resolution
└── stats/             Aggregation engine, A/B comparison
```

Dependency additions:
```
learning    ──► bead_store, telemetry, types
skill       ──► bead_store, config, types
trace       ──► dispatch, config, types
transcript  ──► config, types
drift       ──► transcript, telemetry, types
decision    ──► transcript, types
placement   ──► config, types
stats       ──► telemetry, config, types
```

# Phase 5: Fleet Robustness — Explore Strand Hardening

**Status:** planned (ADR-001). The meta-agent concept sketched above as a "potential Phase 5" remains future, unnumbered work.

**Goal:** make multi-workspace roaming (the explore strand) a reliable dispatch path instead of a best-effort one. Driven by the 2026-07-11 lab incident: 24 ready beads across 24 workspaces, 4 roaming workers, throughput of ~1 bead per 40 minutes, with one workspace's unclaimable beads deadlocking the entire scan loop. Full evidence and rationale in [ADR-001](../adr/001-explore-strand-hardening.md).

## Changes

### 5.1 Selection correctness
- **Claimable-aware candidate filtering.** `ExploreStrand::evaluate` returns at the first workspace with ready candidates, but "ready" ignores the worker's exclusion state and unclaimable assignees — a workspace whose candidates are all excluded/assigned traps every worker forever. Feed the worker's exclusion set into `Filters` so the loop advances past workspaces with nothing *claimable by this worker*.
- **Per-worker scan-order rotation.** Start iteration at `hash(qualified_id) % N`, wrapping around, so workers partition the workspace list instead of converging on the same first store and racing.
- **Store-layer limit correctness.** The `br ready --json` path passes no `--limit` (default truncation hides low-priority beads); another path passes `--limit 0` (returns nothing on deployed bead-forge 0.2.0). Always pass an explicit large limit; add a boot-time `bf --version` handshake that WARNs on known-bad versions.

### 5.2 Stale-state healing
- **Mend releases stale assignees on open beads.** Cross-workspace mend only handles orphaned in-progress beads today; an open bead with a dead assignee is permanently invisible to all workers. Clear assignees with no live heartbeat. **Note:** As of 2026-08-24 (ADR-018), `bead reopen` clears the assignee, fixing the reopen-specific case. Mend's assignee healing remains valuable for other cases where beads become stuck with stale assignees.
- **Claim-error ≠ race-lost.** Claim CLI errors currently collapse into `claimed_by=(race)`. Distinguish them; after N consecutive claim errors on one bead, emit an ERROR telemetry event and mark the bead/store suspect instead of silently cycling.

### 5.3 Cadence and liveness
- **Event-driven wakeups + jittered floor.** Replace the flat idle backoff (observed at 900s) with mtime/inotify watches on each workspace's `.beads/issues.jsonl` plus a jittered 60–120s polling floor. Found-but-excluded is a short-retry case, never idle.
- **Periodic re-discovery.** The workspace list is captured at boot; re-run discovery every N cycles (or on directory-create events) so new stores don't require worker restarts. The no-upward-traversal constraint stays.

### 5.4 Observability
- **Per-cycle scan telemetry** (workspaces visited, candidates, exclusion reasons) and a **starvation alarm**: ready beads exist in scanned stores but this worker claimed nothing for X minutes → WARN event; surface last-scan-per-workspace in `needle status`.

### 5.4b Strand-error reliability (found 2026-07-15 dispatching fresh roaming workers)
Three further gaps, found live: a worker went `EXHAUSTED` with 0 beads claimed despite `explore` finding a real candidate. Root cause was three compounding issues, not the deadlock/herd problems above:
- **`weave` stalled 237s before failing** with the same error `mend`/`unravel`/`knot` hit in 0–63ms in the same cycle, against the same store — something in weave's `bf list --json` path differs from the others and needs its own investigation, plus a bounded timeout so no single strand can stall a whole selection cycle for minutes (`bf-5hlhn`).
- **Home-store-missing and genuine failure both report as `error`.** A pure-roam worker (home = no `.beads/`) will always show `pluck`/`mend`/`weave`/`unravel`/`reflect`/`knot` as `error` every cycle by design — indistinguishable from a real bug without grepping raw stderr. Needs a distinct `no_home_store` (or similar) result (`bf-6c8vp`).
- **The underlying `bf list`/`bf list --json failed` message recurs historically (07-09, 07-10 stderr logs) against workspaces with genuinely valid stores** (ARMOR, HOOP) — a longer-standing CLI reliability issue independent of the no-home-store case, currently swallowing `bf`/`br`'s real stderr behind a generic wrapper message (`bf-2e4mc`).

### 5.5 Testing
- Liveness property test: N workers × M workspaces, every ready bead eventually claimed.
- Deadlock regression test: candidates in workspace #2 while workspace #1 has only excluded/assigned beads.
- CLI-args regression tests pinning explicit `--limit` behavior.

### 5.6 Deployment
- Version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, then staged fleet rollout through the canary channel (`:testing` → `:stable`). Never overwrite a running binary in place; verify with `needle status` on both hosts.

## Exit criteria
- The 2026-07-11 scenario reproduced in a test (24 stores, 1 hot store with unclaimable beads) drains completely with 4 workers in minutes, not hours.
- A bead flushed into any registered store is claimed without a worker restart and without a 15-minute wait.
- `needle status` answers "when did each workspace last get scanned, and why was nothing claimed" directly.

# Phase 6: Pluck Telemetry Isolation, Fleet Process Tracking, and Concurrent-Dispatch Safety

**Status:** planned (ADR-002; §6.5/§6.6 found after ADR-002 was written, not yet their own ADR).

**Goal:** stop Pluck's own operational confusion from leaking into a target repo as fabricated work, make `needle stop`/`needle status`/`needle list` trustworthy enough to use during incident response without a manual `ps aux` cross-check, make the fleet's own failure-handling strands (Splice, Unravel) actually catch a worker that's stuck retrying a human-gated bead forever, and make it safe for two workers to share one workspace without racing on git state. Driven by an 8-day incident on `~/ARMOR` (2026-07-06 through 2026-07-14): a Pluck starvation self-diagnostic was written as a bead into ARMOR's own tracker, could never legitimately resolve (its "fix" target was NEEDLE's own config, unreachable from ARMOR), and the unresolved loop spiraled into 346 fabricated beads and ~2,300 wasted bead-cycles across two workers over 8 days — one of which (`bravo`) kept running after `needle stop` reported success, and a third (`alpha`) was invisible to `needle status`/`needle list` entirely. Full evidence and rationale in [ADR-002](../adr/002-pluck-telemetry-isolation-and-process-tracking.md). Two same-day follow-up checks found further gaps: why didn't Splice/Unravel stop this class of incident on their own (§6.5), and whether it's actually safe to run two workers in one workspace at all (§6.6).

## Changes

### 6.1 Pluck telemetry isolation
- **Redirect the starvation self-diagnostic to NEEDLE's own telemetry.** `PluckStrand` (`src/strand/pluck.rs`) must never write a bead into the workspace it is scanning. Emit a `pluck.starvation_detected` event through the existing telemetry pipeline instead, with workspace path, open/excluded counts, and candidate exclusion reasons.
- **If a persistent record is needed, file it in NEEDLE's own workspace**, never the target's — apply the same isolation rule anywhere else Pluck (or any strand) might be tempted to write its own operational state as a bead in a scanned repo.
- **Filter target-repo auto-decomposition for NEEDLE-internal work.** A worker dispatched against a target repo should never be prompted to "investigate/fix Pluck configuration" or equivalent — that class of task has no legitimate resolution path from inside the target repo and should be recognized and rejected at decomposition time, not left to spiral.

### 6.2 Fleet process tracking
- **`needle stop` kills the full process tree.** Parent `needle run` process, its `bash -c` prompt wrapper, and the dispatched `claude` subprocess — not just the tmux registry entry. Verify the PID is actually gone before reporting success.
- **`needle status`/`needle list` must have no blind spots.** Every live `needle run --workspace ...` process must be discoverable through standard fleet commands regardless of how it was started (tmux-wrapped or bare background). Add a reconciliation check (registry view vs. `ps aux` process-table view) that WARNs on any unregistered `needle run` process.

### 6.5 Splice/Unravel loop-detection gap
Found investigating whether the fleet's own failure-handling strands activated during the 2026-07-14 ARMOR (`bf-34xw9`) and Commitgraph (`bf-39by`) retry-storm incidents (both: a worker retrying a human-gated-access bead forever). They didn't — confirmed via telemetry (`bf-34xw9`: 41 `bead.claim.succeeded`, 25 `bead.orphaned`, 42 `agent.completed`, only 1 real `bead.completed`, over 24h) and code review, for three compounding reasons:
- **Detector blind spot.** `SpliceStrand::detect_claim_churn` only counts `bead.claim.race_lost` — this pattern is claim-succeeded-then-orphaned-then-reclaimed, a different signal no detector reads. `detect_state_ping_pong` and `detect_log_runaway` both gate on "no `agent.completed`/`bead.completed` event in the scan window" as their definition of no-forward-progress; a bead that completes an agent run 42 times without ever closing looks identical to real progress under this check, so both are permanently blind to this exact failure mode.
- **Missing config.** `document_failure`/`document_live_loop` silently no-op if `strands.splice.report_workspace` is unset — no bead, just a debug log line. The lab deployment's config has every other strand's section except `splice`'s, so this defaulted to `None` the whole time (confirmed: 455 `strand.evaluated` events fleet-wide in 24h, zero escalation beads created).
- **No connection back to the stuck bead.** Even when `document_live_loop` works as designed, it only creates a *new* side-report bead (`["worker-loop", "human"]`) in the report workspace — it never labels the original stuck bead itself. Pluck excludes `human`-labeled beads and Unravel only acts on them (§6.1's third bullet); a working Splice detection still wouldn't stop Pluck from redispatching the actual stuck bead, and Unravel would never see it.

Fix: a new detector for repeated claim+orphan cycles with high completion-without-resolution count; make `report_workspace` a validated, loudly-WARNed-if-missing config value; and have Splice label the *original* bead `human` directly when it detects a live loop, not just file a side report.

### 6.6 Bead-Id trailer race under concurrent same-workspace dispatch
Found evaluating whether it's safe to dispatch a second worker into `~/NEEDLE` alongside `charlie`, same workspace, same branch. `commit_hook::inject_bead_id_trailer` (`src/commit_hook.rs`) runs `git commit --amend --no-edit --trailer Bead-Id:X` on whatever commit is at HEAD after a successful dispatch. Its only safety check (`src/worker/mod.rs:2079`) is `current_head != pre_dispatch_head` — it never verifies the commit actually at HEAD is the one its own agent produced. Each `needle run` is a fully separate OS process with no lock or mutex between them, so nothing coordinates this across two workers sharing a workspace.

Race: worker A commits (HEAD=A1); before A reaches the trailer step, worker B also commits (HEAD=B1, on top of A1); A's check (`current_head(B1) != pre_dispatch_head(base)`) passes, so A amends B1 — rewriting its hash and mislabeling B's actual diff as belonging to bead A. This corrupts HOOP's `bead_commit_index` (the reason this trailer exists at all) and invalidates worker B's own subsequent bookkeeping. Unlike ordinary concurrent edits, this doesn't require the two beads to touch overlapping files — it's a race on *which commit is at HEAD*, not on file content.

Fix: a short-lived per-workspace advisory lock (e.g. `flock` on `<workspace>/.git/needle-trailer.lock`) held only across the read-HEAD → verify → amend sequence, not the whole dispatch — bounded by the function's existing 10–30s subprocess timeouts, so the throughput cost is negligible. Inside the lock, verify HEAD's commit message actually references this bead's ID (already present per the `fix(needle-XYZ): ...` commit convention) before amending; skip (and log a miss) rather than mislabel someone else's commit if it doesn't match. A further hardening step — swap `--amend` for `git notes add <verified-sha>`, which attaches metadata without rewriting the commit's hash and works even if the target commit is no longer at HEAD — is noted as a bigger, not-yet-required follow-up (HOOP would need to read notes instead of trailers).

### 6.7 Testing
- Regression test: Pluck starvation detection on a workspace with 0 claimable candidates emits a telemetry event and writes nothing to that workspace's `.beads/`.
- Regression test: `needle stop` on a worker mid-dispatch leaves no `needle run` or dispatched-agent process alive (process-table assertion, not just registry-state assertion).
- Regression test: a worker started via the non-tmux boot path (bare `NEEDLE_INNER=1` background invocation) still appears in `needle status`/`needle list`.
- Regression test: the `bf-34xw9` telemetry shape (41 `claim.succeeded`, 25 `bead.orphaned`, 42 `agent.completed`, 1 `bead.completed`) as fixture input — new detector fires and the original bead gets labeled `human`.
- Regression test: two concurrent `inject_bead_id_trailer` calls in the same workspace (simulated racing threads/processes against one repo fixture) never cross-tag each other's commit.

### 6.8 Deployment
- Version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, then staged fleet rollout through the canary channel (`:testing` → `:stable`).

### 6.9 Concurrent same-repo worker isolation

**Status:** Resolved — ADR-015 accepted 2026-08-15.

§6.6's fix (a `flock` around `inject_bead_id_trailer`'s read-HEAD→verify→amend sequence) closes the specific commit-mislabeling race, but the broader condition that makes it possible is untouched: NEEDLE gives every worker assigned to a repo the *same* working directory (`bead.workspace` is passed straight through as a raw path into `run_process()`, `src/dispatch/mod.rs:709`, and rendered verbatim as `{workspace}` — no per-worker suffix, clone, or `git worktree` derivation anywhere in `src/`). The per-workspace claim `flock` (§6.1/plan.md line 219) guards only the CLAIMING step's `br update --claim` call, not the EXECUTING phase — so once two workers are each dispatched into the same repo, their agents can run concurrent `git add`/`commit`/`reset --hard`/`checkout` in that one shared tree for the full duration of both dispatches, with nothing serializing them beyond whatever the agents themselves happen to do. §6.6's lock only protects NEEDLE's own post-hoc trailer amend, not the agent's own work — one agent's `git reset --hard` or `checkout` can still discard another's uncommitted changes while both are mid-execution.

**Decision:** Reject full per-worktree isolation. Accept shared working directories as a deliberate design constraint and enforce it operationally through fleet dispatch discipline (one worker per repository for build-heavy workspaces) and bead authoring guidelines (explicit blocking dependencies for overlapping work). See [ADR-015](../adr/015-concurrent-same-repo-worker-isolation.md) for full context, alternatives considered, and consequences.

## Exit criteria
- A workspace with beads Pluck cannot claim produces zero new beads in that workspace and one telemetry event in NEEDLE's own stream.
- `needle stop -i <session>` leaves zero matching `needle run`/dispatched-agent processes in `ps aux`, verified, not assumed.
- `needle status`/`needle list` output matches `ps aux | grep 'needle run'` 1:1 on a host with workers started via both the tmux and bare-background paths.
- A bead stuck in a claim→orphan→reclaim cycle for N cycles gets labeled `human` automatically and stops being redispatched, without a human having to notice the retry-storm first.
- Two workers dispatched concurrently in the same workspace never cross-tag each other's commits with the wrong `Bead-Id` trailer.

# Phase 7: Cleanup Command Orphan-Detection Gap

**Status:** partially implemented, with a verified regression (ADR-003 + addendum). `7.1` shipped in commit `b5ada58` (bead bf-1ep0s) but does not achieve its own exit criteria — see `7.1a`. `7.2`/`7.3` not done: no test caught the regression, and it has not shipped through the canary channel.

**Goal:** make `needle cleanup`'s no-flags behavior match its own documentation — remove only sessions with no live process behind them — so an operator (or Claude, acting on an operator's behalf) can run it during incident cleanup without first needing a manual `ps aux` cross-check, the same trust bar §6.2 already set for `needle stop`/`status`/`list`. Driven by a 2026-07-19 incident during lab fleet remediation: bare `needle cleanup` (intended to remove sessions for workers already stopped) instead matched and killed two live sessions — `armor-p6a` (an actively-executing worker, unrelated to the cleanup) and `needle-supervisor` (the fleet's own auto-scaling daemon) — because `cmd_cleanup`'s only actual filter is an identifier-substring match that defaults to matching everything when no identifier is given; despite its help text and doc comment both claiming an "orphaned sessions only" check, no liveness check exists anywhere in the implementation. Full evidence and rationale in [ADR-003](../adr/003-cleanup-orphan-detection-gap.md).

## Changes

### 7.1 Real liveness check as cleanup's default
- `cmd_cleanup` (`src/cli/mod.rs`), when called with neither `--all` nor `-i`, must only target sessions with no live process behind them — reuse `scan_needle_processes()` (the same reconciliation helper `cmd_list` already calls, and the one §6.2 commits to building out for `needle status`/`needle list`) rather than the current identifier-substring filter, which matches every session when the identifier is empty.
- `--all` keeps its current fully-destructive meaning; update its help text to say so explicitly ("removes every needle session, including live ones") rather than relying on a separate design doc to convey the danger.
- `-i <pattern>` keeps its current targeted/deliberate meaning (bypasses the liveness check, same as today) — naming a specific session is itself the operator's explicit choice; only the no-flags path's behavior changes.

### 7.1a Regression (found 2026-07-21): the shipped check compares the wrong PID and matches nothing

`b5ada58` implemented 7.1's filter as `s.pid.map_or(true, |pid| !live_pids.contains(&pid))`, where `s.pid` is `TmuxSession.pid` sourced from tmux's `#{pane_pid}` (`src/cli/mod.rs:4046,4086`) and `live_pids` is `scan_needle_processes()`'s output, which *deliberately excludes* shell-wrapper PIDs by design (`src/cli/mod.rs:4186-4205`, its own comment: "Exclude shell wrapper processes ... We only want to discover the actual needle worker process, not the shell wrapper").

Verified empirically by reproducing NEEDLE's exact launch shape (`launch_in_tmux()`, `src/cli/mod.rs:955-971`: `tmux new-session -d -s <name> "NEEDLE_INNER=1 <exe> <args> 2>> <log>"`):

```
$ tmux new-session -d -s needle-pidtest "NEEDLE_INNER=1 sleep 30 2>> /tmp/needle-pidtest.log"
$ tmux list-panes -t needle-pidtest -F '#{pane_pid}'
3322398
$ pstree -p 3322398
bash(3322398)---sleep(3322399)
```

`pane_pid` is the `bash -c` wrapper; the actual worker is a child with a *different* PID (the output redirection defeats bash's exec-optimization for a bare last command). So `s.pid` is always the wrapper's PID, which `scan_needle_processes()` structurally never returns — the containment check is `false` for every genuinely live tmux-launched session, every time. **7.1 as shipped does not reduce the 2026-07-19 incident's risk at all; it still classifies every live session as orphaned.** `cmd_stop` already has the correct primitive for this — `find_needle_process_in_tree()` (`src/cli/mod.rs:1198-1213`), which walks the descendant tree from `pane_pid` to find the actual `needle run` process — `cmd_cleanup`'s liveness check must call it (or fold it into `scan_needle_processes()` as a tree-walking variant) instead of comparing `pane_pid` directly. Full writeup: [ADR-003 Addendum](../adr/003-cleanup-orphan-detection-gap.md#addendum-2026-07-21-the-shipped-fix-bf-1ep0s--commit-b5ada58-is-itself-broken).

### 7.2 Testing
- Regression test: `needle cleanup` with no flags, given one live session and one session with no backing process, removes only the dead one.
- Regression test: `needle cleanup` with no flags and zero dead sessions removes nothing and says so, even when live sessions exist.
- Regression test: `needle cleanup --all` still removes every session regardless of liveness (unchanged behavior, pinned explicitly so it can't regress silently while fixing the no-flags path).
- **New, required by 7.1a:** the above tests must exercise a real `tmux new-session -d -s <name> "NEEDLE_INNER=1 <cmd> ... 2>> <log>"` invocation (or an equivalent fixture that reproduces the shell-wrapper-vs-child PID split), not a `TmuxSession`/`DiscoveredProcess` struct constructed directly in the test — that shortcut is exactly what let 7.1a ship undetected, since it bypasses the real indirection the bug lives in.

### 7.3 Deployment
- Version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, then staged fleet rollout through the canary channel (`:testing` → `:stable`), per the existing convention (§6.8).
- Until 7.1a is fixed and this ships, bare `needle cleanup` should be treated as equivalent to `--all` operationally — the documented safety property does not currently exist, regardless of which commit is deployed.

## Exit criteria
- Bare `needle cleanup` on a host with any mix of live and dead needle sessions removes only the dead ones, verified against `ps aux`, not assumed — using the process's actual PID (walked from `pane_pid` through the tree), not the pane's shell PID.
- `needle cleanup --all`'s own `--help` text states plainly that it removes live sessions too.
- The 2026-07-19 incident (bare cleanup killing `armor-p6a` and `needle-supervisor`) is reproduced as a **tmux-backed** regression-test fixture (per 7.2) and does not recur.

# Phase 8: Recursive Workspace Discovery as Explore's Default, Static List as Pinning Exception Only

**Status:** planned (ADR-004).

**Goal:** restore the originally-intended design — `ExploreStrand` recursively discovers every workspace under `workspace_root` (`/home/coding`) by default, and the `explore.workspaces` config list exists only as a deliberate, exceptional override for pinning a specific worker to a fixed set of repos, never as the normal way to populate the fleet's scan scope. Driven by a 2026-07-19/20 finding during lab fleet remediation: `explore.workspaces` in the live config had been populated with a static, 24-entry enumeration of "all known repos at the time," which — per `ExploreStrand::new()`'s own doc comment ("If `workspaces` is empty, auto-discovers all dirs with `.beads/` under the configured `workspace_root`") — completely bypasses the real, working `discover_workspaces()` recursive-scan code, since that path only runs when the list is empty. The static list is now stale: `commitgraph` and `twitterapi-proxy` both have real `.beads/` directories but are absent from it, making them permanently invisible to every roaming worker regardless of any other fix. This compounds the already-filed `bf-4df1e` (Explore stops scanning at the first workspace with any candidates) — even once that's fixed, a stale static list still hides whole repos from the scan entirely. Full evidence and rationale in [ADR-004](../adr/004-recursive-workspace-discovery-default.md).

## Changes

### 8.1 Recursive discovery is the unconditional default
- `ExploreStrand::new()` (`src/strand/explore.rs`) must call `discover_workspaces(&config.workspace_root)` as the baseline workspace list whenever `config.workspaces` is not being used for a deliberate pin — not merely "when the list happens to be empty" as an incidental side effect of current config state, but as the designed default behavior an operator has to deliberately opt out of, not accidentally fall into.
- `config.workspaces`, when explicitly set by an operator, is a **pin/exception list** — scoping a specific worker to a fixed, restricted set of repos for a deliberate operational reason (e.g. a dedicated worker that must never touch anything outside 2-3 sensitive repos). It must remain fully configurable, but must never be treated as required or default fleet-wide configuration, and its presence should not silently disable discovery for every worker that doesn't need pinning.

### 8.2 Immediate operational fix (config, not code)
- Clear the live lab config's `explore.workspaces` list back to empty. None of its current 24 entries represent a genuine pinning exception — they were simply an enumeration of known repos, which is exactly what `discover_workspaces()` already produces, minus the two it's missing (`commitgraph`, `twitterapi-proxy`).
- This can ship independent of the 8.1 code change, since the current code already does the right thing when the list is empty — the bug is entirely that the list was populated with something that should never have been treated as exhaustive, ongoing configuration.

### 8.3 Open question: discovery staleness within a worker's lifetime
`ExploreStrand::new()`'s own doc comment: "The workspace list is captured at construction time and never re-read." Even with 8.1 fixed, a long-lived worker won't see a brand-new repo created after it started without a restart. Not solved by this phase — flagged for a follow-up decision (e.g. periodic re-discovery on an interval, or accept restart-to-pick-up-new-repos as adequate given workers already cycle relatively often).

### 8.4 Testing
- Regression test: `ExploreStrand::new()` with an empty `config.workspaces` and a `workspace_root` containing several `.beads/`-having directories produces a worker that scans all of them, not a hardcoded subset.
- Regression test: `ExploreStrand::new()` with a non-empty `config.workspaces` (simulating a deliberate pin) scans exactly that list, never falling back to discovery — preserves the exception mechanism's own correctness while fixing the default.
- Fixture test reproducing the exact 2026-07-19/20 finding: a `workspace_root` containing `commitgraph` and `twitterapi-proxy` (both with `.beads/`) alongside the other 24 known repos — with an empty `workspaces` config, all 26 are discovered, not just the 24 that happened to be hand-listed.

### 8.5 Deployment
- Version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, then staged fleet rollout through the canary channel (`:testing` → `:stable`), per the existing convention (§6.8/§7.3).

## Exit criteria
- A fresh worker with `explore.workspaces` unset (or emptied) discovers every `.beads/`-having directory under `workspace_root`, including `commitgraph` and `twitterapi-proxy`, without any manual list maintenance.
- Setting `explore.workspaces` explicitly still restricts a worker to exactly that list — the pin/exception mechanism keeps working for whoever deliberately wants it.
- The live lab config no longer carries a stale, exhaustive-looking static list where an empty (discovery-driven) one was intended.

# Phase 9: Unify GitHub-Release Upgrade with the Canary-Gated Hot-Reload Channel

**Status:** accepted, not yet implemented. [ADR-005](../adr/005-unify-release-upgrade-with-canary-hot-reload.md) was **accepted 2026-08-12**; Phase 9 is authorized work. Two facts from the acceptance re-verification change the implementation and are normative here: the version comparison must be **strictly greater**, never merely different — as of 2026-08-12 the installed binary (`0.2.19`) is *three releases ahead* of the latest published release (`v0.2.16`), so a "versions differ, fetch latest" implementation would silently downgrade the whole fleet through the `:testing` slot; and `hot_reload: true` is already live in `.needle.yaml` with `~/.needle/bin/needle-stable` present, so anything that writes `needle-stable` takes effect fleet-wide **today**, without a canary. Sequence this ahead of ADR-013's staged rollout — Phase 9 is the mechanism that makes that rollout survivable.

**Goal:** make a new GitHub release reach every live worker on every fleet host automatically, canary-validated, without a human having to remember to SSH into each host and run `needle upgrade`. Driven by a 2026-07-20 finding during the fleet-wide deployed-artifact improvement review: this repo already ships two binary-update mechanisms — the manual, canary-free `needle upgrade` CLI command, and a fully automatic, already-tested, canary-gated hot-reload pipeline that runs inside every worker's own loop every cycle — but the two are structurally disconnected. The automatic pipeline only ever reacts to a `needle-testing` binary written by the (currently disabled) self-modification pipeline; nothing writes a `needle-testing` binary from a GitHub release. Confirmed live on the ex44 host during this review: the installed `needle` binary reported version `0.2.11` while GitHub's latest published release was `v0.2.12`, published the same day — a real, present instance of the gap, not a hypothetical. Full evidence and rationale in [ADR-005](../adr/005-unify-release-upgrade-with-canary-hot-reload.md).

## Changes

### 9.1 Release-to-`:testing` download step
- Add a function alongside `check_for_update()` (`src/upgrade/mod.rs`) that, on finding a newer GitHub release, downloads it to `~/.needle/bin/needle-testing` (the same path the self-modification pipeline already targets) instead of `perform_upgrade()`'s current direct-overwrite-of-`env::current_exe()` behavior. `perform_upgrade()` / `needle upgrade` itself is unchanged — this is an additional path, not a replacement.
- Skip the write (and log why) if a `:testing` binary is already present and unpromoted, so a supervisor-driven release check can never clobber an in-flight self-modification candidate mid-canary.

### 9.2 Supervisor-driven periodic check
- `needle supervise` (`src/supervisor/mod.rs`) gains a poll loop calling the 9.1 function on an interval, new config `supervisor.update_check_interval_secs` (default 21600 / 6h).
- Gated behind new config `supervisor.auto_upgrade_check: bool` (default `false`) — independent of `self_modification.enabled`, since a tagged/published release is a different trust level than an agent's own self-edit. Promotion-automatic-vs-manual still reuses the existing `self_modification.auto_promote` flag; no second flag for the same decision.

### 9.3 No changes required to propagation
- `check_auto_canary()` and `check_hot_reload()` (`src/worker/mod.rs`) already implement canary-gating, promotion, and safe-boundary hot-reload (never mid-dispatch) — confirmed by direct read and existing unit tests (`promote_moves_testing_to_stable`, etc. in `src/canary/mod.rs`). They pick up a release-sourced `:testing` binary with zero modification once 9.1/9.2 land.

### 9.4 Testing
- Regression test: 9.1's download function, given a mocked newer-release response, writes to `needle-testing` and does not touch the currently-running binary.
- Regression test: 9.1's download function, given an already-present unpromoted `needle-testing`, skips the write and logs the skip reason.
- Regression test: with `supervisor.auto_upgrade_check: true` and a mocked release, a full poll cycle results in a promoted `:stable` and a subsequent worker-loop hot-reload — exercised against the existing canary-workspace fixtures.
- Validation item (not a standard regression test): confirm the existing canary-workspace fixtures (`~/.needle/canary/`) give adequate coverage for a full release-level binary swap, not just source-level self-modification deltas — a real release diff may change more surface (new CLI flags, new default adapters) than the fixtures were built against.

### 9.5 Deployment
- Ships `auto_upgrade_check: false` by default — prove on one host (ex44) for a full release cycle before flipping the fleet default, per the same "opt-in, prove on one host first" discipline already applied to weave/unravel/pulse. Version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, staged rollout through the canary channel (`:testing` → `:stable`), per the existing convention (§6.8/§7.3/§8.5) — this phase's own change set gets promoted through the very mechanism it's building.

## Exit criteria
- A fresh GitHub release, published while a supervisor daemon is running with `auto_upgrade_check: true`, results in every live worker on that host running the new version within one `update_check_interval_secs` + one canary cycle, with no human action required.
- A failing canary against a release-sourced `:testing` binary leaves `:stable` untouched and the fleet running the previous version — a bad release cannot silently propagate.
- `needle upgrade` (manual path) continues to work unchanged for fresh installs and single-host immediate upgrades.

## ADR-005: 2026-07-20 — Unify the GitHub-Release Upgrade Path with the Canary-Gated Hot-Reload Channel

### Context

NEEDLE ships two binary-update mechanisms that share vocabulary (`:testing` / `:stable`, "canary", "hot-reload") but are structurally disconnected — confirmed by direct code read during this review, not assumption:

1. **Manual GitHub-release upgrade** — `needle upgrade` (`perform_upgrade()`, `src/upgrade/mod.rs`). Downloads the latest GitHub release and `fs::rename`s it directly over `env::current_exe()` — in place, on whatever host and path the operator happens to be running it from. No canary validation. No fleet propagation: a human must run this on every host individually, and nothing does so automatically.
2. **Self-modification canary/hot-reload channel** — `src/canary/mod.rs` plus `check_auto_canary()` / `check_hot_reload()` in `src/worker/mod.rs`. A real, implemented, unit-tested automatic pipeline: every worker's own loop, every cycle, between LOGGING and SELECTING, detects a `~/.needle/bin/needle-testing` binary, canary-validates it against `~/.needle/canary/`, promotes to `needle-stable` on an all-pass (backing up the previous stable to `needle-stable.prev` for rollback), and every live worker on that host `exec()`s into the new binary with `--resume` at its next safe loop boundary — never mid-dispatch. This works today, but is gated behind `self_modification.enabled && self_modification.auto_promote` (both `false` in the live `.needle.yaml`), and — separately from that gate — **nothing anywhere in the codebase ever writes a `needle-testing` binary from a GitHub release.** The only producer of that path today is the self-modification pipeline itself (an agent editing NEEDLE's own source and building it locally).

Live evidence gathered during this audit, ex44 host, 2026-07-20: `needle --version` reported `needle 0.2.11`; GitHub's `releases/latest` API reported `v0.2.12`, `published_at: 2026-07-20T12:49:30Z` — published the same day, and never picked up by either existing path. The fleet runs up to `worker.max_workers: 10` per host across at least two hosts (ex44, lab), each worker an independent, long-lived tmux-session loop, deliberately without a central orchestrator (README: "coordination happens through the shared bead queue"). Any fix has to preserve that property rather than introduce a controller that pushes commands to hosts.

### Decision

Route GitHub releases through the *existing* `:testing` slot instead of building a second, weaker auto-update path: add a download step (reusing `check_for_update()`'s version check) that writes a newer release to `~/.needle/bin/needle-testing`, triggered periodically from `needle supervise` (the fleet daemon that already runs continuously per host, independent of dispatch, and already owns fleet-wide operational decisions like auto-scaling) via a new `supervisor.auto_upgrade_check` flag (default `false`) and `supervisor.update_check_interval_secs` (default 6h). `check_auto_canary()` and `check_hot_reload()` — already running in every worker's loop — pick up a release-sourced `:testing` binary with zero code changes, since they only ever look at file paths and hashes, not provenance. Promotion-automatic-vs-manual continues to be governed by the existing `self_modification.auto_promote` flag rather than a new one. `needle upgrade` / `perform_upgrade()` remains available unchanged for the manual/immediate/fresh-install case. Full detail, alternatives, and evidence: [ADR-005](../adr/005-unify-release-upgrade-with-canary-hot-reload.md).

### Alternatives Considered

1. **Canary-validate inside `perform_upgrade()`, keep it manual-only.** Rejected as the primary fix — still requires a human to remember to run it per host, exactly the condition that produced the observed drift. Worth doing as a small independent hardening regardless (today the manual path installs with zero validation); filed as a separate, smaller bead.
2. **Central push** (a control host SSHes into every fleet host and runs `needle upgrade`). Rejected — reintroduces the single controller NEEDLE's design explicitly avoids, and no host-inventory/SSH-fanout tooling exists for the fleet today.
3. **Check on every worker-loop iteration instead of via the supervisor.** Rejected — couples a GitHub API call and download to bead-dispatch latency, and roaming/short-lived Explore-strand workers don't have a reliable idle moment for it; the supervisor daemon exists specifically as the per-host, dispatch-independent decision-maker.
4. **Do nothing automatic — just print a "N releases behind" warning in `needle status`.** Rejected as a complete fix (still needs a human to notice and act, per host, per release), but cheap enough to ship immediately as a stopgap; filed separately.

### Consequences

- **Positive:** closes the exact drift class observed live during this audit by reusing machinery that already exists, is already unit-tested, and already has a rollback story (`needle rollback`) — not by inventing a new, weaker "just overwrite it" mechanism.
- **Positive:** preserves the no-central-orchestrator principle — every host polls and validates independently; no host depends on another host or a controller.
- **Risk:** the canary suite's existing fixtures were tuned for agent-authored source-level self-modification deltas; not yet confirmed they give adequate coverage for a full official-release binary swap (potentially larger surface change per hop). Needs its own validation pass before `auto_upgrade_check` becomes the fleet default.
- **Risk:** two producers can now write `needle-testing` (self-modification and the new supervisor check) — needs the mutual-exclusion rule described in §9.1 so they can't clobber each other mid-validation.
- **Deferred:** automatic rollback triggered by post-promotion outcome-rate anomalies, using the existing `needle rollback` primitive — reasonable future hardening, not required for v1.

# Phase 10: Bead Lifecycle Reliability — Test Isolation, Failure Quarantine, and Liveness-Independent Reclamation

**Status:** 10.2 implemented (ADR-012, 2026-07-30); 10.1/10.3/10.4 planned (ADR-006).

**Goal:** stop beads from getting stuck in states NEEDLE has no mechanism to recover from, and stop NEEDLE's own test suite from being the thing that puts them there. Driven by a 2026-07-21 lab fleet audit that found the fleet wasn't resource-starved (load 3.0/12 cores, 42G RAM free) but *data-quality-starved*: ~284 phantom `in_progress`/stale-assigned beads across ~22 real repos, six roaming workers permanently `EXHAUSTED` behind stale-assignee-only candidate pools despite real ready work existing elsewhere, and — reviewed in the same pass — a long-standing, still-open gap where a bead too large for one turn can fail hundreds of times with no automatic stop (a prior incident: 310 failures/24h on one bead, ~$500). All three root causes trace back to the same weakness: nothing in NEEDLE notices and corrects a bead stuck in a state it shouldn't be able to stay in indefinitely. Full evidence and rationale in [ADR-006](../adr/006-bead-lifecycle-reliability.md).

## Changes

### 10.1 Test-suite fixture isolation
- `dead_worker_cleanup_integration` (`tests/integration_tests.rs`) spawns the real compiled `needle` binary without overriding `HOME` or disabling Explore — `explore.enabled` defaults `true` and `explore.workspace_root` defaults to the real `$HOME` inherited from the test process. Fix: override `HOME` to a tempdir for this test, and either disable Explore in its config or add an `explore_workspace_root` override to `CliOverrides` so the test's subprocess never scans real filesystem paths.
- Audit the rest of the real-binary-spawning test suite (any `Command::new(CARGO_BIN_EXE_needle)` call) for the same gap — this is the second time this exact mechanism has produced real contamination (the first, 2026-07-20, left ~284 phantom beads under fixture worker identifiers across ~22 repos).
- Document the policy explicitly in this repo's CLAUDE.md Testing section: any test spawning the compiled binary as a real subprocess must isolate `$HOME` and Explore's scan root.

### 10.2 Failure circuit-breaker — IMPLEMENTED (ADR-012, 2026-07-30)
- After K consecutive failures on a bead (config `outcome.quarantine_after_failures`, default 5 — above Pluck's existing `split_after_failures` default of 3, so mitosis gets first crack at splitting), `handle_failure` (`src/outcome/mod.rs`) sets `status: blocked` (new `BeadStore::block` primitive), adds a `cycling` label, and emits a `bead.quarantined` telemetry event so the `auto`/Pluck strand stops re-claiming it.
- Mitosis's `NotSplittable` verdict (`src/worker/mod.rs`, previously a silent fallthrough) no longer needs its own check — the quarantine ceiling in `handle_failure` runs before mitosis evaluation every cycle, so a bead at/past the threshold is already `Blocked` by the time mitosis would have fallen through on it again.
- Quarantine, not auto-split — shipped as the safe MVP, per plan. Auto-splitting on threshold via mitosis remains a larger, separately-decidable follow-on, not done.
- Bonus, not in the original §10.2 scope: `PluckStrand::sort_candidates` now also weighs `failure_count` into its ordering (priority ASC, failure_count ASC, created_at ASC, id ASC), so a struggling bead stops monopolizing the queue even before it reaches the quarantine threshold. See ADR-012.
- Still open, not done here: `needle status`/`needle logs` do not yet have a dedicated view for `cycling`-labeled beads (a filter or summary count) — quarantine is currently only visible via `bf`/`br` label queries, not surfaced in the fleet's own tooling.

### 10.3 Mend releases stale assignees on Open beads
- This is Phase 5.2's original promise ("Mend releases stale assignees on open beads"), never implemented — `cleanup_orphaned_in_progress` (`src/strand/mend.rs`) only handles `status == InProgress`. Add a sibling function (or extend it) that releases the assignee on any `Open` bead whose assignee has no live heartbeat/registry entry, using the same staleness definition already applied to `in_progress` claims.
- **Before implementing:** re-verify `bf update <id> --status open --assignee ""` (or equivalent clear-assignee call) against the currently-deployed `bf`/`br` version — it was rejected as of bf 0.3.0 with no `bf release` subcommand available. This is an external dependency outside this repo; if still broken upstream, this sub-item blocks on that fix (or a documented workaround) rather than reaching for a `.beads/` direct-edit shortcut, which this repo's own conventions already prohibit.

### 10.4 Decouple reclamation from worker liveness
- `needle supervise`'s `tick()` (`src/supervisor/mod.rs`) currently only spawns a worker when `ready_beads` is non-empty — but stale claims are exactly what suppress the ready queue, so a fully idle fleet holding only stale claims can never trigger a spawn, which means Mend (only reachable from inside a worker's own loop) never runs, which means the stale claims never clear. Fix: `tick()` calls a mend-equivalent reclamation pass **unconditionally, every tick**, before its `ready_beads` check — not gated by it.
- Add the same reclamation (stale in-progress + stale-assignee-on-Open, per 10.3) to `needle doctor --repair` as a second, independent path, so a host not running `needle supervise` still has a standalone command for it (cron-friendly, no worker or supervisor required to be alive).
- Preserve the supervisor's existing tick interval/backoff — only the *order* of operations within one tick changes (reclaim before checking readiness), not how often ticks happen.

### 10.5 Testing
- Regression test: the isolated `dead_worker_cleanup_integration` (10.1) makes zero writes outside its tempdir fixture, verified by fixture-path assertion, not just "doesn't error."
- Regression test: a bead that fails 5 consecutive times (mocked agent, deterministic failure) ends up `status: blocked`, labeled `cycling`, and is no longer returned by Pluck's candidate query.
- Regression test: a mitosis `NotSplittable` verdict on a bead already at 4 prior failures results in quarantine on the 5th, not a 6th dispatch attempt.
- Regression test: an `Open` bead with a dead-heartbeat assignee gets its assignee cleared by the new Mend function and becomes claimable again.
- Regression test: `needle supervise`'s `tick()`, given zero ready beads and one stale in-progress claim, reclaims the claim within one tick without requiring any worker process to be running.

### 10.6 Deployment
- Version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, then staged fleet rollout through the canary channel (`:testing` → `:stable`), per the existing convention.

## Exit criteria
- No test in this repo's suite can write to a real, non-fixture `.beads/` directory under a developer or CI `$HOME`.
- A bead that fails K consecutive times stops being redispatched automatically — no manual split-and-block intervention required to halt the loop. **Met (ADR-012).**
- An `Open` bead with a stale assignee is reclaimed and becomes claimable again without any worker restart or manual `bf`/`br` intervention.
- A fully idle fleet (zero live workers, only stale claims) recovers to a claimable ready queue via `needle supervise` alone, with no worker needing to be manually launched first to "kick" reclamation.

# Phase 11: Deploy-Path Hardening — Hot-Reload Self-Healing and Spawn-Path Guardrails

**Status:** planned (ADR-007).

**Goal:** make NEEDLE's own binary-replacement paths recover from the two ways they've actually failed in production, rather than relying on an operator remembering the sanctioned procedure every time. Driven by two incidents: 2026-04-30 (`cp`-ing a new binary onto the live spawn path forced an unwanted hash-mismatch re-exec, disrupting 20 sessions) and 2026-07-20 (`mv`-replacing the spawn path instead left running workers hashing a deleted inode — `file_hash` errors every cycle, `check_hot_reload()` logs a warning and continues, and those workers can never hot-reload again). Both share a root cause: NEEDLE's sanctioned deploy path (upgrade → canary → promote → hot-reload) works, but nothing prevents writing directly to the spawn-path binary instead, and the hot-reload check has no recovery behavior for the specific failure a direct `mv` produces. Separately, `needle upgrade` itself still overwrites `env::current_exe()` directly with zero canary validation — the same unvalidated-overwrite shape, just invoked deliberately instead of by accident. Full evidence and rationale in [ADR-007](../adr/007-deploy-path-hardening.md).

## Changes

### 11.1 Self-heal the deleted-inode hot-reload failure
- When `get_current_binary_path()`'s resolved path indicates a deleted/unlinked exe (`/proc/self/exe` reading `"... (deleted)"`, or the equivalent `fs::read()` `NotFound` case), `check_hot_reload()` (`src/upgrade/mod.rs`) must treat this as an unconditional signal to re-exec against `needle-stable` immediately, rather than logging a warning and returning `Ok(())`. A worker running on a deleted inode has no legitimate reason to keep doing so — reload, don't stall.

### 11.2 Canary-gate `needle upgrade`
- `perform_upgrade()` (`src/upgrade/mod.rs`) should write the downloaded release to `~/.needle/bin/needle-testing` and go through the existing canary validation before promotion, instead of `fs::rename`-ing directly over `env::current_exe()`. This is ADR-005's explicitly deferred item ("filed as a separate, smaller bead" — no such bead exists), now in scope. `needle upgrade`'s UX is unchanged; it stops bypassing validation just because it was invoked manually. Share implementation with Phase 9 §9.1 if that phase lands first — same target path, same validation pipeline.

### 11.3 Visibility for unsanctioned spawn-path writes
- Add a check (in `needle doctor`, and/or each worker's BOOTING step) that detects "this process's own binary file has changed since the process started" (compare a hash or inode+mtime recorded at boot against the current state of the same path) and distinguish it from a legitimate hot-reload (which re-execs into a new process). Emit a `spawn_path.modified_in_place` telemetry event and a `needle doctor` warning naming affected workers. This cannot prevent a `cp` — it makes the resulting confusion visible instead of silent, which both historical incidents lacked.

### 11.4 Documentation at the point of use
- `needle upgrade --help` and `docs/plan/plan.md`'s Binary Structure section state plainly, at the call site (not only in an ADR an operator has to already know exists) that `cp`/`mv` onto `~/.local/bin/needle` or `~/.needle/bin/needle-stable` while any worker is running is unsupported, and name the two concrete failure modes (session disruption vs. permanent hot-reload stall) it produces.

### 11.5 Testing
- Regression test: a worker whose spawn-path binary is `mv`-replaced mid-run self-heals (re-execs into `:stable`) within one loop cycle, instead of logging `hot-reload check failed, continuing` indefinitely.
- Regression test: `needle upgrade` against a mocked bad release (fails canary) leaves the running binary and `:stable` untouched — mirrors Phase 9 §9.4's equivalent test for the supervisor-driven path.
- Regression test: `cp`-overwriting the spawn-path binary's content in place (no re-exec) is detected by the 11.3 check and produces a `spawn_path.modified_in_place` event.

### 11.6 Deployment
- Version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, staged canary rollout (`:testing` → `:stable`). 11.1 specifically should be validated with its own canary fixture (simulate a spawn-path `mv` mid-canary-run) before promotion, since it patches part of the hot-reload machinery it depends on.

## Exit criteria
- A worker whose spawn-path binary is replaced via `mv` while it's running recovers on its own within one loop cycle, with no permanent stall and no operator intervention.
- `needle upgrade` never installs an unvalidated release — a failing canary against a manually-triggered upgrade leaves the previous version running, same guarantee Phase 9 provides for the automatic path.
- An operator who runs `cp`/`mv` directly against a spawn-path binary sees an explicit warning (via `needle doctor` or telemetry) rather than silent, delayed, hard-to-diagnose session disruption.

# Phase 12: Fleet Resource Safety — Enforced CPU/RAM Gating on Worker Launch

**Status:** planned (ADR-008).

**Goal:** stop freshly-launched workers from being silently killed by CPU saturation during `worker_construction`, and stop batch launches from causing the saturation that kills them. Driven by a previously-diagnosed operational incident (2026-07-19, lab): an identical `needle run` invocation died twice at load ~2.5 with no panic or backtrace (stderr ending mid-`worker_construction`), then succeeded 90 minutes later at load ~0.74 — load was the only variable that changed. Plan.md already states the design intent ("NEEDLE monitors [CPU/RAM] and warns when saturated," `docs/plan/plan.md:115`); reviewing the implementation against that intent found the only resource check in the codebase (`check_system_resources`) is called exclusively from an already-running worker's dispatch loop, immediately before executing an already-claimed bead — never during `worker_construction` itself, and never during a `--count=N` batch launch, where the fixed (non-adaptive) launch stagger is the only thing standing between a batch launch and the saturation that kills its own later members. Full evidence and rationale in [ADR-008](../adr/008-fleet-resource-safety.md).

## Changes

### 12.1 Gate `worker_construction` on system resources
- Before entering `worker_construction` (`src/cli/mod.rs`), call `check_system_resources()` (generalized beyond its current rate-limit-specific naming, since it's now used at launch time too). If saturated, retry with bounded backoff rather than proceeding into a step known to be slow (~5s) and vulnerable to being killed mid-step; if still saturated after the max wait, fail the launch with an explicit, actionable error instead of letting it vanish silently.

### 12.2 Load-adaptive launch staggering
- Replace the fixed `launch_stagger_seconds` sleep in the `--count=N` sequential-launch path (`src/cli/mod.rs`) with a load-aware delay: use the existing short default when load is comfortable, extend (bounded, capped) when it isn't, so a batch launch doesn't blindly push itself past the saturation threshold its own later members will then be killed by.

### 12.3 Apply the same gate to `needle supervise`'s auto-scale path
- `needle supervise` (`src/supervisor/mod.rs`) is the other place new workers get spawned (queue-depth-driven auto-scaling) — it should not spawn into saturation any more than a manual `--count=N` invocation should. Reuse 12.1's gate rather than a separate implementation.

### 12.4 Testing
- Regression test: `worker_construction` launched under a simulated saturated-load condition defers/retries instead of proceeding, and eventually fails with a named reason if saturation doesn't clear within the bound.
- Regression test: a `--count=5` batch launch under simulated rising load produces increasing inter-launch delays, not a flat interval.
- Regression test: `needle supervise`'s auto-scale spawn path respects the same gate as the CLI launch path (shared implementation, not divergent behavior).

### 12.5 Deployment
- Version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, staged canary rollout (`:testing` → `:stable`).

## Exit criteria
- No worker launch is silently killed mid-`worker_construction` under CPU saturation — the outcome is either a successful (possibly delayed) launch or an explicit, logged failure with a reason.
- A `--count=N` batch launch on an already-loaded host does not itself push load high enough to kill its own later-launched members.
- `needle supervise`'s auto-scaler and the CLI's manual launch path share one resource-gating implementation, not two that can drift apart.

# Phase 13: External-Adopter Hardening — Gate Bead-Context, Gate Configurability, Deferred Bead Status, Spawn-Path Robustness

**Status:** implemented (ADR-009), fixes GitHub issues #7, #8, #9, #10, #11.

**Goal:** close five independent hardening gaps surfaced by the first external production adopter of NEEDLE-rs + bead-forge (a ~900-bead monorepo migrated from bd/Dolt, run alongside a legacy orchestrator during cutover). All five were filed as GitHub issues within the same 23-minute window on 2026-07-28, each citing head `74356cd`, each confirmed against source before any fix was written, and each already running as a validated local patch in the reporter's production fork. None requires a design change — each is a hardcoded constant or unhandled enum variant that assumed NEEDLE's own defaults would hold for every deployment. Full evidence and rationale in [ADR-009](../adr/009-external-adopter-hardening.md).

## Changes

### 13.1 Gate commands receive bead context (#7)
- `CommandGate::run_command` (`src/validation/mod.rs`) exports `NEEDLE_BEAD_ID` and `NEEDLE_WORKSPACE` into the spawned command's environment, sourced from the `bead: &Bead` parameter `CommandGate::validate` already receives but previously discarded (`_bead`). Enables bead-aware gates (resolve acceptance criteria, tag commits, write labels back) without racing other workers to guess bead identity from `br list --json` assignee state.

### 13.2 Configurable outcome-handler gate timeout (#8)
- New `ValidationConfig` section on `Config`: `validation.outcome_timeout_seconds`, default `50` (preserves current behavior). Threaded into `OutcomeHandler::handle_with_cancellation`'s `tokio::time::timeout` (`src/outcome/mod.rs`) in place of the hardcoded `Duration::from_secs(50)`. Unblocks gates running real verification workloads (container test suites, secret scanning, fresh-model diff verification) that need minutes, not seconds.

### 13.3 Configurable gate stderr cap (#9)
- Same `ValidationConfig` section: `validation.stderr_cap_bytes`, default `4096` (preserves current behavior). Threaded into `CommandGate` (constructed with the configured cap from `ValidationGate::new`/`from_commands`) in place of the `MAX_OUTPUT_BYTES` const in `src/validation/mod.rs`. A gate veto now carries as much diagnostic evidence as the operator configures, instead of always being cut to 4KB.

### 13.4 `BeadStatus` accepts bead-forge's `deferred` status (#10)
- New `Deferred` variant on `BeadStatus` (`src/types/mod.rs`), alongside the existing `Done`/`Closed` aliasing precedent. `is_done()` returns `false` for `Deferred` (distinct from `Blocked` — deliberately-postponed vs. blocked-by-dependency are different states, both now representable). Closes a silent-data-loss gap: a store with `deferred` beads previously failed deserialization for those records only, making them invisible to every strand and to `needle supervise`'s queue view with no surfaced error.

### 13.5 Supervisor spawns workers via its own binary path (#11)
- `Supervisor::spawn_worker` (`src/supervisor/mod.rs`) resolves the worker binary via `std::env::current_exe()` by default instead of `Command::new("needle")` (a bare `$PATH` lookup), since supervisor and worker are built from the same binary. New optional `worker.worker_binary_path` config override for deployments that deliberately want a different spawn target. The resolved path is logged once at supervisor startup so a name collision on `$PATH` (as in the reporter's migration, where a legacy tool occupied the name `needle`) is visible immediately rather than only via stalled worker heartbeats.

### 13.6 Testing
- `src/validation/mod.rs`: gate command sees both `NEEDLE_BEAD_ID` and `NEEDLE_WORKSPACE` in its environment; configurable stderr cap truncates at the configured value, not just the old default.
- `src/outcome/mod.rs`: config parse test for `validation.outcome_timeout_seconds`; behavioral test that a gate running longer than the default-but-shorter-than-configured timeout completes successfully.
- `src/types/mod.rs`: `"deferred"` deserializes to `BeadStatus::Deferred`; `is_done()` is `false` for it; round-trip serialize/deserialize.
- `src/supervisor/mod.rs`: spawn resolves to `current_exe()` when no override is configured; override path is honored when set.
- Targeted `cargo test --lib` runs across every touched module (`validation`, `outcome`, `types`, `supervisor`, `config`, plus `mitosis`/`dispatch`/`strand::pluck`/`bead_store` for the prerequisite compile fixes) — 405 tests, 0 failures, reproduced independently on iad-ci for everything it reached. A full-suite `needle-ci` "Succeeded" verdict was not obtained: 4 pre-existing, unrelated `strand::explore` tests hang the test binary (tracked separately as bf-2unnq, not touched by this phase).
- **13.6a follow-up (same session):** the first attempt at the outcome-timeout behavioral test used a slow `verification:` gate command and failed on iad-ci — `CommandGate` ran the command via blocking `std::process::Command` with no async yield point, so `tokio::time::timeout` could never actually preempt it (see ADR-009 addendum). Fixed as bf-3saat: `Gate::validate` is now `async` (`#[async_trait]`) and `CommandGate` uses `tokio::process::Command` with `kill_on_drop(true)`, so a slow gate command is now genuinely killed at the configured timeout, not just reported late. New tests: `command_gate_slow_command_is_killed_when_dropped_mid_flight` (src/validation/mod.rs), `handle_with_cancellation_kills_a_slow_verification_gate_command` (src/outcome/mod.rs, end-to-end).
- Full suite run via `needle-ci` on iad-ci (fmt + clippy + test), triggered by the push to `main` per this repo's standard CI convention (`CLAUDE.md`).

### 13.7 Deployment
- Direct commit to `main` (this repo's established convention — no PR/branch workflow), triggering `needle-ci` on iad-ci automatically. Version bump and GitHub Release follow the existing convention once CI is green.

## Exit criteria
- All five reproduction cases from issues #7–#11 pass with the fix applied and fail without it (regression-tested).
- No existing NEEDLE deployment's behavior changes by upgrading alone — every new config field defaults to today's hardcoded value.
- `needle-ci` (fmt + clippy + test) passes on `main` at the commit implementing this phase.
- Each of #7–#11 is closed on GitHub with a comment showing the fixing commit and the passing CI run.

# Phase 14: Supervisor Zombie-Child Reaping

**Status:** planned (ADR-010), fixes GitHub issue #12.

**Goal:** stop `needle supervise` from leaking `<defunct>` zombie processes for every worker it spawns, and stop the supervisor's own capacity accounting from being fooled by them. Reported by the same production adopter as #7–#11 (ADR-009), from a cutover soak test on pin `v0.2.12`/`fad0b50`: 22 zombies observed under `needle-supervise` after ~15 minutes of operation. Confirmed against source before any fix was written. Full evidence and rationale in [ADR-010](../adr/010-supervisor-zombie-reaping.md).

## Changes

### 14.1 Reap exited workers once per supervisor tick (#12)
- `Supervisor::tick()` (`src/supervisor/mod.rs`) gains a reap sweep at the top of each tick: loop `libc::waitpid(-1, &mut status, libc::WNOHANG)` until it returns `0` (nothing left to reap right now) or `-1`/`ECHILD` (no children at all). Safe because `needle supervise` only ever directly spawns worker processes — gate commands, dispatch subprocesses, etc. run inside the *worker* process, a separate PID tree, so this cannot race with the `.wait()` calls already present in `dispatch`/`telemetry`/`canary` for their own, different child processes.
- No change to `spawn_worker`'s detach model (`setsid` + `process_group(0)`, null stdio) — the supervisor remains the parent for `wait()` purposes; this phase adds the missing reap, not a re-architecture to double-fork/reparent-to-init.

### 14.2 `is_pid_alive` treats zombies as dead, not alive (#12, compounding)
- `registry::is_pid_alive` (`src/registry/mod.rs`) additionally checks `/proc/<pid>/stat`'s process-state field on Linux and returns `false` for state `Z`, even though `kill(pid, 0)` still succeeds for a zombie. Falls back to today's `kill(pid, 0)`-only behavior on non-Linux platforms and whenever `/proc/<pid>/stat` can't be read. Fixes the compounding bug the reporter connected to the zombie leak: `Supervisor::tick()`'s `alive_count >= max_workers` capacity gate (`src/supervisor/mod.rs:335`) counts zombies as alive today, so unreaped exited workers could hold a fleet at (false) capacity even with real headroom. `strand::mend`'s liveness check inherits the fix for free, since it calls the same function.
- `cli::is_pid_alive` (`src/cli/mod.rs`, a separate duplicate used for `needle status`/`needle cleanup` display) is intentionally left unchanged — out of scope for this issue's reported impact (supervisor capacity + zombie accumulation, not status display).

### 14.3 Testing
- `src/supervisor/mod.rs`: spawn a real short-lived child (e.g. `true`), let it exit, assert it appears as a zombie (`/proc/<pid>/stat` state `Z`) before the sweep, then assert the sweep reaps it (`waitpid` no longer finds it / `/proc/<pid>` gone).
- `src/registry/mod.rs`: `is_pid_alive` returns `false` for a deliberately-created, deliberately-unreaped zombie PID on Linux; existing alive/dead-PID tests unaffected.
- Regression test reproducing the reporter's capacity-hang scenario: a zombie worker in the registry no longer counts toward `alive_count` in `Supervisor::tick()`.

### 14.4 Deployment
- Direct commit to `main` (this repo's established convention), triggering `needle-ci` on iad-ci automatically. No config or public API shape changes — purely internal process-management correctness.

## Exit criteria
- A worker spawned by `needle supervise` that exits is reaped (no zombie) within one `poll_interval_secs` tick, regression-tested.
- `is_pid_alive` returns `false` for a zombie PID on Linux, regression-tested; behavior on non-Linux platforms and for genuinely-alive/genuinely-dead PIDs is unchanged.
- `needle-ci` (fmt + clippy + test) passes on `main` at the commit implementing this phase.
- #12 is closed on GitHub with a comment showing the fixing commit and the passing CI run.

# Phase 15: Activity-Aware Agent Execution Timeouts

**Status:** planned.

**Goal:** stop terminating healthy agents merely because a bead takes longer than an adapter's fixed wall-clock timeout, while still terminating agents that have stopped making observable progress and bounding agents that emit output forever. This phase is driven by the 2026-08-06 fleet observation that GLM-4.7 workers timed out at the adapter's exact 600-second deadline while still emitting stream events and performing tool calls; the same absolute-timeout behavior also affected the Opus worker, so this is dispatcher policy rather than a model-specific workaround.

## Changes

### 15.1 Split the adapter timeout into idle and hard deadlines
- Extend the adapter schema with `idle_timeout_secs` and `hard_timeout_secs`. `idle_timeout_secs` is the maximum interval with no observable agent-process output; `hard_timeout_secs` is the non-resettable maximum wall-clock execution time. `0` disables the corresponding deadline.
- Preserve compatibility for existing adapters that specify only `timeout_secs`: it retains today's absolute wall-clock behavior until that adapter opts into the new fields. Reject ambiguous adapter configurations that combine legacy `timeout_secs` with either new field rather than silently choosing precedence.
- The effective timeout remains adapter-specific first and global/default second. `needle config`, adapter validation errors, and example adapters must expose the resolved policy clearly.

### 15.2 Reset the idle deadline on streaming output activity
- Treat every successfully-read byte from the agent process's stdout or stderr as activity and reset `idle_timeout_secs`. Detection must happen before output transforms and must not wait for a newline, valid JSON, or a transform result; partial JSONL records and stderr-only progress count.
- Refactor `dispatch::run_process` so process exit, output activity, idle expiry, hard expiry, and cancellation are observed concurrently. Continue draining stdout/stderr without back-pressure and preserve the existing process-group kill/reap behavior on either timeout.
- Output activity resets only the idle deadline. It never extends `hard_timeout_secs`, so an agent that prints heartbeats or loops noisily cannot run forever.

### 15.3 Distinguish timeout reasons in outcomes and telemetry
- Preserve exit code `124` for compatibility, but carry a structured timeout reason (`idle` or `hard`) through `ExecutionResult`, outcome handling, trace metadata, and telemetry.
- Emit the configured idle/hard limits, elapsed wall time, and time since last output with the timeout event. Operator-facing logs must say which deadline fired rather than the current undifferentiated `agent timed out` message.
- A timed-out bead continues through the existing release/failure-count/quarantine policy; this phase changes detection and evidence, not bead lifecycle rules.

### 15.4 Deterministic tests
- A fixture agent that emits stdout bytes periodically for longer than `idle_timeout_secs`, then exits before `hard_timeout_secs`, succeeds.
- A fixture agent that emits only stderr activity receives the same idle-deadline resets and succeeds.
- A fixture agent that emits partial lines without newlines still resets the idle deadline.
- A silent fixture is killed at the idle deadline with reason `idle`, exit code `124`, and its full process group reaped.
- An endlessly chatty fixture is killed at the hard deadline with reason `hard`, proving activity cannot defeat the cap.
- A legacy adapter containing only `timeout_secs` retains the current absolute-timeout behavior, and a mixed legacy/new configuration fails validation with an actionable message.

### 15.5 Deployment
- Update the GLM-4.7 adapter to opt into activity-aware execution only after the deterministic dispatcher tests pass. Choose initial limits from observed fleet traces (expected starting point: a short inactivity bound and a 30–60 minute hard cap), then compare completion, idle-timeout, hard-timeout, and bead-orphan rates against the 2026-08-06 baseline before applying the policy to other adapters.
- Version bump, `needle-ci` (fmt + Clippy + test on iad-ci), and staged canary rollout through `needle-testing` to `needle-stable`.

## Exit criteria
- A streaming agent can run beyond its former fixed ten-minute adapter timeout as long as it continues producing stdout/stderr activity and remains below the hard cap.
- A silent agent is terminated within the configured idle bound, and a continuously chatty agent is terminated within the configured hard bound.
- Timeout telemetry identifies the firing deadline and contains enough timing evidence to distinguish provider stalls from long-running productive work.
- Existing adapters retain their current timeout behavior until explicitly migrated.

# Phase 16: Configurable Bead-CLI Backends — Descriptors, Not Hardcoded Harnesses

**Status:** accepted, not yet implemented. `docs/adr/013-pluggable-bead-cli-backends.md` was **accepted 2026-08-12** and carries the descriptor decision, dialect matrix, backend priority, and rejected alternatives. `docs/adr/014-explicit-workspace-bead-backend-binding.md` was **accepted 2026-08-12** and supersedes ADR-013's ordered `auto` rule for worker selection: repository configuration, not executable availability, owns backend choice. Phase 16 is authorized work.

**Goal:** make NEEDLE interoperable with **bead-rs (primary)**, **bead-forge (secondary)**, **beads_rust (tertiary)**, *and other bead systems that exist in the world* — the last being a requirement, not a side effect. That is achieved by making the bead-CLI layer configurable the way the agent layer already is: a bead backend becomes a **descriptor** — a serde struct loaded from YAML — not a Rust impl. `builtin_bead_backends()` ships **bead-rs** (`bead` v0.1.3), **bead-forge** (`bf` v0.4.1), and **beads_rust** (`br` v0.1.28, dicklesworthstone) as data; user files in `~/.config/needle/bead-backends/` override by name. A fourth CLI — the Go `bd`, a fork, something not yet written — is a YAML file, not a release.

The priority ordering sets descriptor authoring order, not workspace ownership or tiers of support: the primary backend's descriptor is written first, but every repository binds explicitly to a descriptor in `.needle.yaml`. Installing a higher-priority binary never changes an existing repository. All three remain first-class. See ADR-013 §7 for capability gaps and ADR-014 for selection semantics.

This mirrors `AgentAdapter` + `load_adapters` (`src/dispatch/mod.rs:570-660`) exactly — the pattern NEEDLE already uses to add agent harnesses without recompiling.

Triggered by `game-of-life` in the agent-sandbox cluster (bead-rs-backed, undrivable by stock NEEDLE), but the investigation found the existing two-backend handling is already wrong: `BrCliBeadStore` speaks correct beads_rust for create/dep/sync/list, yet `discover()` resolves `bf` first, binding it to the wrong binary — and bf-only `batch`/`claim` calls were grafted into the same store. The fleet runs that chimera. A descriptor design makes it unconstructible: argv and binary come from one source, and identity is verified against it.

The `BeadStore` trait (`src/bead_store/mod.rs:546`) remains the seam and does not change shape. Below it, the two impls are replaced by one descriptor-driven engine.

## Changes

### 16.1 `BeadBackend` descriptor type and loader
- Serde struct: `name`, `binary`, `detect_paths`, `identity_pattern`, `version_command`, per-operation specs, declared `capabilities`.
- `builtin_bead_backends()` + `load_bead_backends()` merging `~/.config/needle/bead-backends/*.yaml`, user overriding built-ins by name — same contract as `load_adapters`.
- **Validation at load time, not first claim:** reject unknown strategies, unresolvable `{placeholders}`, and missing required operations. A malformed descriptor is now how a fleet breaks; it must fail loudly at startup.

### 16.2 Operation strategies
Implement the closed strategy set once, selected per-operation by descriptor:
- `claim`: `compare_and_set` | `batch_op`
- `claim_auto`: `atomic_subcommand` | `non_atomic_scan`
- `split`: `transactional_batch` | `sequential`
- create→ID parse: `bare_id` | `json_field`
- labels: `csv` | `repeated`
- import: `bare` | `input_plus_mode`

Six enums cover every divergence across three upstreams plus their Go ancestor. A backend needing a genuinely new behavior adds one variant — available to all backends — not a `BeadStore` impl.

### 16.3 `CliBeadStore` engine
- One `BeadStore` impl driven by a `BeadBackend`. Renders argv from templates, dispatches on strategy, parses per the declared shape.
- Delete `BrCliBeadStore` and `BfCliBeadStore`; their behavior survives as builtin descriptors. Their test suites must be re-expressed as descriptor conformance tests or coverage silently drops on claim/release.

### 16.4 Builtin descriptors, primary first
Authored in priority order, so the backend furthest from NEEDLE's baked-in `bf` assumptions surfaces missing strategy variants earliest.

- **`bead-rs` (primary)**: `--description` + repeated `--label` (no `--json` on create), `dep add <blocked> <blocker> --kind blocks`, `update --assignee`/`--clear-assignee`, `import: input_plus_mode`, `claim_auto: atomic_subcommand`, `split: sequential`. The builtin descriptor and real-binary gates are pinned to annotated release `v0.1.3` at commit `85f36ac`; conformance must be re-run when that pin moves.
- **`bead-forge` (secondary)**: `--description` + repeated `--label`, `dep add <blocker> --blocks <blocked>`, `claim: batch_op`, `claim_auto: atomic_subcommand` with velocity metadata, `split: transactional_batch`.
- **`beads_rust` (tertiary)**: `--body`/`--silent`/`-l --labels` (csv), `dep add <blocked> <blocker> -t blocks`, bare `sync --import-only`, `ready --json --limit`; `claim: compare_and_set`, `claim_auto: non_atomic_scan` (no `claim` subcommand exists), `split: sequential`.
- Each argv pinned to the installed binary's own `--help`, not inferred.
- **Open world:** a backend with no shipped descriptor must still be drivable from user YAML alone. That makes the six strategy enums a published extension point rather than an internal detail, and capability negotiation (16.x) the discovery path when nobody has written the descriptor yet — all three current backends expose a capabilities-style surface, and bead-rs's is a full JSON contract (`contract: native-v1`, `atomic_claim`, statuses, checkpoint formats, schema refs).

### 16.5 Identity verification and one resolver
- `resolve_bead_cli()` returns a descriptor plus a verified path, replacing all five hardcoded chains (`bead_store/mod.rs:758-775`, `:1127-1136`, `:1893-1902`; `worker/mod.rs:732-742`; `cli/mod.rs:3626-3632`).
- Match the resolved binary's `--version` against `identity_pattern`. `~/.local/bin/br` is a shim that `exec`s `bf` and reports `bf <version>`, so it fails `beads_rust`'s `^br ` check instead of silently supplying the wrong dialect. Mismatch fails loudly, naming path and identity found.
- `bead_cli.backend` names a descriptor explicitly, plus an optional explicit `path`; it is workspace-scoped in `.needle.yaml`.
- Production workers fail closed when the binding is missing, unknown, or identity-mismatched. `auto` may propose a binding in doctor/onboarding output but is never authority to open a store.
- Every construction path—including Explore targets—uses the target workspace's binding rather than a host-wide preference or the home workspace's backend.

### 16.6 Capability declaration and reconciliation
- Descriptors declare capabilities. Where an upstream exposes a contract surface (`bead capabilities --profile`, `bf schema`/`robot-docs`, `br schema`), probe at discovery and reconcile against the declaration, warning on mismatch — so drift is visible rather than silently wrong.
- Make the bead-forge version handshake (`bead_store/mod.rs:295-419`) descriptor-conditional, and revisit the unconditional `--limit 999999` workaround at `:1357`/`:2094`.

### 16.7 Route `predispatch` through the store
- `validation/predispatch.rs:128` shells out to a literal `"bf"`, bypassing the trait. Replace with `BeadStore::show` — the last non-trait bead-CLI call site.

### 16.8 Descriptor-derived prompt fragments
- Render the bead-command block (`prompt/mod.rs:294-325`) and the references at `:56`/`:64` from the active descriptor, so the agent is told to run the installed binary and the dialect exists in exactly one place. Backend-parameterize the literal-string assertion at `:1066-1067`.

### 16.9 `needle bead-backend <name>` and `needle doctor`
- Mirror `needle test-agent`: resolve, verify identity, probe capabilities, print the rendered argv for every operation. A descriptor is testable before a worker ever dispatches.
- `needle doctor` reports the resolved backend, path, and capability gaps, replacing the `"checked bf, br"` message that is wrong on two of three.

### 16.10 Conformance tests
- One descriptor conformance suite run against all three builtins using the existing fixture-CLI pattern (`bead_store/mod.rs:2809`, `:2858`, `:2960`): argv assertions per operation, per backend.
- Round-trip test proving `dep add` lands edges in the intended direction on each backend — `br`/`bead` take two positionals `(blocked, blocker)`, `bf` takes one plus `--blocks`. A wrong guess inverts every edge silently rather than failing.
- Descriptor validation tests: unknown strategy, bad placeholder, missing operation, identity mismatch.
- Honor the test isolation policy (CLAUDE.md): pin `HOME` and `strands.explore.workspace_root` to a tempdir.

### 16.11 Document per-backend capability gaps
- Two gaps change fleet **safety**: atomic mitosis is bf-only, and atomic server-side claim is bf/bead-only — beads_rust `claim_auto` carries a real TOCTOU window in which two workers can claim the same bead, the duplicate-claim hazard CLAUDE.md already names as the real fleet failure mode.
- Capability matrix in `docs/configuration.md`, plus a "writing a bead backend descriptor" section so a fourth CLI can be added without reading NEEDLE source.

### 16.12 Explicit workspace binding and transition audit
- Add `bead_cli.backend: <descriptor-name>` to repository `.needle.yaml`; existing bead-forge repositories bind to `bead-forge`, while verified native bead-rs repositories bind to `bead-rs`.
- Retain `jedarden/bead-forge` on bead-forge as a permanent explicit exception; migration inventory treats that binding as the desired terminal state.
- Add a read-only audit/onboarding command that reports unbound workspaces, resolved descriptor/path/version, identity result, and a proposed binding without writing it.
- Add an explicit bind command that updates only the selected repository configuration after operator invocation; it must state that binding is routing, not data migration.
- Remove independent PATH-probe chains from worker, supervisor, strands, validation, prompts, recovery, and doctor. All receive one resolved backend context.
- Gate rollout on mixed-backend tests with all three binaries installed, plus fail-closed tests for missing/unknown/mismatched bindings.

## Exit criteria
- Adding a bead CLI that fits the existing strategies requires **no Rust change** — a YAML descriptor and `needle bead-backend <name>` to verify it.
- A worker claims, dispatches, closes, and releases end to end on all three builtin backends.
- With all backend binaries installed, each workspace invokes only the descriptor explicitly bound in its own `.needle.yaml`; PATH ordering cannot change the result.
- An unbound or identity-mismatched workspace is ineligible for dispatch and no bead CLI is spawned against its store.
- A bead-forge workspace with an explicit `bead-forge` binding behaves identically to before this phase; the transition audit identifies every unbound legacy workspace before enforcement is enabled.
- No store can bind to a binary speaking a different dialect: identity is verified against the descriptor that supplies the argv.
- `grep -rn '"bf"\|"br"' src/` returns nothing outside descriptor definitions and their tests.
- `needle doctor` names the resolved backend, its path, and its capability gaps.

# Phase 17: OTLP Resource Propagation and Roaming-Worker Identity

**Status:** proposed (ADR-016). Supersedes the incomplete fix closed under `needle-501aa991` / commit `519468a`.

**Goal:** make the fleet dashboard's worker card describe the worker that actually exists — which repository it is working in *right now*, which harness and model it dispatched with, and which NEEDLE version it is running — instead of showing `Unknown`. Driven by a 2026-08-16 investigation: four of the five facts on every worker card render `Unknown`, and `Repo`, `semver`, and `worker_pool` render `Unknown` for **every** worker in the fleet, always. Full evidence and rationale in [ADR-016](../adr/016-otlp-resource-propagation-and-roaming-worker-identity.md).

Three independent defects stack:

1. **The OTel Resource never reaches the wire.** `src/telemetry/otlp.rs` interposes four resilience wrappers (`ResilientHttp/GrpcLogExporter`, `ResilientHttp/GrpcSpanExporter`) that implement `export()` and nothing else. `LogExporter::set_resource` and `SpanExporter::set_resource` are **defaulted no-ops** on the SDK traits, so each wrapper silently absorbs the Resource that `SdkLoggerProvider::build()` pushes down and never forwards it to the inner `opentelemetry_otlp` exporter. A raw capture of the deployed 0.3.1 binary's `/v1/logs` payload contains **zero** resource attributes — no `service.name`, no `service.version`, no `deployment.cluster`, no `needle.worker.pool`, none of the `needle.*` trio. `OtlpSink::build_resource()` is correct; its output is discarded one hop before the socket. Metrics are unaffected (`MetricExporter` is not wrapped), which is why `deployment.cluster` — injected by the *collector*, not by NEEDLE — is the one dashboard field that works.

2. **`TelemetryEvent.workspace` is a dead field.** It is declared (`telemetry/mod.rs:71`), documented in the telemetry module spec above, filterable (`mod.rs:4023`), read by `OtlpSink::emit_log` — and hardcoded to `None` at both emit sites (`mod.rs:3323`, `mod.rs:3488`). The dashboard's *first* choice for `repo` is exactly this attribute.

3. **Identity is resolved once at boot, from config, onto an immutable Resource.** `worker_telemetry_identity()` (`cli/mod.rs:849`) reads `config.agent.default` and `config.workspace.default`. But `needle run -w` states in its own help text that the home workspace is "NOT an exclusive scope" — the Explore strand roams, and model selection is per-dispatch whenever `agent.routing` is set. A Resource attribute cannot track either. Fixing (1) alone would make `Repo` confidently wrong rather than blank.

## Changes

### 17.1 Forward `set_resource` through every exporter wrapper
- Implement `set_resource` on `ResilientHttpLogExporter`, `ResilientGrpcLogExporter`, `ResilientHttpSpanExporter`, and `ResilientGrpcSpanExporter`, delegating to the inner exporter via `Arc::get_mut` (unique at provider-build time, which is when the SDK calls it). WARN if the unique reference cannot be obtained — a silently resource-less exporter is the bug being fixed.
- **Normative rule:** a wrapper interposed on an OTel SDK exporter trait must implement *every* method of that trait, including defaulted no-ops. `export()` alone is never a complete implementation.

### 17.2 Split identity between Resource and record
- Resource keeps what is fixed for the process or host: `service.*`, `host.name`, `process.pid`, `needle.session_id`, and every operator-supplied `telemetry.otlp.resource_attributes` entry (`deployment.cluster`, `needle.worker.pool` — currently discarded entirely).
- The log record carries what changes: `workspace` (repo basename), plus `needle.agent` / `needle.model` as dispatched.
- `needle.agent` / `needle.model` appear at both layers: Resource documents the configured default, the record wins. State this in the Semantic Mapping table so consumers do not guess.

### 17.3 Populate `TelemetryEvent.workspace` at emit time
- `Telemetry` gains a current-workspace cell updated when a claim binds the worker to a workspace; `emit`/`emit_sync` read it instead of writing `None`.
- `Bead.workspace` (`claim/mod.rs:609`) is the authoritative source and is already known before dispatch — no new cross-module plumbing.
- Basename reduction stays in `workspace_label`; full filesystem paths must never reach the browser contract.

### 17.4 Test at the transport seam, not the builder
- Every telemetry-attribute test runs through the real `build_http_providers` / `build_grpc_providers` path with a capturing exporter substituted at the transport seam, asserting on the Resource and attributes the *exporter* is handed.
- Regression test specifically covering the wrapper hop: build providers via the real path, assert the wrapper forwarded the Resource.
- A test asserting on `build_resource()`'s return value is insufficient by construction — that is how `needle-501aa991` was closed while the dashboard stayed blank.

### 17.5 Check in the wire-capture harness
- Script the isolated capture used to diagnose this: throwaway `HOME` + `XDG_CONFIG_HOME`, a single-workspace `strands.explore.workspaces` pin, `compression: none`, a local OTLP receiver that dumps the raw payload. It is safe against the live fleet and is the only check that observes the actual defect class.

### 17.6 Live verification
- Re-fetch `/api/dashboard` and confirm no `null` in `repo`, `harness`, `model`, `semver`, or `worker_pool` for a freshly restarted worker; confirm `repo` follows a roaming worker across repositories.
- Coordinate with the existing open beads for the OTLP startup panic and end-to-end verification rather than duplicating them.

## Exit criteria
- A raw OTLP payload captured from a real `needle` binary contains `service.name`, `service.version`, `service.instance.id`, `deployment.cluster`, `needle.worker.pool`, and the `needle.*` identity attributes.
- Spans exported to Tempo carry service identity; today every NEEDLE span arrives with an empty Resource.
- The dashboard worker card shows no `Unknown` for Environment, Harness, Model, Repo, or the semver badge on a freshly restarted worker.
- `Repo` changes when a roaming worker claims a bead in a different repository, and never shows a full filesystem path.
- Operator-supplied `telemetry.otlp.resource_attributes` entries reach the collector — currently none of them do.
- A test fails if any resilient exporter wrapper stops forwarding the Resource.

# Phase 18: Configuration Hot-Reload at the Cycle Boundary

**Status:** proposed (ADR-017).

**Goal:** make a configuration change take effect on a running worker without a restart, at the boundary where it is already safe to change things. Driven by the 2026-08-21 fleet-wide OTLP migration: flipping one boolean (`telemetry.otlp_sink.enabled`) required draining and relaunching all fifteen ex44 workers — ~55 minutes of wall clock, bounded by whatever each worker's in-flight dispatch happened to be doing, with every drained worker releasing its claimed bead. Full evidence and rationale in [ADR-017](../adr/017-configuration-hot-reload-at-the-cycle-boundary.md).

The runtime is built to make this impossible today, in three distinct ways:

1. **Config is a boot-time snapshot.** `Config`'s own doc comment says "Loaded once at boot, immutable during a session." `Worker::new` builds `Telemetry`, `StrandRunner`, `PromptBuilder`, `Dispatcher`, `OutcomeHandler`, `HealthMonitor`, and `RateLimiter` from that snapshot and holds them for the process lifetime.
2. **The tracing subscriber is a one-shot global.** `init_tracing_subscriber` installs the OTLP layer via `tracing_subscriber::registry()....try_init()`. A process that booted without a reload handle can never acquire one.
3. **Unapplicable config is discarded silently.** `telemetry` is in `NON_OVERRIDABLE_KEYS`, so a workspace `.needle.yaml` enabling OTLP is parsed, warned once, and dropped. The fleet ran for days on a config file that read as correct.

The safe seam already exists: `check_hot_reload()` runs after `LOGGING`, "between dispatch cycles, never mid-claim, ensuring no bead is left in_progress." Config reload is the same problem with a smaller blast radius.

**That seam's history is itself a requirement.** `check_hot_reload` was `#[allow(dead_code)]` with no call site anywhere from 2026-03-21 until ~2026-08-16 (`needle-eea03800`) — five months during which its doc comment claimed it ran between LOGGING and SELECTING, and every "hot-reload" deploy was really a manual kill-and-relaunch. It is live now (21 `binary.freshness.exit` events as of 2026-08-21), but that is days of evidence, not months. A reload mechanism that is *documented* to run is not one that runs: Phase 18 must assert the config check actually executes, and fail if the call site is ever dropped.

## Decisions locked

An implementer will hit each of these forks; the plan decides them so no mid-build ADR is needed.

- **Trigger = polled mtime+hash at the cycle boundary**, gated by `worker.config_reload_check_interval_secs` (`0` = disabled, and is the default until the feature has fleet time). *Because* it mirrors `check_hot_reload`'s existing binary-hash check and adds no dependency. *Rejected:* a `notify` file watcher (new dep, new async task, and it must defer to the boundary anyway). *Enforced by* 18.2.
- **SIGHUP must NOT be the trigger.** *Because* `install_unix_signal_handlers` already registers `SIGTERM`/`SIGINT`/`SIGHUP` onto the shutdown flag, deliberately — a killed tmux session delivers SIGHUP, and the handler exists so the worker can release its bead and emit `worker.stopped` rather than die silently. Repurposing it turns every tmux teardown into a reload. *Revisit if* the shutdown handler ever stops covering SIGHUP.
- **Reload applies only between dispatch cycles.** *Because* a worker in `Building`/`Dispatching`/`Executing`/`Handling` holds a bead, and changing `agent.timeout` or an adapter under a running dispatch produces an outcome attributable to neither config. *Enforced by* 18.2 + the test in 18.8.
- **Three declared tiers, written as a table in code, not inferred.** *Because* an untiered atomic swap reads as though everything is reloadable and would silently no-op for immutable keys — recreating the `NON_OVERRIDABLE_KEYS` failure that caused this work. *Enforced by* 18.1.
- **Validate before swap; a reload may never fail closed.** *Because* polled reload reaches every worker within one interval, so a bad edit is fleet-wide and simultaneous — strictly worse than the restart-only status quo unless validation is a hard gate. A config problem must degrade telemetry, never remove a worker. *Enforced by* 18.3.
- **The `reload::Layer` seam is installed unconditionally at boot, including when OTLP is off.** *Because* `try_init()` is one-shot; installing the seam only when OTLP is already enabled means turning OTLP *on* still needs a restart — the exact situation this phase exists to remove, undiscoverable until tried on a live fleet. *Enforced by* 18.5.
- **A reload never tears down a working exporter.** *Because* a reload cannot introduce a new environment variable into a running process, so a rebuild needing an absent `env:` header must keep the old exporter and report. *Enforced by* 18.5.

## Changes

### 18.1 Declare a reload tier for every config key
- A table in `src/config/` mapping each key path to **Tier A (live)**, **Tier B (rebuild)**, or **Tier C (restart-required)**.
- Tier A: `worker.idle_*`, `worker.max_claim_retries`, `agent.timeout`, `budget.*`, strand thresholds — read live off `self.config`, effective next cycle with no rebuild.
- Tier B: `telemetry.*`, `strands.*`, `prompt.*`, `agent.adapters_dir`, `limits.*`, gates/verification — owned by a component built in `Worker::new`.
- Tier C: worker identity/`qualified_id`, `workspace.home`, `bead_cli.backend`, tokio runtime, tracing-stack shape.
- **Normative rule:** a new config key must be assigned a tier in the same change that introduces it. An unclassified key is a compile-time failure, not a runtime surprise.

### 18.2 Cycle-boundary reload check
- `check_config_reload()` alongside `check_hot_reload()` after `do_log()`, gated by `worker.config_reload_check_interval_secs`.
- Detect via mtime plus content hash of the resolved global config path (mtime alone is not sufficient — an editor that rewrites in place with a preserved mtime would be missed).
- Per-section hashing so 18.4 can rebuild only the components whose own subtree changed.

### 18.3 Validate-before-swap, and never fail closed
- Run `ConfigLoader::validate` against the candidate before any swap.
- On error: keep the running config, emit `config.reload.rejected` with the errors, WARN, continue. Never propagate a reload error into the worker's `Result` path — that is how a config problem becomes a dead worker.
- The swap itself is all-or-nothing: no half-applied configuration is ever observable.

### 18.4 Rebuild Tier-B components
- Reconstruct only components whose config subtree changed: `StrandRunner`, `PromptBuilder`, `Dispatcher` (adapter reload), `RateLimiter`, `OutcomeHandler`.
- Each rebuild is fallible and isolated: a component that fails to rebuild keeps its previous instance and reports, rather than leaving the worker with a missing component.

### 18.5 Reload-safe telemetry
- Wrap the OTLP tracing layer in `tracing_subscriber::reload::Layer` at boot **unconditionally**, wrapping a no-op layer when OTLP is disabled. `reload` is not feature-gated in the pinned `tracing-subscriber` 0.3.23 — no new dependency.
- Make the telemetry writer thread's sink set swappable. The thread currently owns `Vec<Box<dyn Sink>>` for its lifetime (`PendingWriter`); add a control message on the existing channel so sinks can be replaced without tearing down the writer or losing queued events.
- Re-resolve `env:`-prefixed header values from the process environment on rebuild. If a required header is absent, keep the existing exporter and emit `config.reload.restart_required` — never replace a working exporter with a dead one. Header values never reach a log, event, or error message.

### 18.6 Report what cannot be applied
- A Tier-C key change emits `config.reload.restart_required` naming the keys, plus a WARN.
- The same treatment for a workspace `.needle.yaml` carrying a `NON_OVERRIDABLE_KEYS` section — currently a single WARN into a log nobody reads.

### 18.7 Observability
- Events: `config.reload.detected`, `config.reload.applied` (with the changed key paths), `config.reload.rejected`, `config.reload.restart_required`.
- `needle config --dump --show-source` must reflect the **live** config of a running worker, including a reload generation counter, so an operator can confirm what a worker is actually running rather than what the file says.

### 18.8 Tests
- A reload requested mid-dispatch is not applied until the cycle boundary; a dispatch launched under the old config completes under it.
- An invalid candidate config leaves the worker running on the previous config and emits `config.reload.rejected` — the worker must still be alive at the end of the test.
- OTLP toggles false→true and true→false on a running worker, asserted at the transport seam (per the ADR-016 rule: assert on what the exporter is handed, not on a builder's return value).
- A Tier-C key change reports and does not silently no-op.
- A rebuild whose `env:` header is absent keeps the previous exporter.
- **The reload check actually executes.** A test that fails if `check_config_reload()` loses its call site or is never reached from the state machine — the `needle-eea03800` failure mode, where a documented-as-running mechanism was dead code for five months.

## Exit criteria
- `telemetry.otlp_sink.enabled` can be flipped on a running worker and traces appear at the collector without the process restarting.
- No reload is ever applied while a bead is claimed; no bead is released by a reload.
- An invalid config edit leaves every worker running, with `config.reload.rejected` emitted — verified by editing the live fleet config to something invalid and observing zero worker exits.
- Changing a Tier-C key produces a named `config.reload.restart_required` rather than silence.
- `needle config --dump --show-source` against a running worker reflects a post-reload value, not the boot-time snapshot.
- The 2026-08-21 migration is reproducible as a config edit: enabling OTLP fleet-wide costs one interval, not a fifteen-worker drain.


# Phase 19: Autonomous Backlog Management — Never Stuck Without a Human

**Status:** planned (ADR-022 quarantine ladder, ADR-023 gate-error classification). Principles 7 and 8.
**Goal:** the fleet keeps a 3,000-bead backlog moving on its own. Measured 2026-08-29 on ex44 (20 workers): 3,181 open beads, **106** ready; 2,926 dependency-blocked of which **2,129 hang off 91 non-ready roots** (quarantined or `human`); 2,075 (65%) are fleet-generated `split-child` beads; 743 are agent-filed "Verify/Test/Confirm …" beads; creates/day ≈ closes/day (1,424 vs 1,401) — a treadmill. 171 open beads sat invisibly quarantined (`manual_blocked`, set by `bead update --status blocked`), and 2,346 of 2,470 verification failures that week were the gate failing to *execute* (empty `bead.workspace`, fixed in `8818f9e9`; a `verification:` hook that never existed on disk), each counted as a bead failure. Nobody was needed to fix any of that; NEEDLE simply had no automatic next step. This phase gives every stuck state one.

## The escalation ladder (the contract every other section serves)

Every bead that cannot make progress moves down exactly this ladder, never skipping a rung, with evidence recorded on the bead at each step:

| Rung | Trigger | Automatic action | Evidence recorded |
|---|---|---|---|
| 1 Retry | transient (Tier 1) error | redispatch, `retry_count` | telemetry only |
| 2 Decompose | `split_after_failures` (3) | Mitosis, ≤ `mitosis.max_children` (8), depth ≤ `mitosis.max_depth` (2) | children + `split-child` label |
| 3 Quarantine | `quarantine_after_failures` (5) | labels `quarantined`, `quarantine-until:<rfc3339>`, `quarantine-round:N`; backoff 2h·2^(N−1), cap 48h; Pluck skips until expiry | note: failure reasons, last trace path |
| 4 Re-analyze | quarantine round 3 expires | one *analysis dispatch*: agent re-reads `docs/plan/plan.md` + the bead's failure trail and must produce either a re-scoped child bead or a `human` label with a stated reason the plan cannot answer | note: `analysis:` block |
| 5 Human | rung 4 explicitly concludes the plan is silent | label `human`; `needle status` counts it | the rung-4 note |

Gate *execution* errors (19.1) are not failures and do not advance a bead down the ladder. Nothing in NEEDLE may ever set `--status blocked` again (19.2).

## Changes

### 19.1 Gate execution errors are infrastructure, not bead failures (ADR-023)
- New outcome class `GateError` in `src/outcome/mod.rs`, distinct from `Failure`: the gate command could not be spawned (ENOENT, permission), the gate's working directory is missing or empty, the command named in config does not exist in the workspace, or the gate itself timed out before producing a verdict. A gate that *ran and failed* is still `Failure`.
- `GateError` → release the bead, leave `failure-count` untouched, emit `gate.execution_error{workspace, gate, command, reason}`.
- Per-workspace gate health: after 3 consecutive `GateError`s the workspace is `gate-degraded` (state file `~/.needle/state/gate-health/<workspace-id>.json`). Pluck and Explore skip a degraded workspace for ordinary dispatch and instead surface exactly one fingerprinted bead there — `Gate broken: <command> — <reason>` (P0, labels `infra`, `fingerprint:<hash>`) — which *is* claimable, because fixing a gate is verified by running the gate. The first successful gate run clears the degradation and closes that bead. *Because* dispatching without a working gate would return NEEDLE to acceptance-by-self-report (ADR-020), and cycling every bead through a broken gate is how 138 beads were buried.
- A `verification:`/`gates:` entry naming a path that does not exist at worker boot is reported by `needle doctor` (FAIL, with the path) and by the worker at startup; it does not wait for the first dispatch to discover it.

### 19.2 Quarantine is visible, time-bounded, and escalates (ADR-022, supersedes ADR-012's mechanism)
- `BeadAction::Quarantined` stops calling `store.block()`. It adds labels `quarantined`, `quarantine-until:<rfc3339>`, `quarantine-round:N` and appends the failure trail to the bead's notes. The `cycling` label is retained for continuity. The backend descriptor's `block` operation is removed.
- Pluck excludes a bead while `quarantine-until` is in the future; when it has passed, the bead is a normal candidate again with its `failure-count` intact (the `reset_failure_count` ordering bug, needle-b39fe1b6, is a prerequisite). Round N+1 is entered on the next failure. After round 3 the bead goes to rung 4, not round 4.
- Backoff: 2h, 4h, 8h — cap 48h — chosen so a transient cause (red tree, provider outage) clears itself inside a working day without a human noticing, while a persistent cause reaches rung 4 within ~14h of wall clock.
- Migration: Mend's first pass under the new binary converts every open bead whose store reports `manual_blocked=true` (bead-rs must expose this — see the bead-rs dependency) to the label scheme with `quarantine-round:1` and an immediate expiry, then clears the flag with `bead update --status open`. Migration-era beads whose old labels live only in notes get `quarantined` + `quarantine-round:1` and nothing else.
- `needle status` and `needle doctor` gain: quarantined count per workspace (by round), beads at the human rung, **blocked-tree size** (open beads transitively behind each non-ready root) and the top five roots by that size. *Because* a root pinning 158 dependents must be the most visible bead in the workspace, and on 2026-08-29 it was invisible to every tool but `bead why`.

### 19.3 Root-aware, aging-aware Pluck ordering
- Sort key becomes `(effective_priority, pinned_bucket, failure_count, created_at, id)` — still fully deterministic (Principle 1). `effective_priority = min(own priority, min priority over all open beads transitively blocked by this bead)`; `pinned_bucket` is `-floor(log2(1 + open beads transitively blocked))` so a root that unblocks 100 beads sorts ahead of a leaf of equal priority.
- Aging: a bead open more than 14 days is treated one priority level higher, more than 30 days two levels, computed from `created_at` at query time — no writes, no drift between workers.
- Graph walk is memoized per cycle from `list_all`; workspaces above 5,000 open beads fall back to own-priority ordering with a `pluck.ordering_degraded` event rather than a slow cycle.

### 19.4 Generation budget and anti-noise
- Mitosis caps: `mitosis.max_children` (default 8) and `mitosis.max_depth` (default 2 — a child of a child is `NotSplittable` and proceeds to rung 3). Both are per-workspace overridable.
- **Post-dispatch audit** (new step in HANDLING, after the gate): beads created during the dispatch window by the dispatching worker's actor are inspected. (a) A bead whose title matches the verification-shaped pattern (`^(verify|test|confirm|validate|check|re-?run)\b`, case-insensitive) and that names the parent's own work is closed with reason `verification is the gate's job (Phase 19.4)` and its body folded into the parent's notes. (b) If the agent created more than `generation.max_per_dispatch` (default 3) beads, the excess (newest first) are set `deferred` with label `over-budget` — visible, reversible, never deleted. *Because* 743 verify-shaped beads and 2,075 split-children were consuming the throughput meant for human-authored work.
- Alert beads (Knot exhaustion, starvation, gate-broken, Unravel proposals) carry `fingerprint:<sha256[:12]>` of `(workspace, kind, cause)`. Creation first looks for an open bead with the same fingerprint and appends to its notes instead; a closed one suppresses re-creation for 24h.
- Fleet metric `generation_ratio` (beads created ÷ beads closed, per day, per workspace and fleet-wide) is emitted by the supervisor; a ratio above 1.0 for three consecutive days raises a fingerprinted alert bead in the NEEDLE workspace itself.

### 19.5 Reclamation runs on the clock, not on Pluck's mood
- Mend's stale-claim sweep runs on a wall-clock timer (`mend.interval_secs`, default 300) inside every worker, independent of whether Pluck found work — the waterfall position stays for its other housekeeping (needle-9f2308f2 carries the implementation).
- Explore's cross-workspace mend passes `stuck_threshold_secs` as the claim TTL instead of `None`, so a roaming worker reclaims by age in remote stores exactly as it does at home.
- Supersession (a worker's own newer claim makes its older claim stale, needle-791e962b) and stale-assignee-on-open (needle-44e7e5cd) are part of the same sweep.

### 19.6 Worker allocation follows frontier health
- Explore ranks candidate workspaces by `(P0 ready count desc, ready count desc, oldest ready bead age desc, path asc)` and de-herds by a per-worker offset `hash(qualified_id) % 3` *within the top three* only — replacing the unconditional per-cycle random shuffle (which violated Principle 1) while keeping bf-6anj4's guarantee that no workspace is unreachable.
- `needle supervise` reports, per workspace, ready count vs. assigned workers, and spawns toward the largest ready frontier first.

### 19.7 Autonomous triage
- Mend closes an open `split-child` whose every parent is closed (reason `orphaned split-child (Phase 19.7)`; 27 existed on 2026-08-29).
- Mend breaks dependency cycles deterministically: for each cycle `bead doctor` reports, remove the edge whose dependency was added most recently (ties: lexically largest blocked id), append a `cycle broken (Phase 19.7): removed <blocked> ← <blocker>` note to both beads, and emit `mend.cycle_broken`. *Because* on 2026-08-29 four cycles held the P0 acceptance beads (needle-66b015d6, needle-165ab6f6) permanently unready, and a cycle has no human-free exit otherwise.
- Mend sets `deferred` + label `stale` on P3/P4 beads created by a worker identity (actor matching the fleet's `<agent>-<identifier>` pattern) that have not been touched for 30 days. Human-authored beads are never auto-deferred — the stale list is reported instead.

### 19.8 Observability
- Events: `gate.execution_error`, `workspace.gate_degraded` / `workspace.gate_restored`, `bead.quarantined{round, until}`, `bead.quarantine_expired`, `bead.escalated{rung}`, `bead.human_rung`, `audit.bead_closed_as_verification`, `audit.bead_deferred_over_budget`, `alert.deduplicated{fingerprint}`, `pluck.ordering_degraded`, `generation_ratio`.
- `needle status --ladder` prints the fleet ladder histogram: beads at each rung, per workspace.

### 19.9 Tests
- A bead failing 5 times ends with `quarantined` + `quarantine-until` labels and **no** `manual_blocked` (assert via `bead why --id`); it is absent from Pluck's candidates until the timestamp passes and present afterwards with `failure-count` intact.
- Round 3 expiry produces exactly one analysis dispatch; its outcome is either a new child bead or a `human` label with an `analysis:` note — a bare `human` label without the note fails the test.
- A gate whose command does not exist yields `GateError`, an unchanged `failure-count`, and after three such errors a `gate-degraded` workspace with exactly one claimable `Gate broken:` bead; a passing gate run restores the workspace and closes it.
- Pluck ordering: a P2 root blocking one P0 bead sorts ahead of a bare P1 leaf; two workers see the identical order.
- Post-dispatch audit: an agent that creates four beads including "Verify the endpoint works" ends the cycle with that bead closed, one bead `over-budget`, and the parent's notes containing the folded text.
- Two Knot alerts with the same cause in the same workspace produce one bead with two note entries.
- Migration: a store with a `manual_blocked` open bead comes out of one Mend pass with the flag cleared and the label scheme applied.

### 19.10 Deployment
- Version bump, needle-ci, GitHub release, canary → stable. Then a one-time fleet sweep converts the 148 beads left quarantined on 2026-08-29 (NEEDLE 32, commitgraph 31, spaxel 71, mta-my-way 14) through the 19.2 migration — no hand `bead update` pass.

## Exit criteria
- Fleet-wide `generation_ratio` < 1.0 for seven consecutive days while closes/day stays at or above the 2026-08-29 baseline.
- Zero open beads with `manual_blocked=true` fleet-wide; every quarantined bead has an expiry.
- The human-rung count across all workspaces is below 10 and every one of them carries a rung-4 `analysis:` note.
- No workspace with open, unblocked work has `ready = 0` for more than one Mend interval.
- No stale in-progress claim (older than `stuck_threshold_secs`) survives two Mend intervals in any workspace, including NEEDLE's own.
