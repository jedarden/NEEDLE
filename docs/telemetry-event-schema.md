# Telemetry Event Schema

This document describes the internal telemetry events emitted by NEEDLE for observability, monitoring, and operational insight.

**Unlike agent events** (documented in [`agent-event-schema.md`](./agent-event-schema.md)), telemetry events are emitted by NEEDLE itself to track internal operations like worker lifecycle, bead processing, upgrade checks, and system health.

The canonical Rust types live in `src/telemetry/mod.rs` as the `EventKind` enum.

---

## Envelope

All telemetry events share these top-level fields (mapped from `EventKind` to `TelemetryEvent`):

| Field            | Type    | Description                                                  |
|------------------|---------|--------------------------------------------------------------|
| `event_type`     | string  | Dot-joined event name (e.g., `"upgrade_check.started"`).    |
| `timestamp`      | string  | ISO 8601 timestamp in UTC (e.g., `"2026-08-28T12:34:56Z"`). |
| `needle_version` | string  | NEEDLE version emitting the event.                          |

Additional fields are event-specific and documented per type below.

---

## Event Categories

### Worker Lifecycle
- `worker.booting` — Worker process starting
- `worker.started` — Worker ready to process beads
- `worker.stopped` — Worker shut down (normal or error)
- `worker.errored` — Worker encountered a fatal error
- `worker.exhausted` — Worker exhausted all available work
- `worker.idle` — Worker entering idle backoff
- `worker.idle_sleep_entered` — Worker starting idle sleep
- `worker.idle_sleep_completed` — Worker woke from idle sleep
- `worker.found_but_excluded` — No claimable beads after exclusions
- `worker.event_driven_wakeup` — Worker woke due to workspace mtime change
- `worker.launch.deferred` — Worker launch delayed due to resource saturation
- `worker.admission_blocked` — Launch admission refused: host above CPU/memory policy; worker holds resident
- `worker.admission_restored` — Launch admission recovered; emitted before selection resumes
- `worker.boot.timeout` — Worker initialization exceeded timeout

### State Transitions
- `worker.state_transition` — Worker moved between states

### Strand Evaluation
- `strand.evaluated` — A strand evaluation completed
- `strand.skipped` — A strand was skipped (e.g., due to config)
- `strand.resolve.evaluated` — Resolve strand evaluation
- `strand.pluck.no_candidate` — Pluck found no local candidate; diagnostic only, waterfall continues
- `strand.knot.starvation_detected` — terminal starvation verdict after the full strand waterfall
- `strand.pluck.starvation_detected` — legacy compatibility event; no longer emitted by strand selection

### Bead Processing
- `bead.claim.attempted` — Worker attempted to claim a bead
- `bead.claim.succeeded` — Bead claim succeeded
- `bead.claim.race_lost` — Bead claim lost to another worker
- `bead.claim.race_lost_skipped` — Skipped retry due to race loss
- `bead.claim.failed` — Bead claim failed (non-race error)

### Bead Store Errors
- `bead_store.error` — Bead store operation failed

### Attempt Ledger
- `attempt.resolved` — Terminal ledger row for one dispatch (one per dispatch, N-T16)

### Provider Health (N-T23)
- `provider.degraded` — One non-gate failure fingerprint (`exit_code:124`,
  `terminal_reason=api_error api_error_status=503`, `signal:9`) dominates an
  adapter's recent failures across several distinct beads. Fields: `adapter`,
  `fingerprint`, `summary`, `failures`, `distinct_beads`, `bead_id`. From this
  point failures carrying the fingerprint resolve as `infrastructure_failure`
  (ledger `terminal_reason: provider_degraded:<fingerprint>`), release the bead
  with no failure count, and workers on the adapter hold before claiming for
  `workspace_health.adapter_degraded_cooldown_secs` after the last such failure.
- `provider.restored` — A degraded adapter produced a verified success again.
  Fields: `adapter`, `bead_id`, `degraded_duration_secs`.

### Memory Exposure (plan 4.4 step 7)
- `prompt.memory_retrieved` — Prior fixes were retrieved for a retry and
  injected into its prompt. Fields: `bead_id`, `attempt`, `ids` (the exact
  retrieved entries the attempt saw — the exposure record), `bytes`.

