# Bead bf-3gs8: Add heartbeat_path field to SupervisorDetectionConfig

## Verification

The `heartbeat_path` field already exists in `SupervisorDetectionConfig` at line 349 of `src/config/mod.rs`.

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

## Acceptance Criteria Met

- Field named `heartbeat_path` exists in `SupervisorDetectionConfig` ✓
- Type is `PathBuf` ✓
- Field is public (`pub`) ✓
- Compiles without errors (verified with `cargo check` and `cargo clippy`) ✓

The field was likely added in a prior commit (see recent commits mentioning supervisor config scaffolding).
