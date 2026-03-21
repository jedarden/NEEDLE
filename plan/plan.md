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
├── claim/            Atomic claiming, lock management
├── prompt/           Prompt construction from bead context
├── dispatch/         Agent adapter loading, process execution
├── outcome/          Exit code classification, outcome handlers
├── telemetry/        Structured event emission, sinks
├── health/           Heartbeat, stuck detection, peer monitoring
├── config/           Hierarchical configuration loading
├── bead_store/       Abstract bead backend interface
└── types/            Shared types, error definitions
```

### Dependency Graph

```
cli ──► worker ──► strand ──► bead_store
                │           │
                ├──► claim ─┘
                │
                ├──► prompt ──► bead_store
                │
                ├──► dispatch
                │
                ├──► outcome ──► bead_store
                │               ├──► telemetry
                │               └──► health
                │
                ├──► telemetry
                │
                └──► health ──► telemetry

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
- `Outcome::Success` does NOT mean the bead is closed. It means the agent exited cleanly. Bead closure is the agent's responsibility.

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
}
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

## Binary Structure

NEEDLE is a single binary with subcommands:

```
needle run [--workspace PATH] [--agent NAME] [--count N] [--identifier NAME]
needle stop [--all | --identifier NAME]
needle list [--format table|json]
needle attach <identifier>
needle status [--format table|json]
needle config [--get KEY | --set KEY VALUE]
needle doctor [--repair]
needle version
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

---

# Strand Waterfall

Strands are NEEDLE's strategy for finding work. They are evaluated in strict sequence — the first strand that yields actionable work wins. When a strand returns `NoWork`, the worker falls through to the next.

The waterfall is the answer to "what does a worker do when it has no beads?" It is not a priority system for beads (that's handled by deterministic ordering within each strand). It is a priority system for *strategies*.

## Waterfall Sequence

```
  Strand 1: PLUCK ──── primary work from assigned workspace
       │ no work
       ▼
  Strand 2: EXPLORE ── look for work in other configured workspaces
       │ no work
       ▼
  Strand 3: MEND ───── cleanup: stale claims, orphaned locks, health
       │ nothing to clean
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
  Strand 7: KNOT ───── alert human, enter backoff
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

## Strand 2: Explore

**Purpose:** Discover work in other configured workspaces when the home workspace is empty.

**Invokes agent:** No. Explore only finds candidates — execution happens back through the standard CLAIMING → DISPATCHING flow.

**Entry condition:** Strand 1 returned no work. Explore is enabled in config. At least one additional workspace is configured.

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
| No candidates in any workspace | Return `NoWork` → fall through to Strand 3 |

**Design notes (from `docs/notes/explore-strand-bugs.md`):**
- **No filesystem scanning.** NEEDLE-deprecated's `find`-based discovery caused 35+ CPU load with 40 workers. Workspaces must be explicitly configured.
- **No upward traversal.** The v1 explore strand walked up parent directories to `/home`, then `/`. This is eliminated.
- **Workspace list is static** for the duration of a session. It is read from config at boot and not re-evaluated.
- **Workers do not permanently relocate.** If a worker finds work in another workspace, it processes that bead and returns to its home workspace for the next cycle.

## Strand 3: Mend

**Purpose:** Maintenance and cleanup operations that keep the bead store healthy.

**Invokes agent:** No.

**Entry condition:** Strands 1-2 returned no work.

**Algorithm:**
1. **Stale claim cleanup:** Find beads with status `in_progress` where the assigned worker has no active heartbeat (TTL expired). Release them.
2. **Orphaned lock cleanup:** Find workspace lock files older than TTL. Remove them.
3. **Dependency cleanup:** Find closed beads that are still listed as blockers on open beads. Remove the stale dependency links.
4. **Database health:** Run `br doctor` (not `--repair` unless errors found).

**Exit conditions:**
| Result | Action |
|--------|--------|
| Cleanup performed | Return `WorkCreated` → restart from Strand 1 (released beads may now be claimable) |
| Nothing to clean | Return `NoWork` → fall through to Strand 4 |

**Design notes (from `docs/notes/bead-lifecycle-bugs.md`):**
- Stale dependency links caused permanent blocking in NEEDLE-deprecated. Mend must clean these.
- Distinguish "did work" from "found nothing" — v1 had an infinite loop where mend returned success on failed releases.

## Strand 4: Weave (opt-in)

**Purpose:** Analyze workspace documentation for gaps and create new beads to address them.

**Invokes agent:** Yes — uses the agent to analyze documentation and propose beads.

**Entry condition:** Strands 1-3 returned no work. Weave is explicitly enabled in workspace config (`strands.weave.enabled: true`).

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

## Strand 7: Knot

**Purpose:** All work-finding strategies are exhausted. Alert the human and enter backoff.

**Invokes agent:** No.

**Entry condition:** Strands 1-6 all returned `NoWork`.

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
  knot:
    enabled: true           # always on, cannot be disabled
    alert_cooldown_minutes: 60
    exhaustion_threshold: 3 # cycles before creating alert bead
