# NEEDLE Configuration Guide

NEEDLE reads configuration from multiple sources in order of precedence (highest last):

1. Built-in defaults
2. Global config file (`~/.config/needle/config.yaml`)
3. Workspace config file (`.needle.yaml` in workspace root)
4. Environment variables (`NEEDLE_*` prefix)
5. CLI arguments

This guide covers the most commonly used configuration options.

---

## Configuration Reload Tiers

**Every configuration key has a reload tier** that determines how changes take effect. The authoritative tier table is implemented in [`src/config/tiers.rs`](../src/config/tiers.rs) — this is the single source of truth for which keys are in which tier.

| Tier | Behavior | Applies When |
|------|----------|--------------|
| **A (Live)** | Takes effect immediately on the next worker cycle | Change the config file; no restart needed |
| **B (Rebuild)** | Component rebuilt at cycle boundary | Change the config file; no restart needed |
| **C (Restart Required)** | Requires full process restart | **Must restart workers** to apply |

**Critical:** Editing a **Tier-C** key without restarting produces a **silent no-op** — the worker continues running with the old value and emits `config.reload.restart_required`. This is the same failure mode that let a `.needle.yaml` enable OTLP for days while doing nothing.

### Tier A (Live) — Most Common Operations

**These keys take effect immediately on the next cycle.** No restart, no component rebuild.

**Agent settings:**
- `agent.default` — Default agent CLI
- `agent.timeout` — Agent process timeout
- `agent.args` — Extra arguments passed to agent
- `agent.routing.*` — Model-to-adapter routing rules

**Worker runtime behavior:**
- `worker.idle_timeout` — Seconds to wait between queue polls when idle
- `worker.idle_action` — What to do when queue is empty (`wait` or `exit`)
- `worker.allow_exit_without_supervisor` — Opt-in to exit without supervisor
- `worker.enforce_shipped_work` — Require pushed work (or an explanatory bead note) before accepting a closure
- `worker.max_claim_retries` — Maximum claim retry attempts
- `worker.cpu_load_warn` — CPU load warning threshold
- `worker.memory_free_warn_mb` — Memory warning threshold
- `worker.building_timeout` — Timeout for building prompts
- `worker.freshness_check_interval_secs` — Interval for checking fresh beads

**Budget and pricing:**
- `budget.warn_usd` — Warning threshold in USD
- `budget.stop_usd` — Stop threshold in USD
- `pricing` — Provider pricing model

**Strand thresholds:**
- `strands.pluck.exclude_labels` — Labels to exclude from plucking
- `strands.mend.stuck_threshold_secs` — Time before a claimed in-progress bead is considered stale and becomes a release candidate. `strands.mend.stale_claim_ttl` is an accepted alias of the same key (the name used in the original plan); both spellings are live-reloadable
- `strands.mend.lock_ttl_secs` — Lock file TTL. `strands.mend.lock_ttl` is an accepted alias of the same key
- `strands.explore.workspaces` — Pinned workspace list
- `strands.weave.*` — Weave bead creation limits
- `strands.unravel.*` — Unravel alternative limits
- `strands.pulse.*` — Pulse scanner settings
- `strands.reflect.*` — Learning and skill capture limits
- `strands.splice.*` — Live loop detection thresholds
- `strands.knot.*` — Exhaustion recovery settings
- `strands.mitosis.enabled` — Enable/disable mitosis
- `strands.mitosis.first_failure_only` — Split only on first failure
- `strands.mitosis.max_depth` / `strands.mitosis.max_children` — Split-tree caps; together they bound the worst case at `1 + C + C² + … + C^max_depth` beads per parent (73 at the shipped 8 × 2)

**Outcome handling:**
- `outcome.quarantine_after_failures` — Quarantine bead after N failures

**Stop command:**
- `stop.grace_period_secs` — Grace period for killed processes to exit

### Tier B (Rebuild) — Component Reconstruction

**These keys trigger component rebuild at the cycle boundary.** No restart needed.

**Telemetry:**
- `telemetry.file_sink.*` — Log file settings (enabled, log_dir, retention_days, rotation)
- `telemetry.stdout_sink.*` — Console output (enabled, format, color)
- `telemetry.hooks` — Webhook configuration
- `telemetry.otlp.enabled` — **OTLP can be toggled on a running worker**
- `telemetry.otlp.endpoint` — Collector endpoint
- `telemetry.otlp.protocol` — gRPC or HTTP
- `telemetry.otlp.headers` — Authorization headers (supports `env:` references)
- `telemetry.otlp.timeout_ms` — Request timeout
- `telemetry.otlp.compression` — Compression (gzip, none, zstd)
- `telemetry.otlp.tls.*` — TLS configuration
- `telemetry.otlp.signals` — Which signals to export
- `telemetry.otlp.resource_attributes` — OTEL resource attributes

**Prompt building:**
- `prompt.context_files` — Context file patterns
- `prompt.instructions` — Agent instructions
- `prompt.templates` — Prompt templates
- `prompt.variants` — Prompt variants

**Agent adapters:**
- `agent.adapters_dir` — Directory containing adapter YAML files

**Rate limiting:**
- `limits.providers` — Provider-level rate limits
- `limits.models` — Model-level rate limits

**Validation gates:**
- `gates` — Gate configuration. Gates resolve from the **bead's** workspace config at outcome time, never from the worker's startup config — a worker homed in a gate-declaring workspace does not carry those gates onto foreign beads, and a workspace that declares no gates runs none (needle-da77b68a)
- `verification` — Legacy gate commands; resolved the same per-workspace way
- `validation.outcome_timeout_seconds` — Gate execution timeout
- `validation.stderr_cap_bytes` — Stderr capture limit

