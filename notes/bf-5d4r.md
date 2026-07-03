# Verification: heartbeat_path Field on SupervisorDetectionConfig

## Task
Add the heartbeat_path field to SupervisorDetectionConfig.

## Findings
The `heartbeat_path` field already exists on `SupervisorDetectionConfig` (lines 347-354 in `src/config/mod.rs`).

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

## Acceptance Criteria Verification
- ✅ `heartbeat_path` field exists on the struct
- ✅ Field type is `PathBuf` (acceptable per "String or PathBuf")
- ✅ Field is properly typed and public (`pub heartbeat_path: PathBuf`)

## Conclusion
No changes needed — the field was added in a previous bead (likely needle-bf-4duc which added `socket_path`).
