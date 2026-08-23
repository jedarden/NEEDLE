# NEEDLE Configuration Guide

NEEDLE reads configuration from multiple sources in order of precedence (highest last):

1. Built-in defaults
2. Global config file (`~/.config/needle/config.yaml`)
3. Workspace config file (`.needle.yaml` in workspace root)
4. Environment variables (`NEEDLE_*` prefix)
5. CLI arguments

This guide covers the most commonly used configuration options.

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

```yaml
# ~/.config/needle/config.yaml
agent:
  default: claude              # Default agent CLI (default: "claude")
  timeout: 3600                # Agent process timeout in seconds (default: 3600 = 1 hour)
  args: []                      # Extra arguments passed before the prompt (default: [])

  # Directory containing adapter TOML files (default: ~/.config/needle/adapters)
  adapters_dir: ~/.config/needle/adapters

  # Model-to-adapter routing (optional)
  routing:
    # Rules are evaluated in order; first match wins
    rules:
      - match_model: "(claude-)?(sonnet|opus|fable|haiku).*"
        adapter: claude-print    # Use subscription billing for Anthropic models
    default_adapter: claude-code-glm-4.7  # Fallback for non-matching models
    strict: false                # If true, fail when no rule matches (default: false)
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

# These fields are ignored if set in .needle.yaml:
# - workspace.default (resolved globally)
# - workspace.home (resolved globally)
```

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
    first_failure_only: true     # Only split on first failure (default: true)
    force_failure_threshold: 0  # Force split after N failures (0 = disabled)
    repeat_interval: 0           # Re-split every N failures (0 = disabled)
    max_depth: 0                # Maximum generation depth (0 = unlimited)

    # Timeout-triggered mitosis (opt-in, default: disabled)
    timeout_triggered:
      enabled: false
      agent_wallclock_timeout: false
      handler_timeout: false
      min_elapsed_fraction: 0.9  # Trigger only if 90% of timeout elapsed
```

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
export NEEDLE_AGENT__ROUTING__DEFAULT_ADAPTER=claude-print

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
        adapter: claude-print
    default_adapter: claude-code-glm-4.7
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
```

---

## See Also

- [README.md](../README.md) — Project overview and quickstart
- [ADR-009](../docs/adr/009-external-adopter-hardening.md) — Background on `worker_binary_path` and validation configurability
- [Source code: src/config/mod.rs](../src/config/mod.rs) — Full struct definitions and defaults
