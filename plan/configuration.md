# Configuration

NEEDLE uses a hierarchical configuration system. Values are resolved from highest to lowest precedence, with the first defined value winning.

---

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

---

## Global Configuration

**Location:** `~/.needle/config.yaml`

This file controls NEEDLE's behavior across all workspaces. It is the primary configuration file.

```yaml
# ~/.needle/config.yaml

# ──────────────────────────────────────────────────────────────
# Agent Configuration
# ──────────────────────────────────────────────────────────────
agent:
  default: claude-anthropic-sonnet    # agent to use if not specified
  timeout: 600                        # seconds before killing agent (default: 10 min)
  adapters_dir: ~/.needle/agents      # directory for custom adapter YAML files

# ──────────────────────────────────────────────────────────────
# Worker Configuration
# ──────────────────────────────────────────────────────────────
worker:
  max_workers: 20                     # hard ceiling on total workers
  launch_stagger_seconds: 2           # delay between launching workers
  idle_timeout: 300                   # seconds before idle worker exits (0 = never)
  idle_action: wait                   # "wait" (backoff + retry) or "exit"
  max_claim_retries: 5                # retries per claim cycle before moving to next strand
  identifier_scheme: nato             # "nato" (alpha, bravo...) or "numeric" (1, 2, 3...)

# ──────────────────────────────────────────────────────────────
# Workspace Configuration
# ──────────────────────────────────────────────────────────────
workspace:
  default: ~/projects/main            # workspace to use if not specified via --workspace
  home: ~/projects/main               # worker's home workspace (returns here after explore)

# ──────────────────────────────────────────────────────────────
# Strand Configuration
# ──────────────────────────────────────────────────────────────
strands:
  pluck:
    enabled: true                     # always on (cannot be disabled)
    exclude_labels:                   # beads with these labels are skipped
      - deferred
      - human
      - blocked

  explore:
    enabled: true
    workspaces:                       # explicit list of workspaces to explore
      - ~/projects/api-server
      - ~/projects/frontend
      - ~/projects/shared-libs

  mend:
    enabled: true
    stale_claim_ttl: 300              # seconds before releasing stale claims
    lock_ttl: 600                     # seconds before removing orphaned locks
    db_check_interval: 50             # check db health every N beads

  weave:
    enabled: false                    # opt-in
    max_beads_per_run: 5
    cooldown_hours: 24
    exclude_workspaces: []

  unravel:
    enabled: false                    # opt-in
    max_per_run: 3
    cooldown_days: 7

  pulse:
    enabled: false                    # opt-in
    max_beads_per_run: 10
    cooldown_hours: 48
    severity_threshold: warning       # minimum severity to create beads
    scanners: []                      # list of scanner commands

  knot:
    enabled: true                     # always on (cannot be disabled)
    alert_cooldown_minutes: 60
    exhaustion_threshold: 3           # cycles before alerting

# ──────────────────────────────────────────────────────────────
# Concurrency Limits
# ──────────────────────────────────────────────────────────────
limits:
  providers:
    anthropic:
      max_concurrent: 10
      requests_per_minute: 60
    openai:
      max_concurrent: 5
      requests_per_minute: 40
  models: {}                          # per-model overrides (optional)

# ──────────────────────────────────────────────────────────────
# Health Monitoring
# ──────────────────────────────────────────────────────────────
health:
  heartbeat_interval: 30              # seconds between heartbeats
  heartbeat_ttl: 300                  # seconds before heartbeat is stale
  peer_check_interval: 60             # seconds between peer health checks

# ──────────────────────────────────────────────────────────────
# Telemetry
# ──────────────────────────────────────────────────────────────
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

# ──────────────────────────────────────────────────────────────
# Cost Tracking
# ──────────────────────────────────────────────────────────────
pricing: {}                           # per-model token pricing (optional)
budget:
  warn_usd: 0                        # emit warning when daily cost exceeds this (0 = disabled)
  stop_usd: 0                        # stop workers when daily cost exceeds this (0 = disabled)

# ──────────────────────────────────────────────────────────────
# Self-Modification Protection
# ──────────────────────────────────────────────────────────────
protection:
  exclude_workspaces: []              # workspaces where NEEDLE will not process beads
  allow_self_modification: false      # if true, workers can process beads for NEEDLE itself
```

---

## Workspace Configuration

**Location:** `.needle.yaml` in workspace root (next to `.beads/`)

Workspace-level configuration overrides global settings for that specific workspace. Only a subset of settings can be overridden at the workspace level.

```yaml
# .needle.yaml (in workspace root)

agent:
  default: claude-anthropic-opus      # use Opus for this complex project
  timeout: 1200                       # 20 min timeout (complex tasks)

strands:
  weave:
    enabled: true                     # enable gap analysis for this workspace
    max_beads_per_run: 3
  pulse:
    enabled: true                     # enable health scans for this workspace
    scanners:
      - name: rust-clippy
        command: "cargo clippy --message-format=json 2>/dev/null"
      - name: test-coverage
        command: "cargo tarpaulin --skip-clean -o json"

# Workspace-specific prompt additions
prompt:
  context_files:                      # additional files to include in every prompt
    - AGENTS.md
    - docs/architecture.md
  instructions: |                     # additional instructions appended to every prompt
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

---

## Environment Variables

All configuration keys can be overridden via environment variables with the `NEEDLE_` prefix. Nested keys use `__` (double underscore) as separator.

| Config Key | Environment Variable |
|------------|---------------------|
| `agent.default` | `NEEDLE_AGENT__DEFAULT` |
| `agent.timeout` | `NEEDLE_AGENT__TIMEOUT` |
| `worker.max_workers` | `NEEDLE_WORKER__MAX_WORKERS` |
| `strands.weave.enabled` | `NEEDLE_STRANDS__WEAVE__ENABLED` |

Environment variables are primarily useful for:
- CI/CD pipelines where config files aren't available
- Temporary overrides during debugging
- Per-worker customization when launching via scripts

---

## CLI Arguments

CLI arguments have the highest precedence and override all other sources.

```bash
# Override agent
needle run --agent claude-anthropic-opus

# Override workspace
needle run --workspace ~/projects/api-server

# Override timeout
needle run --timeout 1200

# Override worker count
needle run --count 5

# Override identifier
needle run --identifier alpha

# Combined
needle run --workspace ~/projects/api-server --agent claude-anthropic-opus --count 3 --timeout 1200
```

---

## Agent Adapter Configuration

Agent adapters are defined in YAML files in `~/.needle/agents/`. See [agent-adapters.md](agent-adapters.md) for the full specification.

Adapters are loaded by name from this directory. The adapter name in the config (e.g., `claude-anthropic-sonnet`) maps to a file (`~/.needle/agents/claude-anthropic-sonnet.yaml`).

Built-in adapters are embedded in the binary and can be overridden by placing a file with the same name in the adapters directory.

---

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
# Show resolved configuration (all sources merged)
needle config --dump

# Show where each value came from
needle config --dump --show-source

# Example output:
# agent.default: claude-anthropic-sonnet (from: ~/.needle/config.yaml)
# agent.timeout: 1200 (from: /home/coder/project/.needle.yaml)
# worker.max_workers: 20 (from: NEEDLE_WORKER__MAX_WORKERS env var)
# worker.idle_timeout: 300 (from: built-in default)
```
