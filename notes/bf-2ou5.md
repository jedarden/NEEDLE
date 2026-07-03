# Bead bf-2ou5: Add Debug and Clone derives to SupervisorDetectionConfig

## Task
Add derive macros to `SupervisorDetectionConfig`.

## Finding
The struct already has the required derives.

## Current State (src/config/mod.rs:346)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorDetectionConfig {
    pub heartbeat_path: PathBuf,
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
}
```

## Verification
- ✅ Struct derives Debug
- ✅ Struct derives Clone
- ✅ Derives are properly formatted

The task was already complete — the derives were added in a prior change.