```

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
| `bead.orphaned` | Agent exited 0 but bead still open | `bead_id` |

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

### Health

| Event Type | Emitted When | Data Fields |
|------------|-------------|-------------|
| `heartbeat.emitted` | Heartbeat file updated | `state`, `current_bead` |
| `peer.stale` | Stale peer detected | `peer_id`, `last_seen`, `claimed_bead` |
| `peer.crashed` | Dead peer cleaned up | `peer_id`, `released_bead` |
| `health.check` | Periodic health check | `db_healthy`, `disk_free_mb`, `peer_count` |

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

## Self-Modification Protection

From `docs/notes/self-modification-risks.md`: NEEDLE workers must not modify the NEEDLE binary or configuration during a session.

1. **Binary immutability.** The NEEDLE binary is not modified while any worker is running. Updates are applied between sessions only (via `needle upgrade` when no workers are active).

2. **Configuration immutability.** Config is loaded at boot and cached for the session. Changes to config files take effect on next worker restart, not mid-session. No hot-reload.

3. **Workspace exclusion.** If NEEDLE's source code lives in a workspace with beads, workers do not process beads for NEEDLE itself unless explicitly configured. This prevents the self-referential bug cycles documented in v1.

4. **Version pinning.** All workers in a fleet run the same NEEDLE version. `needle run` checks that the binary version matches the registry version and refuses to start if mismatched.

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
  max_workers: 20
  launch_stagger_seconds: 2
  idle_timeout: 300
  idle_action: wait
  max_claim_retries: 5
  identifier_scheme: nato

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

# ── Cost Tracking ──
pricing: {}
budget:
  warn_usd: 0
  stop_usd: 0

# ── Self-Modification Protection ──
protection:
  exclude_workspaces: []
  allow_self_modification: false
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

- `worker.max_workers` > CPU count (performance warning)
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

## Prompt Construction

The prompt given to the agent is a deterministic function of the bead state — same bead produces the same prompt.

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

### Agent-Owned Closure

The prompt instructs the agent to close the bead via `br close`. NEEDLE does not close beads itself. This is a deliberate design decision based on v1 experience:

- The agent knows whether the work is actually done
- NEEDLE's post-dispatch parsing of agent output was fragile
- Exit code 0 does not guarantee the work was completed correctly
- The agent can include a meaningful closure message

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
- `needle attach`, `needle status`, `needle config`

### Success Criteria

- [ ] `needle run --workspace /path --agent claude-anthropic-sonnet` launches a worker in tmux
- [ ] Worker claims a bead, dispatches to Claude Code, handles outcome
- [ ] All 6 outcome paths tested with mock agent (exit 0, 1, 124, 127, 130, timeout)
- [ ] Telemetry JSONL file contains events for every state transition
- [ ] `needle list` shows running workers
- [ ] `needle stop` gracefully stops a worker (releases claimed bead)
- [ ] Worker loops: after handling one bead, it selects the next
- [ ] Worker exhausts: when no beads remain, enters backoff and eventually exits
- [ ] Binary compiles for Linux x86_64 and macOS arm64

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
| **Strand 2 (Explore)** | Roam configured workspaces for work |
| **Strand 3 (Mend)** | Stale claim cleanup, orphaned locks, dependency repair, db health |
| **Worker state registry** | Shared fleet state file |
| **Concurrency limits** | Provider/model max_concurrent, RPM limiting |
| **Workspace config** | `.needle.yaml` per-workspace overrides |
| **Additional adapters** | OpenCode, Codex, Aider built-in |
| **Cost tracking** | Token extraction, pricing config, effort events |
| **Budget enforcement** | Warn/stop at daily cost thresholds |
| **CLI extensions** | `needle attach`, `needle status`, `needle config` |
| **Database recovery** | Auto-detect corruption, repair from JSONL |

### Success Criteria

- [ ] `needle run --count 5` launches 5 workers with staggered startup
- [ ] Workers claim different beads (no thundering herd — verify via telemetry)
- [ ] Crashed worker's claimed bead is released by peer within heartbeat_ttl
- [ ] Workers discover work in other configured workspaces (Explore strand)
- [ ] Mend strand cleans stale claims and orphaned locks
- [ ] Provider concurrency limits enforced (>N workers for same provider wait)
- [ ] `needle status` shows fleet summary with per-worker and per-bead stats
- [ ] `needle attach alpha` connects to a running worker's tmux session
- [ ] Cost tracking produces accurate estimates (±20% of actual)
- [ ] Database corruption is detected and auto-repaired
- [ ] Workspace `.needle.yaml` overrides global config correctly

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
| **Self-update** | `needle upgrade` with version checking |
| **Doctor command** | `needle doctor` for full system health check |
| **Telemetry queries** | `needle logs --filter`, `needle status --cost` |
| **Installer** | One-liner install script, GitHub releases |

### Success Criteria

- [ ] Weave strand creates valid beads from documentation gaps (with guardrails)
- [ ] Unravel strand proposes alternatives for HUMAN beads without modifying originals
- [ ] Pulse strand detects codebase issues and creates beads (with deduplication)
- [ ] All opt-in strands respect cooldowns and max-bead limits
- [ ] Validation gates block bead closure when tests fail
- [ ] Hook sink delivers events to configured webhooks
- [ ] `needle upgrade` downloads and installs new version
- [ ] `needle doctor` reports system health across all subsystems
- [ ] One-liner install works on Linux and macOS

### Estimated Scope

~8 additional source files, ~3,000 additional LOC.

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

### Property Tests

| Property | Description |
|----------|-------------|
| **Deterministic ordering** | For any queue state, all workers compute the same candidate ordering |
| **Exhaustive outcomes** | The outcome enum covers all possible exit codes (no `_` wildcard) |
| **Claim exclusivity** | Given N concurrent claim attempts on 1 bead, exactly 1 succeeds |
| **Heartbeat liveness** | A healthy worker's heartbeat is always within TTL |

### No Mocking of `br`

From `docs/notes/mitosis-explosion-postmortem.md`: v1's tests mocked `br` output and missed that `br show --json` never included labels. v2 integration tests run against a real `br` instance with a test `.beads/` directory.
