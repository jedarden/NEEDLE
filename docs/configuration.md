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

Not all configuration changes are equal. Changes take effect at three different times:

### Tier A: Live (Takes effect next cycle)

**No rebuild required.** Changes are read from `self.config` and apply immediately on the next worker cycle. No components are reconstructed.

**Effective timing:** Between 0 and `worker.config_reload_check_interval_secs` seconds after the file is saved (default: 0, meaning disabled).

**Keys in this tier:**
- `worker.idle_timeout` — Backoff duration when no work is found
- `worker.idle_action` — What to do when idle (wait or exit)
- `worker.max_claim_retries` — Maximum retry attempts for claim races
- `agent.timeout` — Agent execution timeout (read fresh per dispatch)
- `budget.*` — Cost tracking thresholds (warn_usd, stop_usd)

### Tier B: Rebuild (Component reconstruction)

**Component rebuild required.** Changes trigger reconstruction of the subsystem that owns them. The worker continues running; only the affected component is rebuilt.

**Effective timing:** Next cycle after detection, plus time to rebuild the affected component(s).

**Keys in this tier:**
- `telemetry.*` — Telemetry sinks, OTLP configuration, hooks. **Tracing subscriber is rebuilt.** Turning OTLP on/off takes effect without a restart, but traces from the old configuration remain active until their spans close.
- `strands.*` — All strand settings (enabled thresholds, cooldowns, max counts). **StrandRunner is rebuilt.**
- `prompt.*` — Prompt templates, context files, instructions. **PromptBuilder is rebuilt.**
- `agent.adapters_dir` — Adapter loading path. **Dispatcher is rebuilt.**
- `agent.routing` — Model-to-adapter routing rules. **Dispatcher is rebuilt.**
- `limits.*` — Provider/model concurrency and rate limits. **RateLimiter is rebuilt.**
- `validation.*` — Gate timeouts and output caps. **OutcomeHandler is rebuilt.**

**What happens when a Tier-B component fails to rebuild:** The worker keeps running with the previous instance of that component. A `config.reload.rejected` telemetry event is emitted with the error. The worker does not crash.

### Tier C: Restart-Required (Cannot be applied to a running worker)

**Worker restart required.** These keys are locked at boot time and cannot be changed safely without restarting the process.

**Keys in this tier:**
- `worker.config_reload_check_interval_secs` — The reload mechanism itself cannot be enabled/disabled or reconfigured from within a running worker (a worker started with `0` cannot discover a change that turns it on).
- `worker.identifier_scheme` — Worker identity naming (NATO, custom) — locked into `qualified_id` at boot.
- `workspace.home` — State directory path — heartbeats, logs, and registry paths are resolved once at startup.
- `bead_cli.backend` — Bead store binding — changing mid-session would break in-progress claims and strand state.
- `tokio runtime` configuration (if exposed) — Runtime shape is process-global.
- `tracing-stack` shape (subscriber layers) — The tracing subscriber is installed once at boot; changing what layers are present cannot be done safely mid-process.

**What happens when you change a Tier-C key:** The change is detected, a `config.reload.restart_required` telemetry event is emitted naming the changed keys, and the WARN is logged. The worker continues running with the previous value. You must restart the worker to apply the change.

### How Reload Detection Works

1. **Polling check:** Every `worker.config_reload_check_interval_secs` (default: `0` = disabled), after a bead completes and before the next selection cycle, the worker checks:
   - Global config file mtime + content hash (`~/.config/needle/config.yaml`)
   - Active workspace `.needle.yaml` mtime + content hash
   - Per-section hashing identifies which config subtree changed

2. **Validation:** The candidate config is fully validated before any swap. If invalid, the reload is rejected:
   - `config.reload.rejected` telemetry event emitted
   - WARN logged with validation errors
   - Worker continues on the previous config
   - No retry — fix the config, it will be picked up on the next check

3. **Swap:** All-or-nothing. No half-applied configuration is ever observable.

4. **Component rebuild:** Tier-B components are rebuilt. If a rebuild fails:
   - Component keeps its previous instance
   - `config.reload.component_rebuild_failed` telemetry event emitted
   - Other components proceed with their new instances
   - Worker continues running

5. **Tier-C rejection:** Changes to Tier-C keys emit `config.reload.restart_required` and are not applied.

### Operational Guidance

**Enabling OTLP fleet-wide (the 2026-08-21 use case):**
- Pre-condition: Set `worker.config_reload_check_interval_secs: 30` (or similar) and restart all workers. A worker started with `0` cannot discover the config that would enable polling.
- Apply: Set `telemetry.otlp_sink.enabled: true` in global config
- Result: Within one poll interval (max 30s), all workers rebuild their telemetry writer and begin exporting
- Contrast: Without reload, this required draining all workers (~55 minutes for 15 workers on ex44), releasing their claimed beads, and relaunching.

**Changing strand behavior:**
- Tier B: `strands.pluck.split_after_failures`, `strands.explore.scan_interval_cycles`, `strands.mitosis.*`
- Apply: Edit config, wait ≤ one interval
- Result: StrandRunner is rebuilt; next strand evaluation uses the new thresholds

**Changing worker identity:**
- Tier C: `worker.identifier_scheme` (nato → custom), any change that would alter `qualified_id` construction
- Apply: Worker restart required
- Why: Heartbeat files, registry entries, and telemetry `service.instance.id` all embed `qualified_id`. Changing it mid-session would fragment identity and break peer monitoring.

**Getting the live config:**
- `needle config --dump --show-source` shows the **live** config of a running worker, including a reload generation counter
- This reflects what the worker is actually running, not just what the file says

### See Also

- [ADR-017: Configuration Hot-Reload at the Cycle Boundary](../adr/017-configuration-hot-reload-at-the-cycle-boundary.md) — Full design rationale, implementation details, and failure-mode analysis
- [Phase 18 in plan.md](../docs/plan/plan.md#phase-18-configuration-hot-reload-at-the-cycle-boundary) — Implementation status and exit criteria

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
    endpoint: "http://localhost:4317"  # gRPC endpoint (default: "http://localhost:4317")
    protocol: "grpc"            # grpc | http (default: "grpc")
    timeout_ms: 5000            # Request timeout in milliseconds (default: 5000)
    compression: "gzip"          # gzip | none | zstd (default: "gzip")
    tls:                        # TLS configuration
      insecure: false           # Disable TLS verification (default: false)
      ca_file: ""               # Path to custom CA certificate (default: system trust store)
    headers: []                 # HTTP headers, format: "key: value" (default: [])
    signals:                    # Signal export controls
      traces: true              # Export tracing spans (default: true)
      metrics: true             # Export metrics (default: true)
      logs: true                # Export log records (default: true)
    resource_attributes: []     # Resource attributes, format: "key=value" (default: [])
    metrics_interval_secs: 10   # Metrics export interval in seconds (default: 10)
    service_namespace: "needle-fleet"  # Service namespace for OTel semantic conventions
    max_queue_size: 2048        # Maximum queue size for batch processors (default: 2048)
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
