# bf-1ig4: IdleAction::Exit Warning Implementation

## Summary
Implemented startup-time config validation to warn when `IdleAction::Exit` is configured without an active supervisor. This prevents orphaned in_progress beads that can occur when workers exit by policy (empty queue) without a supervisor to reclaim stuck beads.

## Changes Made

### 1. Added supervisor detection (`src/health/mod.rs`)
- **New function**: `HealthMonitor::detect_supervisor()`
- **Detection logic**:
  - Returns `true` if multiple active workers exist (indicating supervisor-managed fleet)
  - Returns `true` if recent worker spawn activity detected (workers started within last 5 minutes)
  - Returns `false` for single standalone worker (no supervisor)
- **Purpose**: Allows workers to determine if they're running under supervisor supervision

### 2. Added idle_action validation step (`src/worker/mod.rs`)
- **Location**: `Worker::boot()` method, after config validation
- **New init step**: `idle_action_validation`
- **Behavior**:
  - Checks if `idle_action == Exit`
  - If yes and no supervisor detected: emits visible warning about orphaned bead risk
  - Warning includes fix: either run under supervisor or set `idle_action=wait`
  - If supervisor detected: logs confirmation that exit policy is safe
- **Telemetry**: Emits `init.step.started`/`init.step.completed` events

### 3. Added comprehensive tests (`src/health/mod.rs`)
- `detect_supervisor_no_other_workers`: Tests standalone worker case
- `detect_supervisor_multiple_workers`: Tests multi-worker fleet detection
- `detect_supervisor_ignores_stale_heartbeats`: Ensures stale workers don't trigger detection
- `detect_supervisor_recent_spawn_activity`: Tests recent spawn detection
- `detect_supervisor_nonexistent_directory`: Tests graceful handling of missing directory

## Testing
- All new tests pass (5/5)
- Full test suite passes without regressions
- Supervisor detection correctly identifies fleet-managed vs standalone workers

## Impact
- **Proactive guard**: Warns at startup before orphaned beads can occur
- **User-friendly**: Clear warning message with actionable fix
- **Non-breaking**: Only emits warnings, doesn't change behavior
- **Complementary**: Works alongside reactive reclaimer (bf-et0) as defense-in-depth

## Example Warning Output
```
WARN no supervisor detected: worker configured to exit when queue is dry will leave orphaned in_progress beads with no reclaim mechanism
WARN to fix: either run workers under a supervisor (needle supervise) or set idle_action=wait in config
```
