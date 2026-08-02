# Worker Construction Structured Logging

This document describes the structured logging framework for worker construction phases.

## Overview

The worker construction process emits telemetry events for each major construction phase, allowing for observability and debugging of worker initialization.

## Construction Phases

The worker construction (`Worker::build()`) emits `InitStepStarted` and `InitStepCompleted` events for the following phases:

1. **strand_setup** - StrandRunner creation and configuration
2. **claimer_creation** - Claimer initialization for bead claiming
3. **prompt_builder_setup** - PromptBuilder setup with workspace skills
4. **dispatcher_setup** - Dispatcher and adapter loading
5. **outcome_handler_creation** - OutcomeHandler initialization
6. **health_monitor_setup** - HealthMonitor setup for worker health tracking
7. **rate_limiter_setup** - RateLimiter initialization
8. **mitosis_evaluator_setup** - MitosisEvaluator for bead splitting
9. **registry_state_restoration** - Registry state restoration for hot-reload resume

## Event Format

Each construction phase emits two events:

### InitStepStarted
```json
{
  "timestamp": "2026-08-02T10:00:00.000Z",
  "event_type": "init.step.started",
  "worker_id": "claude-test-worker",
  "session_id": "a1b2c3d4",
  "sequence": 1,
  "data": {
    "step": "strand_setup"
  }
}
```

### InitStepCompleted
```json
{
  "timestamp": "2026-08-02T10:00:00.100Z",
  "event_type": "init.step.completed",
  "worker_id": "claude-test-worker",
  "session_id": "a1b2c3d4",
  "sequence": 2,
  "data": {
    "step": "strand_setup",
    "duration_ms": 100
  }
}
```

## Consistency

The logging framework uses consistent field names across all phases:
- `step` - Phase name (snake_case)
- `duration_ms` - Phase duration in milliseconds (completed event only)
- `timestamp` - ISO 8601 timestamp with UTC timezone
- `event_type` - Dotted event type string
- `worker_id` - Qualified worker identifier
- `session_id` - 8-character hex session identifier

## Launch Mode Compatibility

The structured logging works identically for both launch modes:
- **Direct mode** - Worker construction happens in the main process
- **Tmux mode** - Worker construction happens in a re-exec'd process inside tmux

Both modes use the same `Worker::build()` function, ensuring consistent telemetry.

## Usage

The events are automatically emitted during worker construction. No additional configuration is required.

To monitor worker construction:
1. Open the worker's JSONL telemetry log
2. Filter for `event_type` starting with `init.step.`
3. Observe the sequence of construction phases and their durations

## Example

```bash
# View worker construction phases
cat ~/.needle/tele/claude-test-*.jsonl | \
  jq 'select(.event_type | startswith("init.step."))' | \
  jq '{event_type, step: .data.step, duration_ms: .data.duration_ms}'
```

Output:
```json
{"event_type": "init.step.started", "step": "strand_setup", "duration_ms": null}
{"event_type": "init.step.completed", "step": "strand_setup", "duration_ms": 15}
{"event_type": "init.step.started", "step": "claimer_creation", "duration_ms": null}
{"event_type": "init.step.completed", "step": "claimer_creation", "duration_ms": 2}
...
```

## Implementation

The logging is implemented in `Worker::build()` at `/home/coding/NEEDLE/src/worker/mod.rs`.

Each phase follows this pattern:
```rust
let _ = telemetry.emit(EventKind::InitStepStarted {
    step: "phase_name".to_string(),
});
let phase_start = Instant::now();
// ... phase construction code ...
let _ = telemetry.emit(EventKind::InitStepCompleted {
    step: "phase_name".to_string(),
    duration_ms: phase_start.elapsed().as_millis() as u64,
});
```

## Related Documentation

- `docs/structured-logging.md` - General telemetry architecture
- `CLAUDE.md` - Project conventions
- `src/telemetry/mod.rs` - Telemetry event definitions
