# Bead bf-2ou5: Add Debug and Clone derives to SupervisorDetectionConfig

## Finding

The `SupervisorDetectionConfig` struct in `src/config/mod.rs` already has `Debug` and `Clone` derives.

## Current State (line 346)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorDetectionConfig {
    /// Path to the supervisor's heartbeat file for liveness detection.
    pub heartbeat_path: PathBuf,

    /// Optional Unix domain socket path for communication with the supervisor.
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
}
```

## Acceptance Criteria

All criteria met:
- ✅ Struct derives Debug
- ✅ Struct derives Clone
- ✅ Derives are properly formatted (Debug, Clone, Serialize, Deserialize)

No code changes were needed.