**Attempt archive:**
- `attempt_archive.*` — Per-attempt transcript/trace/prompt bundling to the local spool (all keys; default off)

### Tier C (Restart Required) — Process Identity

**These keys cannot be changed without restarting the worker process.** Changes here emit `config.reload.restart_required` and **do not take effect**.

**Worker identity and launch:**
- `worker.max_workers` — **Maximum concurrent workers** (supervisor-only)
- `worker.launch_stagger_seconds` — Delay between worker launches
- `worker.identifier_scheme` — Worker naming scheme
- `worker.worker_binary_path` — Path to worker binary
- `worker.config_reload_check_interval_secs` — **Polling interval for config reloads** (default: 0/disabled)

**Workspace paths:**
- `workspace.home` — **NEEDLE home directory** for heartbeats and logs
- `workspace.default` — Default workspace directory

**Bead CLI backend:**
- `bead_cli.backend` — **Bead backend type** (`bead-forge` or `bead-rs`)
- `bead_cli.path` — Explicit path to bead CLI

**Health monitoring:**
- `health.heartbeat_interval` — Heartbeat interval
- `health.heartbeat_ttl` — Heartbeat TTL
- `health.heartbeat_dir` — Heartbeat directory
- `health.peer_check_interval` — Peer discovery check interval

**Process-level configuration:**
- `tsnet` — Tailscale networking configuration
- `self_modification` — Hot-reload and upgrade settings
- `supervisor` — Supervisor configuration

**⚠️ Restart-Required Summary:** The following configuration categories **always require a full process restart** to take effect:

| Category | Why Restart Required | Key Examples |
|----------|---------------------|--------------|
| **Worker identity** | Process name and PID registry | `worker.identifier_scheme`, `worker.max_workers` |
| **Workspace paths** | Bead store and heartbeat file locations | `workspace.home`, `workspace.default` |
| **Bead CLI backend** | Store-level decision made at process start | `bead_cli.backend`, `bead_cli.path` |
| **Runtime shape** | Tokio runtime and tracing stack | Embedded in process startup (no config key) |
| **Process supervision** | Daemon lifecycle and heartbeat protocol | `supervisor.*`, `health.*` |

**Note:** The tokio runtime and tracing stack are process-level infrastructure with no configuration keys — they are initialized once at process startup and cannot be reconfigured without restarting the process. This is why `telemetry.otlp.*` is Tier B (rebuilds the OTLP layer) but the underlying tracing stack itself is Tier C.

### Common Operations and Their Tiers

| Operation | Tier | How to Apply |
|-----------|------|--------------|
| Change agent timeout | A | Edit config file; applies next cycle |
| Enable OTLP export | B | Edit config file; applies next cycle |
| Adjust memory warning | A | Edit config file; applies next cycle |
| Change worker max_workers | **C** | **Must restart supervisor** |
| Move workspace.home | **C** | **Must restart all workers** |
| Switch bead CLI backend | **C** | **Must restart all workers** |
| Enable config polling | **C** | **Must restart workers** (default: 0/disabled) |
| Change prompt templates | B | Edit config file; PromptBuilder rebuilt next cycle |

### Detecting When Restart Is Required

When a Tier-C key is changed in the config file, a running worker:

1. Detects the change on the next config reload check
2. Emits `config.reload.restart_required` telemetry event
3. Continues running with the **old** value
4. Logs a WARN-level message

**This is a silent failure mode** — the config file says one thing, but the worker is doing another. Always check for `config.reload.restart_required` events after editing configuration.

### Enabling Config Reload Polling

By default, config reload polling is **disabled** (`worker.config_reload_check_interval_secs: 0`). To enable hot-reload:

```yaml
# ~/.config/needle/config.yaml
worker:
  config_reload_check_interval_secs: 30  # Check every 30 seconds
```

**This setting is Tier-C (restart required)** — a worker started with the default `0` cannot discover a file change that enables polling. Restart the worker after changing this value.

### Querying the Tier of Any Key

To find out which tier a specific configuration key belongs to, you can:

```bash
# Check the implementation directly
grep '"<key.path>"' src/config/tiers.rs

# Example: check worker.max_workers
# Output: ("worker.max_workers", ReloadTier::RestartRequired)
```

The authoritative tier table is in [`src/config/tiers.rs`](../src/config/tiers.rs) at the `TIER_TABLE` constant. Every configuration key must have an entry in this table — adding a new config key without a tier assignment is a compile-time failure.

### Verification

To check what a running worker is actually using (not just what the config file says):

```bash
# Dump the live config from a running worker
needle config --dump --show-source
```

This shows the **live** config including reload generation, so you can confirm what the worker is actually running rather than what the file says.

---

## Worker Configuration

The `worker` section controls fleet behavior and worker spawning.

### Basic Worker Settings

```yaml
# ~/.config/needle/config.yaml
worker:
  max_workers: 4              # Maximum concurrent workers (default: 4)
  idle_timeout: 60            # Seconds to wait between queue polls when idle (default: 60)
  launch_stagger_seconds: 2   # Delay between worker launches (default: 2)
```

### Idle Action and Supervisor Safety

`worker.idle_action` controls what a worker does when the bead queue is empty:

```yaml
worker:
  idle_action: wait           # What to do when queue is empty: "wait" or "exit" (default: wait)
```

**Default behavior without supervisor:** When no supervisor is detected and `idle_action=exit`, the worker automatically defaults to `wait` and emits a `ConfigWarning` telemetry event. This prevents orphaned beads — without a supervisor to spawn replacement workers, an exiting worker leaves any in-progress beads stranded with no recovery mechanism.

**Opt-in to exit without supervisor:** If you understand the orphaned bead risk and have an external recovery mechanism, you can explicitly opt-in:

```yaml
worker:
  idle_action: exit
  allow_exit_without_supervisor: true  # Explicit opt-in (default: false)
```