### Configuration
- `config.warning` — Configuration validation warning

### Worker Lifecycle Steps
- `init.step.started` — Initialization step started
- `init.step.completed` — Initialization step completed

### Upgrade Checks
- `upgrade_check.started` — Upgrade check initiated
- `upgrade_check.completed` — Upgrade check completed successfully
- `upgrade_check.failed` — Upgrade check failed

### Canary Testing
- `canary.started` — Canary test suite started
- `canary.suite_completed` — Canary test suite completed
- `canary.promoted` — Canary binary promoted to stable
- `canary.rejected` — Canary binary rejected after failures

### Worker Upgrade Detection
- `worker.upgrade.detected` — Worker detected a new stable binary
- `worker.upgrade.completed` — Worker successfully re-exec'd into new binary

### Process Spawning
- `spawn.launched` — Child process launched successfully
- `spawn.failed` — Child process launch failed
- `spawn.exited` — Child process exited (with exit code)
- `spawn.terminated` — Child process terminated by signal

### Cargo Tests
- `cargo_test.started` — Cargo test run started
- `cargo_test.completed` — Cargo test run completed

### Process Guard Violations
- `process_guard.violation` — Process guard constraint violated
- `process_guard.exhausted` — All process guard retries exhausted

### Spawn Path Integrity
- `spawn_path.modified_in_place` — Spawn-path binary was modified in-place without re-exec

---

## Detailed Event Schemas

### Worker Lifecycle Events

#### `worker.booting`
Emitted when a worker process begins initialization.

| Field           | Type   | Description                              |
|-----------------|--------|------------------------------------------|
| `worker_name`   | string | Worker identifier (e.g., `"alpha"`).      |
| `version`       | string | NEEDLE version.                          |

#### `worker.started`
Emitted when a worker completes initialization and enters the main loop.

| Field           | Type   | Description                              |
|-----------------|--------|------------------------------------------|
| `worker_name`   | string | Worker identifier.                       |
| `version`       | string | NEEDLE version.                          |

#### `worker.stopped`
Emitted when a worker shuts down (normal termination, exhaustion, or manual stop).

| Field             | Type   | Description                                    |
|-------------------|--------|------------------------------------------------|
| `reason`          | string | Shutdown reason (e.g., `"exhausted"`, `"manual"`). |
| `beads_processed` | number | Total beads processed during session.           |
| `uptime_secs`     | number | Session uptime in seconds.                     |

#### `worker.errored`
Emitted when a worker encounters a fatal error and exits.

| Field             | Type   | Description                                    |
|-------------------|--------|------------------------------------------------|
| `error_type`      | string | Error category (e.g., `"panic"`, `"io"`).     |
| `error_message`   | string | Human-readable error description.             |
| `beads_processed` | number | Beads processed before failure.               |

#### `worker.exhausted`
Emitted when a worker exhausts all available work in the configured strands.

| Field                | Type         | Description                                                        |
|----------------------|--------------|--------------------------------------------------------------------|
| `cycle_count`        | number       | How many waterfall cycles completed.                                |
| `last_strand`        | string       | Name of the last strand evaluated.                                 |
| `waterfall_restarts` | number       | How many times the waterfall restarted.                            |
| `restart_triggers`   | string array | Names of strands that triggered each restart.                      |
| `strand_evaluations` | array        | All strand evaluations: `[{strand, result, duration_ms}, ...]`. |

#### `worker.idle`
Emitted when a worker enters idle state (no beads to process).

| Field              | Type   | Description                           |
|--------------------|--------|---------------------------------------|
| `backoff_seconds` | number | Idle backoff duration in seconds.     |

#### `worker.idle_sleep_entered`
Emitted when a worker begins idle sleep.

| Field              | Type   | Description                                    |
|--------------------|--------|------------------------------------------------|
| `backoff_secs`    | number | Sleep duration in seconds.                     |
| `beads_processed` | number | Beads processed before idle.                   |
| `uptime_secs`     | number | Worker uptime in seconds.                      |

#### `worker.idle_sleep_completed`
Emitted when a worker wakes from idle sleep.

