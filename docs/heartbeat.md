# Heartbeat Functionality

## Overview

NEEDLE workers implement a heartbeat mechanism that creates and periodically updates a JSON file to signal liveness. This enables:

- **Peer detection**: Workers can detect crashed peers by checking heartbeat file freshness
- **Capacity management**: External monitors (e.g., cgov) can scale worker pools based on heartbeat state
- **Debugging**: Operators can inspect worker state by reading heartbeat files

## Implementation

### File Location

Heartbeat files are written to:
```
~/.needle/state/heartbeats/<qualified-id>.json
```

Where `qualified-id` is `{adapter}-{worker-id}` (e.g., `claude-code-glm-5-foxtrot`).

The directory can be customized via the `health.heartbeat_dir` config option.

### File Contents

Each heartbeat file contains a JSON document with the following structure:

```json
{
  "worker_id": "foxtrot",
  "qualified_id": "claude-code-glm-5-foxtrot",
  "pid": 12345,
  "state": "Executing",
  "current_bead": "needle-abc",
  "workspace": "/home/user/workspace",
  "last_heartbeat": "2026-06-25T12:34:56Z",
  "started_at": "2026-06-25T12:00:00Z",
  "beads_processed": 42,
  "session": "foxtrot",
  "is_idle": false,
  "current_task": "needle-abc",
  "model": "claude-code-glm-5"
}
```

### Fields

- **worker_id**: Bare NATO name (e.g., "alpha", "foxtrot")
- **qualified_id**: Fully-qualified identity including adapter prefix
- **pid**: Process ID of the worker
- **state**: Current worker state (Selecting, Claiming, Building, etc.)
- **current_bead**: Bead ID currently being processed (if any)
- **workspace**: Path to the workspace being processed
- **last_heartbeat**: ISO 8601 timestamp of last heartbeat write
- **started_at**: ISO 8601 timestamp when worker started
- **beads_processed**: Total beads processed by this worker
- **session**: Session identifier (same as worker_id)
- **is_idle**: Whether worker is idle (no active bead)
- **current_task**: Current task ID (cgov compatibility field)
- **model**: Model being used (from adapter config)

### Update Interval

Heartbeat files are updated every `heartbeat_interval_secs` (default: 30 seconds).

The interval can be configured via:
- Config file: `health.heartbeat_interval_secs`
- Environment variable: `NEEDLE_HEALTH__HEARTBEAT_INTERVAL_SECS`

### Time-to-Live (TTL)

Heartbeats older than `heartbeat_ttl_secs` (default: 300 seconds) are considered stale.

The TTL should be at least 3x the heartbeat interval for reliable detection.

## Verification

### Automated Validation

Run the automated validation script:
```bash
./tests/validate_heartbeat.sh
```

This script:
1. Starts a worker with a 5-second heartbeat interval
2. Verifies heartbeat file creation on startup
3. Checks that the file contains required fields (worker_id, last_heartbeat)
4. Monitors the file for periodic updates over 15 seconds
5. Reports success or failure

### Manual Verification

To manually verify heartbeat functionality:

1. **Start a worker**:
   ```bash
   needle worker --name test-alpha
   ```

2. **Watch the heartbeat file**:
   ```bash
   # Find your worker's heartbeat file
   watch -n 5 'cat ~/.needle/state/heartbeats/*.json | jq .'
   ```

3. **Verify periodic updates**:
   The `last_heartbeat` field should update every 30 seconds.

4. **Check file age**:
   ```bash
   ls -lah ~/.needle/state/heartbeats/
   ```

### Using ls to Monitor Updates

```bash
# Monitor heartbeat file modification times
watch -n 5 'ls -lah ~/.needle/state/heartbeats/*.json'

# The file should update every heartbeat_interval_secs
```

## Architecture

### Thread Safety

The heartbeat emitter runs in a dedicated `std::thread` (not part of the Tokio runtime). This ensures:

- Heartbeat updates continue even if the async runtime wedges
- The worker responds quickly to shutdown signals (interruptible sleep pattern)

### Atomic Writes

Heartbeat files use atomic writes to prevent partial reads:
1. Write to temporary file (`.json.tmp`)
2. Rename temp file to final path

This ensures readers never see partially-written JSON.

### Circuit Breaker

The heartbeat emitter has a circuit breaker that trips after 10 consecutive write failures:
- Sets the shutdown flag to terminate the worker
- Prevents infinite retry loops
- Emits diagnostic information to help troubleshoot

### State Sharing

The main worker loop updates shared state via `Arc<Mutex<SharedHeartbeatState>>`:
- Worker state transitions (Selecting → Claiming → Executing, etc.)
- Current bead ID
- Beads processed count
- Current workspace

The emitter thread reads this shared state without blocking the main loop.

## Configuration

### Config File (.needle.yaml)

```yaml
health:
  heartbeat_interval_secs: 30  # Update every 30 seconds
  heartbeat_ttl_secs: 300       # Stale after 5 minutes
  heartbeat_dir: state/heartbeats  # Relative to workspace.home
```

### Environment Variables

```bash
export NEEDLE_HEALTH__HEARTBEAT_INTERVAL_SECS=30
export NEEDLE_HEALTH__HEARTBEAT_TTL_SECS=300
```

## Testing

Unit tests in `src/health/mod.rs` cover:
- `heartbeat_file_written_on_start`: Verifies file creation on startup
- `heartbeat_updates_with_shared_state`: Verifies state updates propagate
- `heartbeat_file_removed_on_stop`: Verifies cleanup on shutdown
- `atomic_write_never_produces_partial`: Verifies atomic write pattern
- `heartbeat_path_uses_qualified_id_not_bare_worker_id`: Verifies unique filenames
- `heartbeat_files_dont_collide_across_adapter_pools`: Verifies no collisions
- `heartbeat_uses_cross_workspace_bead_workspace`: Verifies workspace tracking

Run tests:
```bash
cargo test heartbeat --lib
```

## Troubleshooting

### Heartbeat file not created

1. Check the heartbeat directory exists and is writable
2. Verify config: `needle config --dump`
3. Check worker logs for errors

### Heartbeat file not updating

1. Verify the worker process is still running
2. Check for disk I/O errors
3. Monitor the worker logs for "heartbeat write failed" messages

### Stale heartbeat detected

1. Check if the worker process crashed
2. Verify the PID in the heartbeat file matches a running process
3. Check system time (clock skew can cause false positives)

## See Also

- `src/health/mod.rs`: Implementation
- `src/config/mod.rs`: HealthConfig structure
- `src/worker/mod.rs`: Worker integration
- [Capacity Governor](../capacity-governor/README.md): External scaling system
