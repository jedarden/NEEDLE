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

---

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

---

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

See [telemetry.md](telemetry.md) for full specification. The module exposes:

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

See [self-healing.md](self-healing.md) for full specification. Core interface:

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

---

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