**With supervisor present:** When a supervisor is actively managing the fleet (detected via heartbeat files), `idle_action=exit` is safe and no override is needed. The supervisor will spawn replacement workers that can reclaim orphaned beads via heartbeat-based peer discovery.

**Detection failures are treated as "no supervisor":** Supervisor presence is checked via a heartbeat file (with a 2-minute freshness TTL) and, failing that, a supervisor socket. If the heartbeat file is corrupt or unreadable, detection falls through to the socket check; if neither can establish presence, the worker behaves as unsupervised — the guard still runs and the `wait` default still applies. A degraded heartbeat never silently disables the safety check.

**The same rule applies to config reloads:** `worker.idle_action` and `worker.allow_exit_without_supervisor` are hot-reloadable (Tier A), and the reload path enforces the identical guard as startup. A reload that requests `idle_action: exit` while no supervisor is detected is downgraded to `wait` with a `ConfigWarning`; enabling `allow_exit_without_supervisor: true` in the reloaded config is honored without a restart. A reload also re-checks the supervisor even when `exit` is already the running policy — if the supervisor has died since boot, the next reload falls back to `wait` instead of keeping an unguarded exit policy.

### Disposable Scratch Checkout Sweep

At startup, before opening the bead store or claiming work, each worker tries
to sweep stale disposable fleet clones from `$HOME/scratch`. A host-local lock
ensures that only one concurrently starting worker performs the sweep.

```yaml
# ~/.config/needle/config.yaml (host-level; not a workspace override)
worker:
  scratch_sweep:
    enabled: true
    ttl_hours: 48
```

Removal is intentionally fail-closed. An entry must be older than the TTL,
match a known fleet-output prefix (`rota-`, `armor-`, `needle-`, `seam-`,
`tg-pitr-`, `icg-`, or `claude-print-`), and be an independent Git clone with
an `origin`, no stash, and no commits absent from remote refs. Git worktrees,
similarly named non-repositories, and reference checkouts such as
`esphome-source`, `tcb-bisect`, and `openbao-source.*` are not eligible.

The worker checks `/proc` both before auditing and immediately before removal.
It preserves a candidate if any same-user process has a working directory
inside it, if a running `cargo` or `rustc` command references it, or if process
inspection is inconclusive. Structured `scratch_sweep` log events include each
removed path and the total allocated bytes reclaimed. These settings are
restart-required and belong in the global config, not `.needle.yaml`.

### Configuration Hot-Reload Check

`worker.config_reload_check_interval_secs` controls how often a worker checks
for configuration changes at the cycle boundary, between dispatches. A value
of `0` disables the check and is the default until the feature has had fleet
time:

```yaml
worker:
  config_reload_check_interval_secs: 0  # Seconds between checks; 0 disables (default)
```

This setting is Tier C (restart-required) because it gates the reload mechanism
itself. In particular, a worker started with the default `0` cannot discover a
file change that enables polling; restart the worker after changing this value.

### Launch Admission Under Resource Pressure

Before a worker selects work — and before the supervisor spawns one, or the
one-shot launcher forks sessions — CPU and memory are checked against
`worker.cpu_load_warn` (normalized load: 1-minute load average divided by core
count) and `worker.memory_free_warn_mb` (available memory). Above policy, the
worker does **not** exit: a service-managed run enters the
`ADMISSION_BLOCKED` state and holds resident, emitting rate-bounded
`worker.admission_blocked` heartbeats and retrying on a capped jittered
backoff until the host is back inside policy, when it emits
`worker.admission_restored` and resumes selection (plan revision 24 §4.6,
N-T33). It claims no bead while blocked and never relaxes the thresholds to
escape the hold.

A one-shot caller (`needle run` outside a service or tmux inner session)
cannot stay resident, so it receives an explicit temporary-unavailable
outcome — `AdmissionUnavailable`, exit code `75` (`EX_TEMPFAIL`) — with no
sessions forked and no work-failure counted, because no bead was claimed.

Backoff and heartbeat pacing have environment overrides (mainly for tests and
fixtures; defaults are fine in production):

| Environment variable | Default | Meaning |
|----------------------|---------|---------|
| `NEEDLE_ADMISSION_BACKOFF_BASE_MS` | `5000` | First retry delay; doubles with ±25% jitter on each attempt. |
| `NEEDLE_ADMISSION_BACKOFF_CAP_MS` | `30000` | Maximum retry delay. |
| `NEEDLE_ADMISSION_HEARTBEAT_SECS` | `30` | Minimum spacing between `worker.admission_blocked` emissions. |
| `NEEDLE_LAUNCH_RESOURCE_PROBE` | — | Read load/memory from a directory of mock `loadavg`/`meminfo` files instead of `/proc` (test fixtures). |
| `NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK` | — | `1` disables the gate entirely. For test harnesses that must launch onto a busy box; do not set on fleet workers. |

### Worker Binary Path Override

**When to use this:** Set `worker_binary_path` when the running binary's path is deliberately not what should be spawned as workers. This is needed in these cases:

- **Wrapper script deployments:** If you run NEEDLE via a wrapper script (e.g., for environment setup or signal handling), the supervisor would spawn workers that re-execute the wrapper instead of the actual needle binary.
- **PATH conflicts:** If another tool on your system is named `needle` and appears earlier in `$PATH`, workers would spawn the wrong binary without this override.
- **Custom install locations:** If needle is installed at a non-standard path and you want to explicitly control which binary is spawned.

**Default behavior:** When `worker_binary_path` is not set (or `null`), the supervisor resolves the worker binary via `std::env::current_exe()` — this is always correct when supervisor and workers are built from the same binary.

**Example:**