| Field              | Type   | Description                                    |
|--------------------|--------|------------------------------------------------|
| `backoff_secs`    | number | Configured sleep duration.                     |
| `elapsed_secs`    | number | Actual sleep duration.                         |
| `shutdown_checks` | number | How many shutdown checks performed during sleep. |

#### `worker.event_driven_wakeup`
Emitted when a worker wakes early due to workspace modification time change.

| Field              | Type   | Description                                            |
|--------------------|--------|--------------------------------------------------------|
| `workspace`        | string | Workspace path that triggered wakeup.                 |
| `mtime_age_secs`   | number | Age of workspace mtime in seconds.                     |

#### `worker.launch.deferred`
Emitted when worker launch is delayed due to resource saturation (CPU/memory limits).

| Field              | Type   | Description                                            |
|--------------------|--------|--------------------------------------------------------|
| `deferred_count`   | number | How many times this worker was deferred.              |
| `total_wait_secs` | number | Total wait time across all deferrals.                  |
| `reason`           | string | Why deferred (e.g., `"cpu_limit"`).                    |

#### `worker.admission_blocked`
Emitted when launch admission is refused because the host is above CPU or
memory policy (plan revision 24 §4.6, N-T33). The worker enters the
`ADMISSION_BLOCKED` state and holds resident: it claims no bead, never reads
as idle, and retries with capped jittered backoff instead of exiting into a
service restart.

The event fires when the hold begins and then rate-bounded while it
continues — one event per `heartbeat_interval` (default 30 s), not one per
retry, so a hours-long saturation produces tens of events rather than
thousands.

| Field         | Type   | Description                                                        |
|---------------|--------|--------------------------------------------------------------------|
| `resource`    | string | Blocking resource: `"cpu"` or `"memory"`.                          |
| `actual`      | string | Observed value, formatted (normalized load, or available MB).      |
| `threshold`   | string | The configured threshold the observed value crossed.               |
| `reason`      | string | One-line human-readable reason.                                    |
| `attempt`     | number | How many admission checks have run since the hold began.           |
| `disposition` | string | `"holding"` (resident retry) or `"one_shot_unavailable"` (a caller that cannot stay resident was told to come back later). |

**Example:**
```json
{
  "event_type": "worker.admission_blocked",
  "timestamp": "2026-09-03T13:42:11Z",
  "data": {
    "resource": "cpu",
    "actual": "7.86",
    "threshold": "1.00",
    "reason": "CPU load saturated: 55.02 (1-minute average) / 7 cores = 7.86 > threshold 1.00",
    "attempt": 4,
    "disposition": "holding"
  }
}
```

#### `worker.admission_restored`
Emitted when launch admission recovers and normal selection is about to
resume. Always emitted before the next selection pass, never after it, and
only when a hold was actually in progress — a worker that was never blocked
does not emit it.

| Field          | Type   | Description                                            |
|----------------|--------|--------------------------------------------------------|
| `blocked_secs` | number | How long the hold lasted, in seconds.                  |
| `attempts`     | number | How many admission checks ran during the hold.         |

#### `worker.boot.timeout`
Emitted when worker initialization exceeds the configured timeout.

| Field         | Type   | Description                         |
|---------------|--------|-------------------------------------|
| `elapsed_ms`  | number | Time elapsed before timeout (ms).   |

---

### Attempt Ledger Events

#### `attempt.resolved`
The terminal ledger row for one dispatch (plan section 4.4 step 1, N-T16).
Exactly **one** is emitted per dispatch, from `OutcomeHandler::handle`, after
the match that routes to the eight terminal sub-handlers — so a sub-handler
that re-routes into another still yields a single row, and no terminal path
(success, gate_failure, gate_error, failure, timeout, crash, agent_not_found,
interrupted) can forget it.

The row separates *what the work did* from *what the lifecycle will do about
it*: `outcome` is the semantic verdict, `requested_action` is the bead action
the handler asked for, and `exit_code` is an observation of the agent process
only — an exit code of 0 with a rejected gate is a `work_failure`, never a
`verified_success`.

