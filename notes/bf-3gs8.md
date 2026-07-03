# Bead bf-3gs8: Add heartbeat_path field to SupervisorDetectionConfig

## Status: Already Complete

The `heartbeat_path` field already exists in `SupervisorDetectionConfig` at `src/config/mod.rs:346-354`.

## Verification

Field meets all acceptance criteria:
- Named `heartbeat_path` ✓
- Type: `PathBuf` ✓
- Public: `pub` ✓
- Code compiles without errors ✓

## Code Reference

```rust
/// Supervisor detection configuration.
///
/// Used to detect if a supervisor process is running via heartbeat files
/// or Unix domain sockets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorDetectionConfig {
    /// Path to the supervisor's heartbeat file for liveness detection.
    pub heartbeat_path: PathBuf,

    /// Optional Unix domain socket path for communication with the supervisor.
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
}
```

No code changes were required.