```yaml
# ~/.config/needle/config.yaml
worker:
  # Explicit path to the worker binary
  # Use absolute paths or paths starting with ~ (expanded to $HOME)
  worker_binary_path: /opt/needle/bin/needle

  # Or use tilde expansion for home directory
  # worker_binary_path: ~/local/bin/needle
```

**How it works:**

- The supervisor logs the resolved binary path at startup (check your logs for "worker spawn path resolved to...")
- The path is validated for existence at startup — a typo or missing binary will fail immediately, not silently spawn the wrong process
- Relative paths are resolved relative to the supervisor's current working directory

**Troubleshooting:**

```bash
# Check what binary your supervisor will spawn
needle supervise --dry-run

# Verify the path exists and is executable
test -x /opt/needle/bin/needle && echo "OK" || echo "MISSING"

# Check supervisor logs for the resolved path
grep "worker spawn path" ~/.needle/logs/supervisor.log
```

**See also:** GitHub issue [jedarden/NEEDLE#11](https://github.com/jedarden/NEEDLE/issues/11) — background on why this override exists.

---

## Agent Configuration

The `agent` section controls how NEEDLE invokes AI agents.

### Basic Agent Settings

```yaml
# ~/.config/needle/config.yaml
agent:
  default: claude              # Default agent CLI (default: "claude")
  timeout: 3600                # Agent process timeout in seconds (default: 3600 = 1 hour)
  args: []                      # Extra arguments passed before the prompt (default: [])

  # Directory containing adapter YAML files (default: ~/.config/needle/adapters)
  adapters_dir: ~/.config/needle/adapters
```

### Process Timeouts: Idle vs Hard Deadline

NEEDLE supports two complementary timeout mechanisms for agent processes:

#### Idle Timeout (Resettable)

The **idle timeout** fires when no stdout/stderr activity occurs for the configured duration. It resets on each output byte, allowing long-running agents that stay active.

**Characteristics:**
- **Resettable**: Deadline resets to `now + idle_timeout` on each output byte
- **Activity-dependent**: Only fires if the agent is silent for the full duration
- **Permissive**: Allows indefinite execution as long as agent produces output

**Example behavior:**
```text
idle_timeout = 10 seconds

Timeline:
t=0s:   Process spawns, idle deadline set to t=10s
t=5s:   Agent produces output → deadline resets to t=15s
t=12s:  Agent produces output → deadline resets to t=22s
t=30s:  Agent goes silent...
t=40s:  IDLE DEADLINE EXPIRES (10s of silence) → Process killed
```

**When to use:**
- Allow long-running agents that make continuous progress
- Detect hung or stalled agents (no output for an extended period)
- Support streaming agents that produce incremental results

#### Hard Deadline (Non-Resettable, Absolute)

The **hard deadline** is an absolute upper bound on total execution time. Once a process is spawned, the deadline is set to `spawn_time + hard_timeout_secs` and **never changes**, regardless of agent activity.

**Characteristics:**
- **Absolute**: Computed once at spawn time from `Instant::now()`
- **Non-resettable**: No mechanism extends or modifies the deadline after creation
- **Activity-independent**: Fires even if the agent is producing output
- **Strict enforcement**: Process is killed via `SIGKILL` when deadline expires

**Example behavior:**
```text
hard_timeout = 5 seconds

Timeline:
t=0s:  Process spawns, hard deadline set to t=5s
t=1s:  Agent produces output (deadline remains t=5s)
t=2s:  Agent produces output (deadline remains t=5s)
t=3s:  Agent produces output (deadline remains t=5s)
t=5s:  HARD DEADLINE EXPIRES → Process killed
```

**When to use:**
- Set strict upper bounds for expensive operations (e.g., model training, large file processing)
- Prevent runaway agents that produce output but never terminate
- Enforce service-level agreements on total execution time
- Complement idle timeout with a maximum wall-clock limit

#### Configuring Both Timeouts

You can configure both timeouts simultaneously:

```yaml
agent:
  default: claude
  timeout: 3600                # Legacy single timeout (for backward compatibility)

# New dual-timeout configuration (preferred)
process_limits:
  # 5 minutes of silence triggers idle timeout
  idle_timeout: 300s

  # 1 hour absolute upper bound (non-resettable)
  hard_deadline:
    enabled: true
    duration_secs: 3600
```

In this configuration:
- If the agent produces output continuously, it runs until the 1-hour hard deadline
- If the agent goes silent for 5 minutes, the idle deadline fires first
- The first deadline to expire kills the process

**Disabling timeouts:**
```yaml
process_limits:
  idle_timeout: null           # No idle timeout (process can hang indefinitely)
  hard_deadline:
    enabled: false             # No hard deadline (no absolute time limit)
```

### Model-to-Adapter Routing

```yaml
agent:
  # Model-to-adapter routing (optional)
  routing:
    # Rules are evaluated in order; first match wins
    rules:
      - match_model: "(claude-)?(sonnet|opus|fable|haiku).*"
        adapter: claude         # Built-in adapter for Anthropic models
    default_adapter: claude     # Fallback for non-matching models
    strict: false                # If true, fail when no rule matches (default: false)
```

**User-defined adapter example:**

You can create custom adapters by placing YAML files in `~/.config/needle/adapters/`:

```yaml
# ~/.config/needle/adapters/my-custom-adapter.yaml
# Example: Custom adapter for a specific API endpoint
adapter:
  cli: my-custom-cli
  args:
    - --endpoint
    - https://api.example.com/v1
    - --model
    - "$MODEL"
  env:
    API_KEY: "${MY_API_KEY}"  # Environment variable reference
```

Then reference it in routing rules:

```yaml
agent:
  routing:
    rules:
      - match_model: "gpt-.*"
        adapter: my-custom-adapter  # Uses ~/.config/needle/adapters/my-custom-adapter.yaml
    default_adapter: claude
    strict: false
```

---

## Workspace Configuration

The `workspace` section defines where NEEDLE stores state and which workspace to process.

```yaml
# ~/.config/needle/config.yaml
workspace:
  # Default workspace directory (default: current directory)
  default: ~/dev/my-project

  # NEEDLE home directory for heartbeats and logs (default: ~/.needle)
  home: ~/.needle

  # Domain labels for cross-workspace skill sharing (optional)
  labels:
    - rust
    - trading
    - api
```

**Workspace overrides (`.needle.yaml`):**

Only certain fields can be overridden at the workspace level. Every workspace
with a bead store must explicitly bind its backend; NEEDLE does not infer store
ownership from whichever executable appears first on `PATH`.

```yaml
# .needle.yaml (in workspace root)
workspace:
  labels:                    # ONLY this field is overridable at workspace level
    - frontend
    - react

bead_cli:
  backend: bead-forge       # Existing bead-forge workspace
# backend: bead-rs          # Native bead-rs workspace after rehydration
# explicit_path: /opt/bin/bead  # Optional host-specific operator override

gates:                       # Validation gates owned by THIS workspace
  - type: command
    commands:
      - scripts/definition-of-done.sh --fast

# These fields are ignored if set in .needle.yaml:
# - workspace.default (resolved globally)
# - workspace.home (resolved globally)
```

`gates` (and its legacy `verification` spelling) declared here apply to beads
that **belong to this workspace**: they are resolved from the bead's workspace
config when the dispatch outcome is judged, the same way `bead_cli.backend` is.
A worker homed in a gate-declaring workspace does not carry those gates onto
beads elsewhere — a workspace that declares no gates runs none. Declare a gate
in every workspace whose work it should judge.

`bead-forge` resolves the `bf` executable and requires its identity to begin
with `bf `. `bead-rs` resolves `bead` and requires `bead `. Identity is checked
before the store is opened. A missing, unknown, or mismatched binding makes the
workspace ineligible for dispatch. Changing this value does not migrate data:
initialize and reconcile the destination store first, then change the binding
during the reviewed cutover.

Explore, Splice, and the supervisor load each target repository's binding
independently, so bead-forge and bead-rs workspaces can coexist during rollout.

---

## Strand Configuration

NEEDLE runs multiple "strands" that find or create work when the primary workspace is empty.

### Explore (Multi-workspace Discovery)

```yaml
# ~/.config/needle/config.yaml
strands:
  explore:
    enabled: true                          # Enable Explore strand (default: true)

    # Leave empty for auto-discovery (recommended default)
    # All directories under workspace_root containing .beads/ are scanned
    workspaces: []

    # Root for auto-discovery (default: $HOME)
    workspace_root: ~/dev

    # Minimum cycles between scans; empty scans back off geometrically (default: 1)
    scan_interval_cycles: 1

    # Maximum cycles between scans after backoff (default: 8)
    max_scan_interval_cycles: 8

    # Re-scan for new workspaces every N cycles (default: 60)
    rediscovery_cycles: 60

    # Alert if no beads claimed for this many minutes (default: 15)
    starvation_threshold_minutes: 15
```

**Pinned mode (exception):** To restrict a worker to specific workspaces:

```yaml
strands:
  explore:
    workspaces:
      - ~/dev/project-a
      - ~/dev/project-b
    # WARNING: When workspaces is non-empty, auto-discovery is DISABLED
    # and only the listed paths are scanned
```

### Mitosis (Bead Splitting)

```yaml
strands:
  mitosis:
    enabled: true                # Enable mitosis (default: true)
    first_failure_only: true     # Evaluate only on the first failure (default: true)
    force_failure_threshold: 0   # Evaluate from the Nth failure on (0 = use the policy above)
    repeat_interval: 0           # Re-evaluate every N failures after the first (0 = disabled)
    max_depth: 2                 # Maximum generation depth (0 = UNLIMITED — not the default)
    max_children: 8              # Maximum children created per split
    child_count_warning_threshold: 24  # Warn when one parent's cumulative children cross this (0 = off)

    # Timeout-triggered mitosis (opt-in, default: disabled)
    timeout_triggered:
      enabled: false
      agent_wallclock_timeout: false
      handler_timeout: false
      min_elapsed_fraction: 0.9  # Trigger only if 90% of timeout elapsed
```

#### Worst-case blast radius

The two caps bound the size of one split tree, and they bound it as a
**product**, so reason about them together, not as two independent knobs.
Every split mints at most `max_children` children, one generation deeper
than its parent, and beads that reach `max_depth` are labelled `human`
instead of being split again. One bead whose dispatches keep failing with
the fault attributed to the work can therefore grow into:

```
1 + max_children + max_children² + … + max_children^max_depth
```

At the shipped defaults (`max_children: 8`, `max_depth: 2`) that is
**1 + 8 + 64 = 73 beads** from a single parent. Other shapes, for scale:

| `max_children` | `max_depth` | Worst case |
|---|---|---|
| 8 (default) | 2 (default) | 73 |
| 4 | 2 | 21 |
| 3 | 1 | 4 |
| 8 | 0 (unlimited) | unbounded |

How fast the ceiling is reached: a split mints up to `max_children` beads
in a single cycle — GitHub #18 went from one bead to seven in about 70
seconds — but each *further* generation costs every splitting child at
least one failed dispatch plus one mitosis analysis dispatch, so the full
73 needs at least `1 + max_children` failed dispatches under the default
trigger policy.

Two things lift the arithmetic above:

- **`repeat_interval` (default 0, off)** re-evaluates the *same* depth-0
  parent at later failure counts. Dedup only skips proposed children that
  already exist, so novel children accumulate past `max_children` across
  re-splits, and each of those can mint its own subtree — the per-parent
  ceiling above no longer holds.
- **`max_depth: 0` disables the depth cap entirely.** That is not the
  shipped default; setting it removes the only bound on generations.

To shrink the blast radius, tighten the product — `max_children: 3` with
`max_depth: 1` caps a parent at 4 beads — or disable the strand outright
(`strands.mitosis.enabled: false`).

#### What bounds a runaway today

Stated plainly, as of 2026-09-09 (see `docs/plan/plan.md` revision 32 and
beads `needle-3f82395d`, `needle-0be52050`):

- **Depth and children caps: enforced.** The evaluator refuses a bead at
  or past `max_depth` (labelling it `human` for the escalation ladder) and
  truncates a proposal past `max_children`.
- **Failure attribution: not yet.** The split call site still evaluates on
  any `Outcome::Failure`, and `Outcome::GateUnsatisfiable` exists in the
  taxonomy without a classification path producing it yet
  (`needle-6519f163`, `needle-857df79d`). Until that lands, a gate or
  configuration fault that fails on every dispatch *will* split on its
  first failure, because the trigger cannot tell "the work was too big"
  from "the gate can never pass". A workspace whose gates can be
  unsatisfiable should tighten the cap product or disable the strand until
  attribution gating ships.
- **Child-count warning: declared, not emitted.**
  `child_count_warning_threshold` (default 24 — one third of the shipped
  73 ceiling) and the `bead.mitosis.child_count_warning` event exist, but
  no code emits the event yet (`needle-3f82395d`). Counting is cumulative
  per parent, so it is the tripwire that catches `repeat_interval` growth
  that never breaches `max_children` in a single split.
- **`needle status`** shows the configured mitosis values; it does not yet
  surface live per-parent child counts (`needle-3f82395d`).

#### Trigger policy

The knobs interact — `force_failure_threshold` **replaces** the
first-failure policy rather than adding to it (`src/mitosis/mod.rs`):

- `first_failure_only: true` (default): evaluate exactly at failure count
  1. Retries never re-evaluate, except at `repeat_interval` ticks, and a
  bead carrying a `mitosis-depth:` label is never re-split.
- `first_failure_only: false`: evaluate on *every* failure — strictly more
  eager, never safer.
- `force_failure_threshold: N` (N > 0): evaluate from failure N onward,
  and `first_failure_only` / `repeat_interval` are ignored for as long as
  it is set.

Raising the threshold makes legitimate splits later (a genuinely oversized
bead burns N−1 extra dispatches first) but does **not** bound the
mis-attributed class: a failure that is not the work's fault repeats on
every dispatch by construction and clears any N. That class is bounded by
attribution gating, not by a higher trigger count — which is why
`first_failure_only` stays the shipped default. `timeout_triggered`, when
enabled, fires on a single qualifying timeout regardless of failure count.

### Weave, Unravel, Pulse (Opt-in Strands)

These strands are disabled by default — enable them explicitly:

```yaml
strands:
  weave:
    enabled: false              # Opt-in (default: false)
    max_beads_per_run: 5       # Maximum beads to create per run
    cooldown_hours: 24          # Minimum hours between runs
    doc_patterns:
      - "README*"
      - "AGENTS.md"
      - "docs/**/*"

  unravel:
    enabled: false              # Opt-in (default: false)
    max_beads_per_run: 5
    max_alternatives_per_bead: 3
    cooldown_hours: 168        # 7 days

  pulse:
    enabled: false              # Opt-in (default: false)
    max_beads_per_run: 5
    cooldown_hours: 48
    severity_threshold: 3      # 1-5, where 1 is critical
    scanners:
      - name: clippy
        command: cargo clippy --all-targets -- -D warnings
```

### Reflect and Learning Context (legacy, off by default)

Since N-T15 (plan section 4.4 step 0, ADR-026) the legacy Reflect
consolidator and everything it feeds are **off by default**. Its output —
`.beads/learnings.md`, `.beads/skills/`, decision and drift files — is
count-reinforced transcript scrape, which ADR-026 classifies as candidate
input rather than validated knowledge. A default-configured worker therefore:

- runs no Reflect pass (`strands.reflect.enabled: false`);
- builds prompts with **no** `## Workspace Learnings` or global-learnings
  section (`strands.learning.inject_legacy_learnings: false`);
- never writes to CLAUDE.md, and on boot strips any marker-fenced
  `<!-- needle-learning:* -->` block a previous placer run left there
  (`strands.reflect.claude_md_placement: false`) — text outside the markers
  is never touched;
- caps whatever learning-derived context it does inject (matched skills, or
  the legacy files when opted back in) at
  `strands.learning.max_learning_context_bytes` (default 8192). Configured
  `prompt.context_files` are operator policy and are never truncated.

The files on disk are left in place as the untrusted candidate corpus for the
N-T13 migration. To restore the pre-N-T15 behaviour byte-for-byte:

```yaml
strands:
  reflect:
    enabled: true
    drift_enabled: true
    claude_md_placement: true
  learning:
    inject_legacy_learnings: true
    max_learning_context_bytes: 1000000
```

---

## Telemetry Configuration

NEEDLE emits structured telemetry (logs, metrics, traces) to multiple sinks.

### File Sink (Local Logs)

```yaml
telemetry:
  file_sink:
    enabled: true               # Write logs to files (default: true)
    log_dir: null               # Null = use workspace.home/logs
    retention_days: 30          # Delete logs older than this (default: 30)
```

### Stdout Sink (Console Output)

```yaml
telemetry:
  stdout_sink:
    enabled: false               # Print to stdout (default: false)
    format: normal               # minimal | normal | verbose (default: normal)
    color: auto                 # auto | always | never (default: auto)
```

### OTLP Sink (OpenTelemetry Export)

```yaml
telemetry:
  otlp_sink:
    enabled: false               # Export to OTLP collector (default: false)
    endpoint: "http://localhost:4317"  # gRPC endpoint
    protocol: "grpc"            # grpc | http
    timeout_secs: 10            # Request timeout (default: 10)
    compression: "gzip"          # gzip | none | zstd
    tls:                        # Canonical TLS configuration
      insecure: false           # Disable TLS verification (not recommended for production)
      ca_file: ""               # Path to custom CA certificate (empty = system trust store)
    headers: []                 # e.g. ["Authorization: env:NEEDLE_OTLP_AUTHORIZATION"]
    resource_attributes: []     # Format: "key=value"
    metrics_interval_secs: 10   # Metrics export interval (default: 10)
    service_namespace: "needle-fleet"
```

The mapping above is the canonical representation in v0.3.1 and later. For
upgrade compatibility, legacy `tls: none` and `tls: tls` values are still
accepted and normalize to `{insecure: true, ca_file: ""}` and
`{insecure: false, ca_file: ""}` respectively. Convert those values to the
mapping above when editing a config; unsupported values such as `mtls` produce
a validation error before the worker starts.

If the collector requires authorization, keep the secret out of YAML and use
an environment reference in `headers`, for example
`Authorization: env:NEEDLE_OTLP_AUTHORIZATION`. The environment variable must
contain the complete header value (such as `Bearer <token>`).

### Webhooks (Hooks)

```yaml
telemetry:
  hooks:
    - event_filter: "outcome.*"         # Glob pattern matched against event_type
      command: "/path/to/alert.sh"      # Shell command (event JSON via stdin)
      url: "https://hooks.slack.com/..."  # Webhook URL (optional)
```

---

## Health Configuration

```yaml
health:
  # Heartbeat interval in seconds (default: 30)
  heartbeat_interval_secs: 30

  # Heartbeat TTL in seconds (default: 300)
  # Heartbeats older than this are considered stale
  heartbeat_ttl_secs: 300

  # Directory for heartbeat files (default: state/heartbeats under workspace.home)
  heartbeat_dir: null
```

---

## Validation Configuration

```yaml
validation:
  # Timeout for gate execution in seconds (default: 50)
  outcome_timeout_seconds: 50

  # Maximum bytes of gate command stderr captured on failure (default: 4096)
  stderr_cap_bytes: 4096
```

---

## Stop Configuration

```yaml
stop:
  # Seconds `needle stop` waits for a signalled process tree to drain,
  # polling at a short interval, before naming the PIDs that are still
  # alive (default: 10).
  #
  # Killed workers exit gracefully over a few seconds, so verifying a kill
  # with no grace period reports cleanly-dying processes as orphans. Only
  # processes still alive after this window are reported — and exit non-zero.
  grace_period_secs: 10
```

---

## Attempt Archive Configuration

**Default off. NEEDLE users are not required to run any archive sink.** With
`attempt_archive.enabled: false` (the default) this section has no effect at
all: no spool directory is created and trace retention behaves exactly as it
did before the section existed.

When enabled, NEEDLE bundles each finished dispatch attempt — the agent
transcript captured in the trace directory, the harness's own session
transcript when it can be located, and the exact prompt that was dispatched —
into one archive and writes it to a **local spool directory**. That is the
whole of NEEDLE's involvement: it never uploads, never holds a sink credential,
and never deletes from the spool. Uploading is a separate, optional,
operator-installed drain (`contrib/attempt-archive/`, see
`docs/attempt-archive.md`), and the spool layout is the only contract between
the two. ARMOR is the reference sink in this fleet; any S3-compatible endpoint,
or none, works.

```yaml
attempt_archive:
  # Master switch. false (default) means nothing below has any effect.
  enabled: false

  # Where finished bundles are written for the drain to pick up. NEEDLE only
  # ever writes here; the drain reads and deletes. Tilde is expanded.
  spool_dir: ~/.needle/spool

  # What goes into each bundle.
  include:
    trace: true               # stdout.txt, stderr.txt, trace.jsonl, metadata.json
    harness_transcript: true  # the harness's own session file(s), by session_id
    prompt: true              # the exact BuiltPrompt bytes that were dispatched

  # zstd -> <attempt-id>.tar.zst (default); none -> <attempt-id>.tar
  compression: zstd

  # When true, trace retention may delete an attempt directory once its bundle
  # has been accepted by the spool. When false, retention is unchanged.
  prune_local_after_spool: true
```

**Reload tier:** B (rebuild) for every key — a change takes effect at the next
cycle boundary, no restart.

**Scope:** global only. `attempt_archive` is not overridable from a workspace
`.needle.yaml` (it is listed in the non-overridable keys and produces a warning
there). The spool is a host resource, and whether a host archives at all is an
operator decision, not a repository's.

**Validation:** unknown keys anywhere under `attempt_archive` (including
`include.*`) are rejected when the configuration is loaded — the error names the
offending key — so a typo such as `uplaod:` fails fast instead of silently doing
nothing. The same key paths are accepted by the override key-path validator
(`validate_key_path`).

---

## Supervisor Configuration

```yaml
supervisor:
  # Path to supervisor's heartbeat file
  # (default: workspace.home/state/supervisor-heartbeat.json)
  heartbeat_path: null

  # Path to supervisor's control socket (optional)
  socket_path: null
```

---

## Environment Variable Overrides

Any config field can be overridden via environment variables with the `NEEDLE_` prefix and `__` as separator:

```bash
# Set agent default
export NEEDLE_AGENT__DEFAULT=claude-interactive

# Set timeout
export NEEDLE_AGENT__TIMEOUT=7200

# Set routing
export NEEDLE_AGENT__ROUTING__DEFAULT_ADAPTER=claude

# Set worker limits
export NEEDLE_WORKER__MAX_WORKERS=8
export NEEDLE_WORKER__IDLE_TIMEOUT=120

# Set workspace
export NEEDLE_WORKSPACE__DEFAULT=~/dev/my-project
```

---

## Complete Example Configuration

```yaml
# ~/.config/needle/config.yaml
worker:
  max_workers: 4
  idle_timeout: 60
  launch_stagger_seconds: 2
  scratch_sweep:
    enabled: true
    ttl_hours: 48
  worker_binary_path: /opt/needle/bin/needle  # Override only if needed

agent:
  default: claude
  timeout: 3600
  adapters_dir: ~/.config/needle/adapters
  routing:
    rules:
      - match_model: "(claude-)?(sonnet|opus|fable|haiku).*"
        adapter: claude
    default_adapter: claude
    strict: false

# Put this binding in the repository's .needle.yaml, not only global config.
bead_cli:
  backend: bead-forge

workspace:
  default: ~/dev/my-project
  home: ~/.needle
  labels:
    - rust
    - api

strands:
  pluck:
    exclude_labels: []
    split_after_failures: 3
  explore:
    enabled: true
    workspaces: []
    workspace_root: ~/dev
    scan_interval_cycles: 1
    max_scan_interval_cycles: 8
    rediscovery_cycles: 60
    starvation_threshold_minutes: 15
  mitosis:
    enabled: true
    first_failure_only: true
    force_failure_threshold: 0
    max_depth: 0

telemetry:
  file_sink:
    enabled: true
    retention_days: 30
  stdout_sink:
    enabled: false
  otlp_sink:
    enabled: false

health:
  heartbeat_interval_secs: 30
  heartbeat_ttl_secs: 300

validation:
  outcome_timeout_seconds: 50
  stderr_cap_bytes: 4096

stop:
  grace_period_secs: 10

attempt_archive:
  enabled: false            # default off; see "Attempt Archive Configuration"
  spool_dir: ~/.needle/spool
```

---

## CLI Commands

### `needle doctor` — System Health Check

The `needle doctor` command performs comprehensive system health checks and optionally attempts automatic repairs.

#### Usage

```bash
needle doctor [--workspace <path>] [--repair] [--json]
```

#### Checks Performed

- **Config** — Validates configuration file syntax and structure
- **Workspace** — Checks workspace accessibility and `.beads/` presence
- **JSONL consistency** — Validates `.beads/issues.jsonl` integrity
- **SQLite integrity** — Checks bead store database integrity
- **Lock files** — Detects stale lock files (optional repair)
- **Bead backend** — Verifies bead CLI availability and compatibility
- **Worker registry** — Checks for orphaned worker entries
- **Heartbeat directory** — Validates heartbeat directory permissions
- **Heartbeat files** — Checks heartbeat file staleness
- **Peer status** — Reports active worker peers
- **Agent binary** — Verifies agent adapter binary availability
- **Adapter transforms** — Checks transform binary availability
- **Disk space** — Validates available disk space
- **Telemetry logs** — Checks telemetry log directory health

#### Exit Codes

| Code | Condition |
|------|-----------|
| **0** | All checks passed (or only warnings — warnings never cause non-zero exit) |
| **1** | One or more checks failed |

The exit code rule applies to both normal and `--repair` mode: repairs run first, then the exit code reflects the post-repair state.

#### Output Format

```
NEEDLE Doctor
────────────────────────────────────────────────────────────
[PASS]  Config                       valid
[FAIL]  Bead backend                 binary not found: nonexistent-backend
         └─ bead_cli.backend points to 'nonexistent-backend'
         └─ but no such binary exists on PATH
         └─ fix: cargo install --git https://github.com/jedarden/bead-rs --bin bead
────────────────────────────────────────────────────────────
12 passed, 1 warning(s), 1 failure(s).
Exit code 1: 1 failure(s).
Run `needle doctor --repair` to attempt automatic fixes.
```

A row with a known repair prints the same `fix` command the `--json` row
carries in its `fix` field.

When all checks pass:
```
────────────────────────────────────────────────────────────
13 check(s) passed.
```

#### JSON Output

`--json` replaces the human table with exactly one machine-readable document on
stdout (nothing else is printed there):

```json
{
  "rows": [
    {
      "name": "Bead backend",
      "status": "fail",
      "detail": "binary not found: nonexistent-backend",
      "fix": "cargo install --git https://github.com/jedarden/bead-rs --bin bead"
    }
  ],
  "summary": { "pass": 12, "warn": 0, "fail": 1 },
  "exit_code": 1
}
```

- `status` is `pass`, `warn` or `fail`; `detail` is the human message plus any
  indented detail lines, joined with newlines.
- `fix` is the command that repairs the row, or `null` when there is nothing to
  run. Every `fail` row a machine could act on carries one — bead CLI missing,
  `.beads/` missing, `.needle.yaml` missing, agent binary missing, transform
  binary missing.
- `exit_code` mirrors the process exit code, so a consumer reading either one
  sees the same verdict.

As a CI gate:

```bash
needle doctor --json | jq -e '.summary.fail == 0'
```

#### Use Cases

- **Pre-deploy validation** — Verify system health before starting workers
- **Troubleshooting** — Identify configuration or dependency issues
- **CI/CD gates** — Use exit codes for automated health checks in pipelines
- **Fresh container validation** — Quickstart gate for new deployments (see `needle-e759f7f3`)

## See Also

- [README.md](../README.md) — Project overview and quickstart
- [ADR-009](../docs/adr/009-external-adopter-hardening.md) — Background on `worker_binary_path` and validation configurability
- [Source code: src/config/mod.rs](../src/config/mod.rs) — Full struct definitions and defaults