Two dispatch end-states outside those sub-handlers are also resolved to a row
by `OutcomeHandler::handle_with_cancellation`: a dispatch cancelled before
handling starts (`outcome: "cancelled"`, reason `cancelled_before_handling`)
and a handler torn down by its own timeout (`outcome: "indeterminate"`, reason
`outcome_handler_timeout`). The fallback is guarded, so a dispatch that
already has its row cannot gain a second.

Until N-T03 resolves attempts against beads, every row carries
`provisional: true` and a dispatch-local UUIDv7 `attempt_id` minted at
dispatch start. The same ID is stamped on every event the dispatch emits (the
envelope's `attempt_id` field, and the `attempt_id` OTLP log attribute), so a
row joins to its dispatch's events. **No consumer may treat a provisional row
as authoritative** — provisional rows are excluded from SLOs (plan Gate A).

| Field                  | Type            | Present | Description                                                                 |
|------------------------|-----------------|---------|-----------------------------------------------------------------------------|
| `schema_version`       | integer         | always  | Row schema version; `1` for this contract.                                  |
| `attempt_id`           | string          | always  | UUIDv7 minted at dispatch start.                                            |
| `provisional`          | boolean         | always  | `true` until the real attempt identity exists (N-T03).                      |
| `bead_id`              | string          | always  | Bead the attempt worked on.                                                 |
| `workspace`            | string          | always  | Workspace directory the attempt ran in.                                     |
| `worker`               | string          | always  | Worker identifier that ran the attempt.                                     |
| `adapter`              | string          | always  | Agent adapter that executed the attempt (e.g. `"claude-code-glm-5.3-flash"`). |
| `prompt_template`      | string          | always  | Prompt template that built the dispatch prompt (e.g. `"pluck"`).            |
| `template_version`     | string          | always  | Version tag of that template (e.g. `"pluck-default"`).                      |
| `gate_results`         | array           | always  | Per-gate entries `{name, status, duration_ms}`, ordered by gate name. `status` is `pass`, `fail`, or `execution_error`; empty when no gate ran. |
| `outcome`              | string          | always  | `verified_success`, `work_failure`, `infrastructure_failure`, `cancelled`, `stale_ownership`, or `indeterminate`. |
| `requested_action`     | string          | always  | Bead lifecycle action the handler requested (e.g. `"Completed"`, `"Released"`). |
| `commits`              | array           | always  | Commit SHAs created in the workspace during the attempt.                    |
| `duration_ms`          | integer         | always  | Wall-clock time from claim to resolution.                                   |
| `exit_code`            | integer         | always  | Agent process exit code. Observation only.                                  |
| `bead_revision_start`  | string          | optional| HEAD SHA captured before the agent ran.                                     |
| `model`                | string          | optional| Model identifier from the adapter config.                                   |
| `provider`             | string          | optional| Model provider (e.g. `"anthropic"`).                                        |
| `context_manifest_hash`| string          | optional| ContextManifest hash — absent until N-T10 ships manifest hashing.           |
| `confirmed_state`      | string          | optional| Authoritative post-action bead state; reserved for the resolver's re-read (plan section 3.2 step 5). |
| `tokens_in`            | integer         | optional| Input tokens reported by the agent's token extractor.                       |
| `tokens_out`           | integer         | optional| Output tokens reported by the agent's token extractor.                      |
| `estimated_cost_usd`   | number          | optional| Estimated cost in USD; absent when no pricing is configured.                |
| `terminal_reason`      | string          | optional| Short machine-readable reason for the terminal state (e.g. `"gate:fmt"`, `"exit_code:1"`, `"signal:9"`). Absent on a verified success. |

**Outcome vocabulary:**
- `verified_success` — the work passed every gate.
- `work_failure` — a gate ran and rejected the work, or the agent reported failure; attributable to the attempt.
- `infrastructure_failure` — nothing judged the work: the agent binary was missing or crashed, or a gate could not run or could never pass. Feeds workspace health, not the bead's failure count.
- `cancelled` — the worker was interrupted before a verdict.
- `stale_ownership` — reserved; assigned by the resolver once ownership is re-checked at resolution time (ADR-024), not by the outcome handler.
- `indeterminate` — the attempt ended without a verdict: the time budget expired while the work was still running.

**Versioned contract:** [`tests/fixtures/attempt-resolved-v1.schema.json`](../tests/fixtures/attempt-resolved-v1.schema.json)
is the schema this row must satisfy; conformance is asserted in
`src/telemetry/mod.rs`. Breaking changes version both the fixture and the
row's `schema_version`.

**Aggregation:** `needle stats --by adapter` and `needle stats --by outcome`
aggregate these rows directly — one attempt per row, with PASS RATE reading
as verified success over all attempts in the group. Both surface a
PROVISIONAL count per group: the rows flagged `provisional: true`. Until
N-T03 lands every row is provisional, and such rows are excluded from
authoritative SLOs (Gate A), so consumers must not treat a provisional
attempt ID as authoritative.

**Example (a rejected gate behind a zero exit code):**
```json
{
  "event_type": "attempt.resolved",
  "timestamp": "2026-09-10T12:34:56.312Z",
  "worker_id": "needle-alpha",
  "session_id": "9f2c11aa",
  "sequence": 812,
  "bead_id": "needle-96dec90b",
  "workspace": "/home/coding/NEEDLE",
  "attempt_id": "0198f6a1-7c2d-7cc3-98c4-dc0c0c07398f",
  "data": {
    "schema_version": 1,
    "attempt_id": "0198f6a1-7c2d-7cc3-98c4-dc0c0c07398f",
    "provisional": true,
    "bead_id": "needle-96dec90b",
    "workspace": "/home/coding/NEEDLE",
    "bead_revision_start": "f902c854",
    "worker": "needle-alpha",
    "adapter": "claude-code-glm-5.3-flash",
    "model": "glm-5.3-flash",
    "provider": "anthropic",
    "prompt_template": "pluck",
    "template_version": "pluck-default",
    "gate_results": [
      { "name": "clippy", "status": "fail", "duration_ms": 0 },
      { "name": "fmt", "status": "pass", "duration_ms": 0 }
    ],
    "outcome": "work_failure",
    "requested_action": "Released",
    "tokens_in": 120000,
    "tokens_out": 4500,
    "estimated_cost_usd": 0.0921,
    "commits": ["deadbee"],
    "duration_ms": 614000,
    "terminal_reason": "gate:clippy",
    "exit_code": 0
  }
}
```

**Example (a verified success — every gate passed, no `terminal_reason`):**
```json
{
  "event_type": "attempt.resolved",
  "timestamp": "2026-09-10T12:51:02.884Z",
  "worker_id": "needle-alpha",
  "session_id": "9f2c11aa",
  "sequence": 907,
  "bead_id": "needle-2f97cbb5",
  "workspace": "/home/coding/NEEDLE",
  "attempt_id": "0198f6a2-e4b5-7cc3-9c2e-1f4b8d6a02c1",
  "data": {
    "schema_version": 1,
    "attempt_id": "0198f6a2-e4b5-7cc3-9c2e-1f4b8d6a02c1",
    "provisional": true,
    "bead_id": "needle-2f97cbb5",
    "workspace": "/home/coding/NEEDLE",
    "bead_revision_start": "f902c854",
    "worker": "needle-alpha",
    "adapter": "claude-code-glm-5.3-flash",
    "model": "glm-5.3-flash",
    "provider": "anthropic",
    "prompt_template": "pluck",
    "template_version": "pluck-default",
    "gate_results": [
      { "name": "clippy", "status": "pass", "duration_ms": 0 },
      { "name": "fmt", "status": "pass", "duration_ms": 0 }
    ],
    "outcome": "verified_success",
    "requested_action": "Completed",
    "tokens_in": 118000,
    "tokens_out": 5200,
    "estimated_cost_usd": 0.0894,
    "commits": ["deadbee"],
    "duration_ms": 589000,
    "exit_code": 0
  }
}
```

**Reading the two examples together:**
- **Provisional ids are not authoritative.** Both rows carry
  `provisional: true` and a dispatch-local UUIDv7 `attempt_id` minted at
  dispatch start — the ID identifies the dispatch, not a durable attempt
  (that starts with N-T03). No consumer may treat a provisional row as
  authoritative, and provisional rows are excluded from SLOs (plan Gate A).
- **`exit_code` is observation only.** It records what the agent process did,
  never whether the work was accepted. The `work_failure` row above exited `0`
  and is still a failure, because `clippy` ran and rejected the work; the
  `verified_success` row exited `0` *and* passed every gate. The `outcome`
  field — not the exit code — carries the verdict. `terminal_reason` names the
  gate on the failure — `gate:clippy`, the `name` of the entry `gate_results`
  shows `fail` — and is absent on the success.

---

### Upgrade Check Events

#### `upgrade_check.started`
Emitted when an upgrade check begins.

| Field      | Type   | Description                                                      |
|------------|--------|------------------------------------------------------------------|
| `source`   | string | Source of the check (e.g., `"manual"`, `"download_to_testing"`). |

**Example:**
```json
{
  "event_type": "upgrade_check.started",
  "timestamp": "2026-08-28T12:34:56Z",
  "needle_version": "0.2.15",
  "source": "manual"
}
```

#### `upgrade_check.completed`
Emitted when an upgrade check completes successfully.

| Field                | Type    | Description                                             |
|----------------------|---------|---------------------------------------------------------|
| `source`             | string  | Source of the check.                                    |
| `current_version`    | string  | Currently running version.                               |
| `latest_version`     | string  | Latest available version from GitHub.                    |
| `update_available`   | boolean | Whether a newer version exists.                          |
| `has_release_notes`  | boolean | Whether release notes were included in the response.     |

**Example:**
```json
{
  "event_type": "upgrade_check.completed",
  "timestamp": "2026-08-28T12:34:57Z",
  "needle_version": "0.2.15",
  "source": "manual",
  "current_version": "0.2.15",
  "latest_version": "0.2.16",
  "update_available": true,
  "has_release_notes": true
}
```

#### `upgrade_check.failed`
Emitted when an upgrade check fails.

| Field           | Type   | Description                                                    |
|-----------------|--------|----------------------------------------------------------------|
| `source`        | string | Source of the check.                                          |
| `error_message` | string | Human-readable error description.                              |
| `error_type`    | string | Error category: `"network"`, `"parse"`, `"api"`, or `"unknown"`. |

**Example (network error):**
```json
{
  "event_type": "upgrade_check.failed",
  "timestamp": "2026-08-28T12:34:58Z",
  "needle_version": "0.2.15",
  "source": "manual",
  "error_message": "failed to fetch latest release from GitHub: connection refused",
  "error_type": "network"
}
```

**Example (API error):**
```json
{
  "event_type": "upgrade_check.failed",
  "timestamp": "2026-08-28T12:34:58Z",
  "needle_version": "0.2.15",
  "source": "download_to_testing",
  "error_message": "GitHub API returned status 403 when checking for updates",
  "error_type": "api"
}
```

**Error Type Classifications:**
- `network` — DNS, connection, or network-level failures
- `parse` — JSON parsing or response format errors
- `api` — GitHub API errors (4xx, 5xx, rate limits)
- `unknown` — Unclassified errors

---

### Canary Testing Events

#### `canary.started`
Emitted when a canary test suite begins execution.

| Field     | Type   | Description                   |
|-----------|--------|-------------------------------|
| `suite`   | string | Canary test suite identifier. |

#### `canary.suite_completed`
Emitted when a canary test suite completes (pass or fail).

| Field     | Type   | Description                        |
|-----------|--------|------------------------------------|
| `suite`   | string | Canary test suite identifier.      |
| `passed`  | number | Number of tests that passed.       |
| `failed`  | number | Number of tests that failed.       |

#### `canary.promoted`
Emitted when a canary-tested binary is promoted to `:stable`.

| Field     | Type   | Description                        |
|-----------|--------|------------------------------------|
| `hash`    | string | SHA-256 hash of the promoted binary. |

#### `canary.rejected`
Emitted when a canary-tested binary is rejected after failing tests.

| Field     | Type   | Description                        |
|-----------|--------|------------------------------------|
| `reason`  | string | Human-readable rejection reason.   |

---

### Worker Upgrade Detection Events

#### `worker.upgrade.detected`
Emitted when a worker detects a new `:stable` binary via hot-reload check.

| Field      | Type   | Description                                  |
|------------|--------|----------------------------------------------|
| `old_hash` | string | Hash of the currently running binary.        |
| `new_hash` | string | Hash of the new `:stable` binary.            |

#### `worker.upgrade.completed`
Emitted when a worker successfully re-exec's into the new binary.

| Field      | Type   | Description                                  |
|------------|--------|----------------------------------------------|
| `new_hash` | string | Hash of the new binary now running.          |

---

### Process Spawning Events

#### `spawn.launched`
Emitted when a child process (agent, test, etc.) is successfully launched.

| Field         | Type   | Description                                |
|---------------|--------|--------------------------------------------|
| `binary`      | string | Path to the executed binary.               |
| `pid`         | number | Process ID of the child.                   |
| `worker_name` | string | Worker that spawned the process (if applicable). |

#### `spawn.failed`
Emitted when a child process launch fails.

| Field         | Type   | Description                                |
|---------------|--------|--------------------------------------------|
| `binary`      | string | Path to the binary that failed to launch.  |
| `error`       | string | Error description.                         |
| `worker_name` | string | Worker that attempted the spawn.           |

#### `spawn.exited`
Emitted when a child process exits (normal or error).

| Field         | Type   | Description                                |
|---------------|--------|--------------------------------------------|
| `pid`         | number | Process ID that exited.                    |
| `exit_code`   | number | Exit status code (0 = success).            |
| `worker_name` | string | Worker that owned the process.             |

#### `spawn.terminated`
Emitted when a child process is terminated by a signal.

| Field         | Type   | Description                                |
|---------------|--------|--------------------------------------------|
| `pid`         | number | Process ID that was terminated.            |
| `signal`      | string | Signal name (e.g., `"SIGTERM"`).          |
| `worker_name` | string | Worker that owned the process.             |

---

### Process Guard Events

#### `process_guard.violation`
Emitted when a ProcessGuard constraint is violated.

| Field         | Type   | Description                                    |
|---------------|--------|------------------------------------------------|
| `constraint`  | string | Constraint that was violated (e.g., `"cputime"`). |
| `pid`         | number | Process ID that violated the constraint.      |
| `worker_name` | string | Worker that owned the process.                |

#### `process_guard.exhausted`
Emitted when all ProcessGuard retries are exhausted.

| Field         | Type   | Description                                    |
|---------------|--------|------------------------------------------------|
| `constraint`  | string | Constraint that was violated.                  |
| `max_retries` | number | Maximum retry attempts configured.             |
| `worker_name` | string | Worker that attempted the spawn.              |

---

### Spawn Path Integrity Events

#### `spawn_path.modified_in_place`
Emitted when the spawn-path binary is modified without a corresponding re-exec.

| Field         | Type   | Description                                    |
|---------------|--------|------------------------------------------------|
| `path`        | string | Path to the modified binary.                   |
| `old_hash`    | string | Original binary hash.                          |
| `new_hash`    | string | New binary hash after modification.            |

---

## Event Type Naming Convention

Event types use **dot-separated names** with these components:

```
{category}.{subcategory}.{action}
```

Examples:
- `upgrade_check.started` → Upgrade check category, started action
- `worker.upgrade.detected` → Worker category, upgrade subcategory, detected action
- `spawn.exited` → Spawn category, exited action (no subcategory)

---

## Timestamps

All timestamps are **ISO 8601** format in UTC:
- Format: `YYYY-MM-DDTHH:MM:SSZ`
- Millisecond precision: `YYYY-MM-DDTHH:MM:SS.sssZ` (when applicable)
- Timezone: Always `Z` (UTC)

---

## Versioning

Telemetry events follow these versioning rules:

1. **Additive changes** (new optional fields, new event types) do NOT break compatibility
2. **Breaking changes** (renaming fields, removing fields, changing types) require a major version bump
3. Consumers MUST ignore unknown fields
4. The `needle_version` field identifies the NEEDLE binary emitting the event

---

## Related Documentation

- [Telemetry Field Capture Strategy](./telemetry-field-capture-strategy.md) — How telemetry fields are recorded on spans
- [Agent Event Schema](./agent-event-schema.md) — Agent output events (separate from internal telemetry)
- [ADR-002: Pluck Telemetry Isolation](./adr/002-pluck-telemetry-isolation-and-process-tracking.md) — Process tracking design
