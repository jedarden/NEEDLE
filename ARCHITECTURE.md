# NEEDLE Architecture

This document describes the internal design of NEEDLE's core subsystems: how they work, how they interact, and what guarantees they provide.

## Table of Contents

1. [Strand Priority Algorithm and Fallthrough](#1-strand-priority-algorithm-and-fallthrough)
2. [Configuration Merge and Override Logic](#2-configuration-merge-and-override-logic)
3. [Event Emission Flow and Guarantees](#3-event-emission-flow-and-guarantees)
4. [Bead Claim Atomicity Guarantees](#4-bead-claim-atomicity-guarantees)
5. [File Locking Semantics and SLAs](#5-file-locking-semantics-and-slas)
6. [Worker Coordination Protocol](#6-worker-coordination-protocol)
7. [Telemetry Pipeline](#7-telemetry-pipeline)
8. [Hook Error Handling Specification](#8-hook-error-handling-specification)

---

## 1. Strand Priority Algorithm and Fallthrough

**Source:** `src/strands/engine.sh`

### Overview

The strand engine is a **priority waterfall dispatcher**. It tries each strand in a fixed order and stops as soon as one reports that it found and processed work.

### Strand Order

| Priority | Strand   | Purpose |
|----------|----------|---------|
| 1        | Pluck    | Primary work from configured workspaces |
| 2        | Explore  | Discover work in other workspaces |
| 3        | Mend     | Maintenance: heartbeat checks, log rotation |
| 4        | Weave    | Create beads from documentation gaps |
| 5        | Unravel  | Propose alternative solutions for blocked beads |
| 6        | Pulse    | Proactive quality monitoring (security, deps, coverage, TODOs) |
| 7        | Knot     | Alert humans when the worker is persistently stuck |

### Return Value Contract

Each strand **must** return one of exactly two values:

- **`0`** – Work found and processed. The engine stops immediately and does not try subsequent strands.
- **`1`** – No work found. The engine falls through to the next enabled strand.

No other return values are defined. Strands that return other values produce undefined engine behavior.

### Enablement Check

Before dispatching to a strand, the engine calls `_needle_is_strand_enabled <strand_name>`. A strand can be in one of three states:

- **`true`** – Always enabled.
- **`false`** – Always disabled (skipped without executing).
- **`auto`** – Enabled/disabled based on the active billing model (see `config/billing_models.sh`).

Disabled strands are recorded as `$strand:disabled` in the `strand_results` diagnostic array.

### Fallthrough Behavior

When a strand returns `1`:

1. The engine emits a `strand.fallthrough` event carrying `from` and `to` strand names.
2. The engine records `$strand:no_work` in diagnostic state.
3. The engine advances to the next **enabled** strand (skipping any disabled ones in between).
4. If all strands return `1`, the engine records starvation diagnostics (enabled count, disabled count, full dispatcher context including workspace, agent, PATH, and `br` availability) and then sleeps before retrying.

---

## 2. Configuration Merge and Override Logic

**Source:** `src/lib/config.sh`

### Precedence (lowest → highest)

1. **Built-in defaults** — JSON blob in `_NEEDLE_CONFIG_DEFAULTS`
2. **Global config** — `~/.needle/config.yaml`
3. **Workspace config** — `<workspace>/.needle.yaml`
4. **Environment variables** — `NEEDLE_*` prefix overrides any file-based value

### Merge Process

```
defaults ──┐
           ├──► deep_merge ──► global result ──┐
global ────┘                                    ├──► deep_merge ──► final config
                                workspace ──────┘
```

The implementation calls `load_workspace_config()` which performs a three-level merge. Keys present at a higher level replace same-named keys at lower levels; nested objects are merged recursively (deep merge), not replaced wholesale.

**Merge backend (in order of preference):**

1. `yq` (YAML/JSON deep merge)
2. Python `PyYAML` (`deep_merge()` recursive function)
3. Basic string substitution (limited; used only when neither of the above is available)

### Accessing Config Values

```bash
get_config "limits.global_max_concurrent"   # dot-notation key access
get_workspace_config "strands.pluck"        # workspace-aware variant
```

### Key Configuration Sections

| Section | Example Keys |
|---------|--------------|
| `billing` | `model` (`pay_per_token` \| `use_or_lose` \| `unlimited`) |
| `limits` | `global_max_concurrent` (default 20), `providers.<name>.max_concurrent` |
| `strands` | `<name>` (`true` \| `false` \| `auto`) |
| `runner` | `polling_interval`, `idle_timeout` (duration strings, e.g. `300s`) |
| `effort.budget` | `daily_limit_usd`, `warning_threshold` |
| `mitosis` | `enabled`, `max_children`, `min_complexity` |
| `select` | `work_stealing_enabled`, `work_stealing_timeout` (default `1800s`) |
| `hooks` | `timeout`, `fail_action` (`warn` \| `abort` \| `ignore`) |
| `file_locks` | `timeout` (default `30m`), `stale_action` |

### Hot-Reload

The config is cached in memory. Every 15 worker loop iterations the runner checks `NEEDLE_CONFIG_CHECK_COUNTER` and calls `reload_config()`, which clears the cache and re-reads from disk. `clear_config_cache()` forces an immediate reload on the next `get_config` call.

---

## 3. Event Emission Flow and Guarantees

**Source:** `src/telemetry/events.sh`

### Event Envelope

All events are written as JSONL (one JSON object per line):

```json
{
  "ts": "2026-03-01T10:00:00.123Z",
  "event": "bead.claimed",
  "level": "info",
  "session": "needle-claude-anthropic-sonnet-alpha",
  "worker": "claude-anthropic-sonnet-alpha",
  "data": { ... }
}
```

### Emission API

```bash
_needle_emit_event <event_type> <level> [key=value ...]
```

- `_needle_telemetry_timestamp` produces ISO 8601 with millisecond precision.
- `_needle_telemetry_infer_level <event_type>` assigns a default level when none is provided:
  - `error.*` → `error`
  - `*.failed`, `*.retry` → `warn`
  - `debug.*` → `debug`
  - everything else → `info`

### Defined Event Types

| Domain | Events |
|--------|--------|
| Strand | `strand.started`, `strand.completed`, `strand.fallthrough`, `strand.skipped` |
| Bead   | `bead.claimed`, `bead.released`, `bead.claim_retry`, `bead.claim_exhausted` |
| Execution | `execution.started`, `execution.completed`, `execution.failed` |
| Heartbeat | `heartbeat.emitted` (every 30 s during idle) |
| Budget | `budget.warning` (at threshold), `budget.exceeded` |
| Knot   | `knot.false_positive_prevented`, `knot.db_corruption_false_positive_prevented` |

### Guarantees

| Property | Behavior |
|----------|----------|
| **Synchronous write** | Events are flushed to `.needle/logs/events.jsonl` before the emitter returns |
| **Non-blocking on failure** | A write failure is logged to stderr but does not block or crash the worker |
| **Session tagging** | Every event carries `NEEDLE_SESSION` (format: `needle-<runner>-<provider>-<model>-<identifier>`) |
| **Ordering** | ISO 8601 millisecond timestamps establish a total ordering within a session |
| **FABRIC forwarding** | If `fabric.enabled`, events are also forwarded via a named pipe to an HTTP endpoint (non-blocking, background process) |

---

## 4. Bead Claim Atomicity Guarantees

**Source:** `src/bead/claim.sh`

### Claim Protocol

```bash
_needle_claim_bead --workspace <ws> --actor <actor> [--max-retries <n>]
```

Internally this calls:

```bash
br update <bead_id> --claim --actor <actor>
```

The `br` CLI executes this as a single **SQLite transaction** with `EXCLUSIVE` isolation. Only one concurrent caller can succeed; all others get exit code `4` (race condition).

### Retry Logic

| Step | Action |
|------|--------|
| Race condition (exit 4) | Selects a different bead from the queue; retries atomically |
| Max retries exceeded | Emits `bead.claim_exhausted`; returns `1` (no work) |

Default max retries: `NEEDLE_CLAIM_MAX_RETRIES` (default `5`).

### Weighted Bead Selection

Before each claim attempt, `_needle_select_bead` chooses a candidate using weighted random selection:

| Priority | Weight |
|----------|--------|
| P0 | 10× |
| P1 | 5× |
| P2 | 2× |
| P3+ | 1× |

Selection uses cumulative distribution sampling and respects dependency constraints (beads blocked by open dependencies are excluded).

### Hook Integration

- **`pre_claim` hook** — called before the claim attempt.
  - Exit `2` → abort claim entirely.
  - Exit `3` → skip this bead (try next).
- **`post_claim` hook** — called after a successful claim.

Hooks fire within the atomic claim window so that skips and aborts are consistent with the database state.

### Release Mechanism

`_needle_release_bead` clears `status='open'`, `assignee=NULL`, `claimed_by=NULL`, `claim_timestamp=NULL` directly via (in preference order):

1. `sqlite3` CLI
2. Python `sqlite3` module
3. `br` CLI (fallback; works around a known CHECK constraint bug in `br` that prevents setting `status='open'` while `claimed_by` is still set)

### Unassigned-by-Default

New beads created by `_needle_create_bead` are auto-assigned to the creator then **immediately released** when `select.unassigned_by_default: true` (default). This prevents starvation when a single worker creates many beads. Exceptions:

- `human`-type beads remain assigned so they are visible to the human.
- Beads created with an explicit `--assignee` flag remain assigned.

---

## 5. File Locking Semantics and SLAs

**Source:** `src/lock/checkout.sh`

### Design Philosophy

- **Non-blocking** — Workers never wait for a lock. If a conflict is detected, a dependency bead is created and the current bead is deferred.
- **Self-healing** — Closing a bead releases all its file claims automatically.
- **Cross-workspace** — All NEEDLE workers on the same machine share a single lock namespace.
- **Volatile** — Locks live in `/dev/shm/needle` (RAM-backed tmpfs); they are automatically cleaned up on reboot.

### Lock File Location and Naming

```
/dev/shm/needle/{bead-id}-{path-uuid}
```

`path-uuid` = first 8 characters of the MD5 hash of the absolute file path.

### Lock File Contents

```json
{
  "bead": "nd-2ov",
  "worker": "claude-code-glm-5-alpha",
  "path": "/home/coder/NEEDLE/src/cli/run.sh",
  "type": "write",
  "ts": 1709337600,
  "workspace": "/home/coder/NEEDLE"
}
```

### SLA / Timeout Behavior

| Config Key | Default | Meaning |
|------------|---------|---------|
| `file_locks.timeout` | `30m` | Age at which a lock is considered stale |
| `file_locks.stale_action` | `warn` | `warn` \| `release` \| `ignore` |

Stale locks are detected during checkout and logged. `stale_action: release` will delete the stale lock file and allow the new claim to proceed. `stale_action: warn` logs the staleness but still blocks the new claim (creating a dependency bead).

### Checkout Flow

1. `_needle_lock_ensure_dir` — creates `/dev/shm/needle` atomically if missing.
2. `_needle_lock_path_uuid <filepath>` — computes the 8-char hash identifier.
3. `_needle_lock_file_path <bead_id> <filepath>` — builds the full lock file path.
4. Inspect existing lock (if any): check `ts` against timeout; apply `stale_action`.
5. `_needle_lock_write_info` — atomically writes lock JSON with current timestamp.
6. On conflict: emit telemetry event, create a dependency bead, defer current bead.

---

## 6. Worker Coordination Protocol

**Source:** `src/runner/loop.sh`, `src/runner/state.sh`, `src/runner/limits.sh`

### Worker Lifecycle

```
Start
  └─► Register (state/workers.json)
        └─► Heartbeat loop (30 s)
              └─► [Strand Engine → Claim → Execute → Record]
                    └─► Graceful Shutdown (drain current bead, unregister)
```

### Registration

`_needle_register_worker` writes an entry to `~/.needle/state/workers.json` using `flock`-based atomic updates. Fields include: session ID, runner, provider, model, identifier, PID, workspace, start time. A duplicate-session check prevents double-registration.

Session ID format: `needle-<runner>-<provider>-<model>-<identifier>`

### Heartbeat Protocol

Each worker maintains a heartbeat file at:

```
~/.needle/state/heartbeats/${NEEDLE_SESSION}.json
```

Updated every **30 seconds** (configurable). Structure:

```json
{
  "worker": "needle-claude-anthropic-sonnet-alpha",
  "pid": 12345,
  "started": "2026-03-01T10:00:00Z",
  "last_heartbeat": "2026-03-01T10:02:15Z",
  "status": "executing",
  "current_bead": "nd-123",
  "bead_started": "2026-03-01T10:02:00Z",
  "strand": 1,
  "workspace": "/home/coder/NEEDLE",
  "agent": "claude-anthropic-sonnet",
  "queue_depth": 1
}
```

`status` values: `idle`, `executing`, `draining`, `starting`.

### Concurrency Enforcement

Limits are checked before a worker begins processing a bead. Three independent tiers are applied:

| Tier | Config Key | Default |
|------|-----------|---------|
| Global | `limits.global_max_concurrent` | 20 |
| Provider | `limits.providers.<name>.max_concurrent` | 10 |
| Model/Agent | `limits.models.<agent>.max_concurrent` | — |

Worker counts are derived from active entries in `workers.json`.

### Rate Limiting

A sliding-window rate limiter tracks requests per provider:

- State file: `~/.needle/state/rate_limits/{provider}.json` — array of ISO 8601 request timestamps.
- Window: last 60 seconds.
- Config: `limits.providers.<provider>.requests_per_minute`.
- On each request: prune timestamps outside the window, count remaining, enforce limit.

### Graceful Shutdown

`SIGTERM`, `SIGINT`, and `SIGHUP` all trigger drain mode:

1. Worker finishes its current bead normally.
2. Worker does not pick up any new beads.
3. `_needle_unregister_worker <session>` removes the entry from `workers.json`.
4. A 5-second grace period allows background processes to clean up.

### Backoff and Crash Recovery

Persistent failures trigger exponential backoff:

| Failure count | Backoff delay |
|---------------|---------------|
| < 3 | None |
| 3–4 | 30 s |
| 5–6 | 60 s |
| > 6 | 120 s (max) |

At threshold `5`, a warning is emitted. At `7` consecutive failures the worker exits. `NEEDLE_FAILURE_COUNT` resets to `0` after any successful bead completion.

---

## 7. Telemetry Pipeline

**Source:** `src/telemetry/events.sh`, `src/telemetry/tokens.sh`, `src/telemetry/budget.sh`, `src/telemetry/effort.sh`, `src/telemetry/fabric.sh`, `src/dashboard/server.py`, `src/cli/dashboard.sh`

### Four-Tier Architecture

```
Worker action
    │
    ▼
Tier 1: Event Emission        ─── JSONL ──► ~/.needle/logs/events.jsonl
    │                                              │
    ▼                                          (optional)
Tier 2: Token & Cost Tracking ─── JSONL ──► ~/.needle/logs/effort.jsonl
    │
    ▼
Tier 3: Budget Enforcement    ─── blocks worker if limit exceeded
    │
    ▼
Tier 4: FABRIC Forwarding     ─── HTTP ──► POST /ingest (async, non-blocking)
                                                  │
                                                  ▼
Tier 5: FABRIC Dashboard      ─── SSE  ──► browser (needle dashboard start)
```

### Tier 1 — Event Emission

All structured events are appended to `$NEEDLE_LOG_FILE` (default `~/.needle/logs/events.jsonl`) synchronously. See [Section 3](#3-event-emission-flow-and-guarantees) for the full event schema and API.

### Tier 2 — Token and Cost Tracking

`_needle_extract_tokens_json <output_file>` parses AI model output and returns `"input_tokens|output_tokens"`. It handles multiple JSON response shapes:

- `{input_tokens, output_tokens}`
- `{usage: {input_tokens, output_tokens}}`
- `{usage: {prompt_tokens, completion_tokens}}`
- `{tokens: {input, output}}`

Returns `"0|0"` when no token data is found.

**Cost formula:**

```
cost = (input_tokens / 1000) × input_per_1k  +  (output_tokens / 1000) × output_per_1k
```

Per-agent rates are defined in `config/agents/*.yaml`:

```yaml
cost:
  type: pay_per_token
  input_per_1k: 0.003
  output_per_1k: 0.015
```

Each bead's token usage and cost are recorded as a JSONL entry in `~/.needle/logs/effort.jsonl`.

### Tier 3 — Budget Enforcement

| Config Key | Default | Meaning |
|------------|---------|---------|
| `effort.budget.daily_limit_usd` | `50.0` | Hard daily spend ceiling |
| `effort.budget.warning_threshold` | `0.8` | Fraction of limit that triggers a warning |

Before each bead, `get_daily_spend()` sums today's `effort.jsonl` entries and compares against the limit:

- At **80%** of limit: emit `budget.warning` event (worker continues).
- At **100%** of limit: emit `budget.exceeded` event; worker stops accepting new beads.

Enforcement is modulated by billing model:

| Model | Behavior |
|-------|----------|
| `pay_per_token` | Strict enforcement at 100% |
| `use_or_lose` | Soft warning at threshold; loose enforcement |
| `unlimited` | No enforcement |

### Tier 4 — FABRIC Forwarding

When `fabric.enabled: true`, events are forwarded in real time to an external HTTP endpoint:

1. A background process opens a named pipe at `/tmp/needle-fabric-{pid}.pipe`.
2. `_needle_emit_event` writes each event to the pipe (non-blocking write).
3. The background forwarder reads from the pipe and POSTs JSONL batches to `fabric.endpoint`.
4. If the endpoint is unreachable, the forward fails silently; local JSONL files are unaffected.

Config keys: `fabric.enabled`, `fabric.endpoint`, `fabric.timeout`.

### Tier 5 — FABRIC Dashboard (consumer)

**Source:** `src/dashboard/server.py`, `src/cli/dashboard.sh`

The FABRIC Dashboard is the consumer side of the telemetry pipeline: a standalone Python HTTP server that receives forwarded events and serves a live browser UI. It requires no external services — the server and dashboard are self-contained.

**Architecture:**

```
NEEDLE workers
  → fabric.sh: POST /ingest → Dashboard server (src/dashboard/server.py)
                                   │
                          in-memory ring buffer (last 10k events)
                                   │
                    ┌──────────────┴───────────────┐
                    │                               │
             GET /stream (SSE)              GET / (dashboard HTML)
                    │
              browser clients
```

**Endpoints:**

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/ingest` | POST | Receive a JSONL event from fabric.sh |
| `/stream` | GET | SSE stream — pushes new events to connected browsers |
| `/` | GET | Self-contained dashboard HTML (no CDN, no build step) |
| `/api/summary` | GET | Aggregate stats: active workers, beads/min, cost today, failures |
| `/api/costs` | GET | Per-bead cost breakdown from `effort.recorded` events (sorted desc) |
| `/api/throughput` | GET | 30-minute bead completion history for the sparkline |
| `/api/events` | GET | Last N buffered events |
| `/health` | GET | Liveness probe: `{"status": "ok", "uptime": "..."}` |

**Dashboard panels:**

- **Active workers** — name, current bead, elapsed time, tokens this session
- **Strand activity** — per-strand run counts and last-run timestamps
- **Bead throughput** — completions/min over time (30-minute sparkline)
- **Cost tracker** — daily spend vs. budget, per-worker and per-bead breakdown
- **Recent events** — live tail of the last 50 events with type-based coloring
- **Failure alerts** — highlighted rows for `bead.failed` and `budget.warning` events

**CLI integration (`needle dashboard`):**

```bash
needle dashboard start              # Start in background (daemonized via nohup)
needle dashboard start --foreground # Start in foreground
needle dashboard start --port 3000  # Custom port
needle dashboard start --open       # Start and open browser
needle dashboard status             # Check liveness (reads PID file + kill -0)
needle dashboard stop               # Graceful shutdown (SIGTERM, then SIGKILL after 10s)
needle dashboard restart            # stop + start
needle dashboard logs [--follow]    # Tail ~/.needle/logs/dashboard.log
```

PID file: `~/.needle/dashboard.pid`. Log file: `~/.needle/logs/dashboard.log`.

**Startup seeding:**

Pass `--seed-file <events.jsonl>` to pre-populate the ring buffer from an existing log.
The seeding pass also updates the throughput sparkline for historical `bead.completed` events.

**Configuration:**

```yaml
dashboard:
  port: 7842        # Default port
  host: localhost   # Bind address; use "0.0.0.0" for Tailscale/remote access

fabric:
  enabled: true
  endpoint: http://localhost:7842/ingest
```

### File Layout

```
~/.needle/logs/
├── events.jsonl              # All structured events (append-only)
├── effort.jsonl              # Token usage and cost per bead
├── heartbeats/
│   └── {session}.json        # Per-worker heartbeat state
└── rate_limits/
    └── {provider}.json       # Sliding-window rate limit state
```

### Telemetry Guarantees

| Property | Behavior |
|----------|----------|
| **Durability** | JSONL entries are flushed to disk synchronously |
| **Ordering** | ISO 8601 millisecond timestamps; monotonic within a session |
| **No loss on write failure** | Failed writes log to stderr but do not propagate errors |
| **Non-blocking forwarding** | FABRIC pipe writes are non-blocking; slow endpoints don't stall workers |
| **No PII** | Bead IDs and file paths are the only identifiers; no user data is recorded |

---

## 8. Hook Error Handling Specification

**Source:** `src/hooks/runner.sh`, `src/hooks/validate.sh`

### Overview

NEEDLE exposes 11 lifecycle hook points where users can run custom scripts. This section specifies exactly how hook failures are handled, what timeout behavior means, and how hook outcomes propagate into the bead lifecycle.

### Hook Points and When They Fire

| Hook | Fires when | Blocking? |
|------|-----------|-----------|
| `pre_claim` | Before a bead is claimed | Yes — abort or skip affects claim |
| `post_claim` | After a successful claim | No — failure only logs |
| `pre_execute` | Before bead execution begins | Yes — abort stops execution |
| `post_execute` | After execution completes (success or failure) | No — failure only logs |
| `pre_complete` | Before a bead is marked completed | Yes — abort keeps bead open |
| `post_complete` | After a bead is marked completed + locks released | No |
| `on_failure` | When bead execution fails | Yes — abort skips retry, goes to quarantine |
| `on_quarantine` | When a bead enters quarantine state | No — bead already quarantined |
| `pre_commit` | Before a git commit is made | Yes — abort stops the commit |
| `post_task` | After the full task cycle completes | No |
| `error_recovery` | When an unhandled error is caught | No — advisory only |

### Exit Code Contract

All hook scripts communicate outcome via exit code:

| Exit Code | Constant | Meaning | Effect |
|-----------|----------|---------|--------|
| `0` | `NEEDLE_HOOK_EXIT_SUCCESS` | Hook succeeded | Continue normally |
| `1` | `NEEDLE_HOOK_EXIT_WARNING` | Non-fatal issue | Log warning, continue |
| `2` | `NEEDLE_HOOK_EXIT_ABORT` | Hard stop | Abort current operation (see per-hook semantics) |
| `3` | `NEEDLE_HOOK_EXIT_SKIP` | Skip signal | Runner returns `2`; caller skips to next bead |
| `124` | `NEEDLE_HOOK_EXIT_TIMEOUT` | Timed out | Handled per `fail_action` config |
| Other | — | Unexpected failure | Handled per `fail_action` config |

Exit codes `2` (abort) and `3` (skip) are only meaningful for **blocking** hooks (see table above). Non-blocking hooks receive the same exit codes but the runner converts them to warnings rather than propagating the abort.

### Timeout Behavior

The `hooks.timeout` config key (default `30s`) caps how long any hook may run. The `timeout` command enforces this; if the hook process does not exit within the limit, it receives `SIGTERM` and exit code `124` is returned.

What happens after a timeout depends on `hooks.fail_action`:

| `fail_action` | Timeout outcome |
|---------------|----------------|
| `warn` (default) | Log warning, return success to caller — execution continues |
| `abort` | Return failure to caller — current operation is aborted |
| `ignore` | Silently continue — same as `warn` but without the log message |

Timeout is **not retried**. The hook invocation is abandoned and the action proceeds according to `fail_action`.

### `fail_action` — General Failure Policy

`hooks.fail_action` also governs how **unexpected exit codes** (anything other than 0–3) are handled:

| `fail_action` | Non-zero, non-abort exit code |
|---------------|-------------------------------|
| `warn` | Log warning, return success — caller continues |
| `abort` | Emit `hook.failed` event, return failure — caller aborts |
| `ignore` | Silently continue |

This policy does **not** affect exit code `2` (abort): that always propagates as a failure regardless of `fail_action`.

### Error Propagation to Bead Lifecycle

#### `on_failure` hook

Called when bead execution exits with a non-zero code or an unhandled error occurs.

- **Exit `0` or `1`**: Standard failure handling proceeds — the bead is eligible for retry according to the worker's backoff schedule.
- **Exit `2` (abort)**: Retry is skipped. The bead is moved directly to quarantine, triggering the `on_quarantine` hook.
- **Exit `3` (skip)**: Treated the same as exit `0` — standard retry logic applies.
- **Timeout**: Handled per `fail_action`; on `abort`, same effect as exit `2`.

File locks held by the bead are released regardless of `on_failure` exit code (the lock-release call in `_needle_hook_on_failure` always runs).

#### `on_quarantine` hook

Called when a bead enters quarantine state (either from explicit quarantine action, repeated failures, or `on_failure` exit `2`).

- **Exit `0` or `1`**: Quarantine proceeds normally — bead is left in quarantine awaiting human intervention.
- **Exit `2` (abort)**: Ignored. The bead is already quarantined and the abort has no further effect.
- **Exit `3` (skip)**: Remaining `on_quarantine` hooks for this event are skipped; quarantine state is preserved.

#### Retry vs. Quarantine Decision Matrix

| Condition | Result |
|-----------|--------|
| `on_failure` exits `0` or `1` | Bead retried per backoff schedule |
| `on_failure` exits `2` | Bead quarantined immediately (no retry) |
| `on_failure` exits `3` | Bead retried (skip treated as success) |
| `on_failure` times out + `fail_action=abort` | Bead quarantined |
| `on_failure` times out + `fail_action=warn` | Bead retried |
| Worker consecutive failures ≥ 7 | Worker exits (see Section 6) |

### Hook Invocation Guarantees

1. **At-most-once per event**: A hook fires once per lifecycle event. There is no automatic retry of the hook itself.
2. **Non-recursive**: Hooks cannot trigger additional lifecycle hooks.
3. **Environment isolation**: Each hook runs in a subprocess. Variables exported by the hook do not affect the parent worker.
4. **Lock release on close**: `post_complete` and `on_failure` both call `_needle_release_bead_locks_on_close`. File locks are always released when a bead terminal state is reached, even if the hook itself fails.
5. **No hook chaining**: Each hook point supports exactly one script path. To run multiple actions, the single script must orchestrate them.

### Configuration Reference

```yaml
hooks:
  timeout: 30s            # Max runtime per hook invocation (default: 30s)
  fail_action: warn       # What to do on unexpected failure: warn | abort | ignore
  pre_claim:    ~/.needle/hooks/pre-claim.sh
  post_claim:   ~/.needle/hooks/post-claim.sh
  pre_execute:  ~/.needle/hooks/pre-execute.sh
  post_execute: ~/.needle/hooks/post-execute.sh
  pre_complete: ~/.needle/hooks/pre-complete.sh
  post_complete: ~/.needle/hooks/post-complete.sh
  on_failure:   ~/.needle/hooks/on-failure.sh
  on_quarantine: ~/.needle/hooks/on-quarantine.sh
  pre_commit:   ~/.needle/hooks/pre-commit.sh
  post_task:    ~/.needle/hooks/post-task.sh
  error_recovery: ~/.needle/hooks/error-recovery.sh
```

Workspace-level config (`.needle.yaml` in the workspace root) overrides global config for all hook settings. This allows per-project hook scripts without affecting other workspaces.

### Telemetry Events

The hook runner emits structured events for observability:

| Event | When emitted |
|-------|-------------|
| `hook.started` | Before hook script is executed |
| `hook.completed` | After hook exits `0`, `1`, or `3` |
| `hook.failed` | After hook exits `2` or exceeds timeout with `fail_action=abort` |

All events carry `hook_name`, `bead_id`, `exit_code`, and `duration_ms` fields.
