# NEEDLE Implementation Plan

> **N**avigates **E**very **E**nqueued **D**eliverable, **L**ogs **E**ffort

## Design Principles

These six principles are non-negotiable. Every design decision in this plan traces back to one or more of them.

1. **Deterministic order.** Given the same queue state, every worker computes the same bead ordering. There is no randomness in selection. Ties are broken by creation time.

2. **Explicit outcome paths.** Every possible result of every operation has a named handler. If an outcome can happen, it has a handler. If it doesn't have a handler, it cannot happen. The type system enforces exhaustiveness.

3. **Platform and model agnostic.** NEEDLE wraps any headless CLI that accepts a prompt and exits. It runs on any POSIX system. It does not depend on any specific AI provider, model, or API.

4. **Observable by default.** Every state transition, claim attempt, dispatch, and outcome emits structured telemetry. A silent worker is a broken worker.

5. **Self-healing.** Workers detect and recover from stuck states, stale claims, crashed peers, and corrupted databases without human intervention. Recovery paths are explicit, not heuristic.

6. **Separation of concerns.** The orchestrator does not execute work. The agent does not manage state. The bead store does not enforce workflow. Each component has one job.

---

## Architecture Overview

NEEDLE is composed of five layers, each documented in its own plan file:

```
┌──────────────────────────────────────────────────────────────┐
│                        CLI Layer                              │
│  needle run | stop | list | attach | status | config          │
├──────────────────────────────────────────────────────────────┤
│                     Worker Layer                              │
│  Worker loop, strand waterfall, session management            │
│  See: strands.md, state-machine.md                            │
├──────────────────────────────────────────────────────────────┤
│                  Coordination Layer                            │
│  Claiming, locking, heartbeats, peer awareness                │
│  See: concurrency.md                                          │
├──────────────────────────────────────────────────────────────┤
│                    Agent Layer                                 │
│  Adapter interface, dispatch, result capture                  │
│  See: agent-adapters.md                                       │
├──────────────────────────────────────────────────────────────┤
│                   Foundation Layer                             │
│  Telemetry, configuration, bead store interface, self-healing │
│  See: telemetry.md, configuration.md, self-healing.md         │
└──────────────────────────────────────────────────────────────┘
```

---

## Component Map

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

---

## Plan Documents

| Document | Contents |
|----------|----------|
| [state-machine.md](state-machine.md) | Core FSM: states, transitions, outcome handlers, error model |
| [architecture.md](architecture.md) | Component architecture, module boundaries, data flow |
| [strands.md](strands.md) | Strand waterfall: purpose, entry/exit conditions, escalation |
| [concurrency.md](concurrency.md) | Multi-worker coordination, claiming, locking, heartbeats |
| [telemetry.md](telemetry.md) | Structured telemetry, metrics, monitoring, debugging |
| [self-healing.md](self-healing.md) | Health checks, recovery, cleanup, database repair |
| [configuration.md](configuration.md) | Config hierarchy, workspace config, runtime overrides |
| [agent-adapters.md](agent-adapters.md) | Agent abstraction, adapter interface, dispatch model |
| [implementation-phases.md](implementation-phases.md) | Phased delivery, success criteria, migration from v1 |

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
| Work decomposition | External (spec2beads or manual) | Built-in mitosis | Mitosis explosion proved in-loop decomposition is dangerous |
| Self-modification | Prohibited in automated mode | Gated with approval | Five incidents of cascading self-modification failures |
| Workspace discovery | Explicit configuration | Filesystem scanning | Explore strand's unbounded find caused 35+ load |
| Alert system | Verify-then-alert with rate limiting | Alert-on-empty | 100% false positive rate from naive alerting |

---

## Key Decisions from Operational Learnings

These decisions are informed by the 9 notes files in `docs/notes/`:

| Learning | Design Response |
|----------|----------------|
| Mitosis explosion (5,741 duplicate beads) | No built-in bead decomposition. Decomposition is an external, human-gated process. |
| 100% false positive starvation alerts | Three-state model: no beads exist / all claimed / invisible. Verify independently before alerting. |
| Bundler shipped undefined functions | Compiled language eliminates this class entirely. |
| Agent-owned closure most reliable | NEEDLE does not close beads. Agent receives `br close <id>` instruction in prompt. |
| stdout/stderr corruption | Telemetry is a structured system, never interleaved with agent output. |
| Workers modifying their own orchestrator | NEEDLE binary is immutable during a session. Updates apply between sessions only. |
| ~20 worker practical limit (EX44) | Fleet sizing is configurable with enforced ceiling. Staggered launch is default. |
| Bead granularity affects success rate | Document guidelines but don't enforce — this is a bead authoring concern, not orchestration. |
