# Telemetry

Every state transition, claim attempt, dispatch, and outcome emits structured telemetry. A silent worker is a broken worker. This document specifies the telemetry system.

---

## Design Principles

1. **Structured from origin.** Events are typed structs, not log strings. They are serialized to JSONL for storage and consumption. There is no string parsing.

2. **Separate from agent output.** Telemetry is written to NEEDLE's own sinks. It is never interleaved with agent stdout/stderr. This eliminates the stdout corruption bug class from v1 (see `docs/notes/bash-at-scale-problems.md`).

3. **Non-blocking.** Telemetry emission never blocks the worker loop. If a sink is slow or failing, events are buffered and dropped after a threshold, not retried.

4. **Complete.** Every state transition produces an event. If you reconstruct events for a worker, you can replay its entire session.

---

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

---

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

---

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

---

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

---

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

## Configuration

```yaml
telemetry:
  file_sink:
    enabled: true                   # always on
    directory: ~/.needle/logs
    rotation: session               # "session" or "size"
    max_size_mb: 100                # per file, if rotation=size
    retention_days: 30              # auto-delete old logs
  stdout_sink:
    enabled: false                  # enable for interactive use
    format: normal                  # minimal | normal | verbose
    color: auto                     # auto | always | never
  hooks: []                         # see Hook Sink section
  effort:
    track_tokens: true              # attempt token extraction
    track_cost: true                # estimate cost from tokens
```
