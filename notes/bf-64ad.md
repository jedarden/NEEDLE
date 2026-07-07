# Supervisor Config Structure Analysis (Bead bf-64ad)

## Summary

Investigated the relationship between `SupervisorConfig` and `SupervisorDetectionConfig` to determine which struct needs work for the "Add basic supervisor config struct" bead.

**Finding: No config struct needs work — all are already complete and implemented.**

## The Three Config Structs

### 1. `SupervisorConfig` in `src/supervisor/mod.rs` (lines 40-51)

**Purpose:** Internal runtime configuration for the supervisor itself.

**Fields:**
```rust
pub struct SupervisorConfig {
    pub workspace: PathBuf,           // Workspace to monitor
    pub max_workers: u32,              // Max concurrent workers
    pub poll_interval_secs: u64,       // Polling interval (seconds)
    pub agent: Option<String>,        // Agent adapter name
    pub agent_timeout: Option<u64>,    // Agent timeout (seconds)
}
```

**Status:** ✅ **COMPLETE**

**Usage:** Built from main config fields in `cmd_supervise()` (lines 445-451):
```rust
let supervisor_config = SupervisorConfig {
    workspace: workspace_root.clone(),
    max_workers: config.worker.max_workers,
    poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,  // hardcoded
    agent: Some(config.agent.default.clone()),
    agent_timeout: Some(config.agent.timeout),
};
```

---

### 2. `SupervisorConfig` in `src/config/mod.rs` (lines 1340-1361)

**Purpose:** Configuration for workers to **detect** if they're running under a supervisor process.

**Fields:**
```rust
pub struct SupervisorConfig {
    pub heartbeat_path: Option<PathBuf>,   // Supervisor heartbeat file
    pub socket_path: Option<PathBuf>,      // Optional control socket
}
```

**Status:** ✅ **COMPLETE**

**Usage:** Used in main `Config` struct (line 1719):
```rust
pub struct Config {
    // ...
    pub supervisor: SupervisorConfig,  // For detection, not behavior
}
```

**Note:** The name is misleading — this struct is for **detection**, not for configuring supervisor behavior.

---

### 3. `SupervisorDetectionConfig` in `src/config/mod.rs` (lines 347-354)

**Purpose:** Appears to be an earlier attempt at supervisor detection config.

**Fields:**
```rust
pub struct SupervisorDetectionConfig {
    pub heartbeat_path: PathBuf,    // Non-Option!
    pub socket_path: Option<PathBuf>,
}
```

**Status:** ⚠️ **DEAD CODE** — Never used anywhere in the codebase.

**Evidence:**
- Not referenced in any source file except its own definition
- Only mentioned in notes from beads that verified it already existed
- The main Config uses `SupervisorConfig` (not `SupervisorDetectionConfig`) at line 1719

---

## How Supervisor Config Currently Works

The supervisor does **NOT** have its own section in the main config file. Instead, it pulls values from existing sections:

| Supervisor Field | Main Config Source | Default |
|------------------|-------------------|---------|
| `workspace` | CLI arg `--workspace` | Current directory |
| `max_workers` | `worker.max_workers` | 4 |
| `poll_interval_secs` | Hardcoded constant | 10s |
| `agent` | `agent.default` | "claude" |
| `agent_timeout` | `agent.timeout` | 300s |

This design is intentional — the supervisor is a consumer of existing config, not a standalone configurable subsystem.

---

## Decision

### Which config struct needs work?

**NONE.** All active config structs are complete and working:

1. ✅ `SupervisorConfig` (supervisor/mod.rs) — complete internal config
2. ✅ `SupervisorConfig` (config/mod.rs) — complete detection config
3. ⚠️ `SupervisorDetectionConfig` (config/mod.rs) — dead code, should be removed

### Should we create a new struct?

**NO.** The current design is correct:

- The supervisor's internal config is complete and functional
- Supervisor detection config (heartbeat/socket paths) is already in the main Config
- No new struct is needed

### Recommendation

1. **Keep** the two active `SupervisorConfig` structs as-is
2. **Remove** the dead `SupervisorDetectionConfig` struct (lines 347-354)
3. **Optional:** Consider renaming the main config's `SupervisorConfig` to `SupervisorDetectionConfig` for clarity, after removing the dead one

---

## Why This Confusion Happened

The naming collision happened because:

1. Early beads created `SupervisorDetectionConfig` for detection config
2. Later beads added a `SupervisorConfig` to the main Config for the same purpose
3. The main Config ended up using `SupervisorConfig` instead of `SupervisorDetectionConfig`
4. The original `SupervisorDetectionConfig` was never cleaned up

This is a normal artifact of iterative development — dead code accumulates. The fix is straightforward: remove the unused struct.

---

## Conclusion

**No implementation work is required.** The supervisor config is complete and functional. The only action item is cleaning up dead code (`SupervisorDetectionConfig`), which is a maintenance task, not a feature addition.

The "Add basic supervisor config struct" bead was likely completed in earlier beads that:
- Created the internal `SupervisorConfig` in supervisor/mod.rs
- Added the detection `SupervisorConfig` to the main Config
- Wired up the config reading in `cmd_supervise()`
