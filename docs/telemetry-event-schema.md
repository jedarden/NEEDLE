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
- `worker.boot.timeout` — Worker initialization exceeded timeout

### State Transitions
- `worker.state_transition` — Worker moved between states

### Strand Evaluation
- `strand.evaluated` — A strand evaluation completed
- `strand.skipped` — A strand was skipped (e.g., due to config)
- `strand.resolve.evaluated` — Resolve strand evaluation
- `strand.pluck.starvation_detected` — Pluck strand detected bead starvation

### Bead Processing
- `bead.claim.attempted` — Worker attempted to claim a bead
- `bead.claim.succeeded` — Bead claim succeeded
- `bead.claim.race_lost` — Bead claim lost to another worker
- `bead.claim.race_lost_skipped` — Skipped retry due to race loss
- `bead.claim.failed` — Bead claim failed (non-race error)

### Bead Store Errors
- `bead_store.error` — Bead store operation failed

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

#### `worker.boot.timeout`
Emitted when worker initialization exceeds the configured timeout.

| Field         | Type   | Description                         |
|---------------|--------|-------------------------------------|
| `elapsed_ms`  | number | Time elapsed before timeout (ms).   |

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
