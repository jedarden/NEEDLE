# Triage: Telemetry Sink Misses Live Workers

**Date**: 2026-06-25
**Bead**: bf-2bw
**Task**: Root-cause why telemetry sink misses live workers

## Root Cause Identified

**TWO BUGS** causing sink-config vs query-path mismatch:

### Bug 1: Workers ignore `file_sink.log_dir` config (SINK-CONFIG)

**Location**: `src/telemetry/mod.rs:2954`

**Problem**: `FileSink::new()` hardcodes the log directory instead of using config:

```rust
pub fn new(worker_id: &str, session_id: &str) -> Result<Self> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let log_dir = PathBuf::from(&home).join(".needle").join("logs");  // ← HARDCODED
    Self::with_dir(&log_dir, worker_id, session_id)
}
```

**Impact**: Workers ALWAYS write to `~/.needle/logs/` regardless of `telemetry.file_sink.log_dir` configuration.

**Call chain**:
1. `Worker::new()` → `Telemetry::from_config(worker_id, &config.telemetry)`
2. `Telemetry::from_config()` → `FileSink::new(&worker_id, &session_id)` (line ~2793)
3. `FileSink::new()` → hardcodes `~/.needle/logs/`

**Fix required**: `Telemetry::from_config()` should use:
```rust
let home = &config.workspace.home;  // or from file_sink.log_dir if set
let log_dir = config.file_sink.log_dir
    .as_ref()
    .map(|p| p.as_path())
    .unwrap_or_else(|| home.join("logs"));
match FileSink::with_dir(&log_dir, &worker_id, &session_id) {
```

---

### Bug 2: `needle stats` reads wrong path (QUERY-PATH)

**Location**: `src/cli/mod.rs:1916`

**Problem**: `needle stats` ignores `file_sink.log_dir` entirely:

```rust
// needle logs (CORRECT - uses config):
let log_dir = config
    .telemetry
    .file_sink
    .log_dir
    .clone()
    .unwrap_or_else(|| needle_home.join("logs"));  // ✓ respects config

// needle stats (WRONG - hardcodes relative to workspace.home):
let log_dir = config.workspace.home.join("logs");  // ✗ ignores config
```

**Impact**: Even if Bug 1 is fixed, `needle stats` won't see telemetry from custom log paths.

**Fix required**: Change `needle stats` to use the same logic as `needle logs`:
```rust
let log_dir = config
    .telemetry
    .file_sink
    .log_dir
    .clone()
    .unwrap_or_else(|| needle_home.join("logs"));
```

---

## Is Transform Working?

**Transform (`needle-transform-claude`)**: ✓ **Working correctly**

**Evidence**:
- Robust line-by-line JSON parsing with graceful degradation (src/bin/needle_transform_claude.rs:47-76)
- PTY escape handling via `unbuffer -p` in adapter invoke_template (src/dispatch/mod.rs:256-276)
- Error telemetry emitted on transform failures (src/dispatch/mod.rs:830-860)
- Backpressure prevention with bounded channel (drops lines rather than blocking)

**Conclusion**: Transform is NOT the culprit. The issue is purely path misconfiguration.

---

## Why "Misses Live Workers"

When NATO workers are running:
1. Workers write telemetry to `~/.needle/logs/` (hardcoded path)
2. User may have set `file_sink.log_dir` to custom path in config
3. `needle logs` reads from custom path (empty) → sees no events
4. `needle stats` reads from `~/.needle/logs` → sees events BUT:
   - If user set custom path and moved logs, stats sees nothing
   - Commands are inconsistent in their path resolution

---

## Fix Plan

### Priority 1: Fix sink-config (workers respect `file_sink.log_dir`)

**File**: `src/telemetry/mod.rs`
**Function**: `Telemetry::from_config()`

Change:
```rust
// OLD (line ~2793):
match FileSink::new(&worker_id, &session_id) {

// NEW:
let log_dir = config.file_sink.log_dir
    .as_ref()
    .map(|p| p.as_path())
    .unwrap_or_else(|| {
        // Fallback to workspace.home/logs if file_sink.log_dir not set
        // Need to pass workspace.home into this function or resolve it
        dirs_or_home(".needle/logs")
    });
match FileSink::with_dir(&log_dir, &worker_id, &session_id) {
```

**Challenge**: `TelemetryConfig` doesn't have `workspace.home` - need to either:
- Pass `workspace_home: &Path` parameter to `Telemetry::from_config()`
- OR resolve `HOME` env var directly in `from_config()`

### Priority 2: Fix query-path (`needle stats` consistency)

**File**: `src/cli/mod.rs`
**Function**: `cmd_stats()`

Change:
```rust
// OLD (line 1916):
let log_dir = config.workspace.home.join("logs");

// NEW:
let log_dir = config
    .telemetry
    .file_sink
    .log_dir
    .clone()
    .unwrap_or_else(|| config.workspace.home.join("logs"));
```

---

## Related Code Locations

| Component | File | Lines | Issue |
|-----------|------|-------|-------|
| `FileSink::new()` | `src/telemetry/mod.rs` | ~2954 | Hardcodes path |
| `Telemetry::from_config()` | `src/telemetry/mod.rs` | ~2793 | Calls hardcoded path |
| `needle logs` | `src/cli/mod.rs` | 2867-2872 | ✓ Correct (uses config) |
| `needle stats` | `src/cli/mod.rs` | 1916 | ✗ Wrong (ignores config) |

---

## Verification Steps

After fix:
1. Set custom log path in config: `file_sink.log_dir: /tmp/needle-logs`
2. Start worker: `needle work`
3. Verify JSONL created in `/tmp/needle-logs/`
4. Run `needle logs` and `needle stats`
5. Both should show live worker telemetry

---

## Test Cases Needed

1. **Default behavior**: `file_sink.log_dir=null` → writes to `~/.needle/logs/`
2. **Custom path**: `file_sink.log_dir=/tmp/custom` → writes there
3. **Command consistency**: Both `needle logs` and `needle stats` read from same path
4. **Per-worker files**: Each worker creates `<worker_id>-<session_id>.jsonl`
