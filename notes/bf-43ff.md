# Bead bf-43ff: Add basic supervisor config struct

## Status: Already Implemented

The `SupervisorConfig` struct already exists in `src/config/mod.rs` at line 1317. It was implemented in commit `277f1c6 feat(needle-bf-hkhz): implement supervisor config structure`.

## Verification

All acceptance criteria met:

1. **Config struct exists** - `SupervisorConfig` defined in `src/config/mod.rs`
2. **heartbeat_path field** - `pub heartbeat_path: Option<PathBuf>`
3. **socket_path field** - `pub socket_path: Option<PathBuf>`
4. **Derives Debug and Clone** - `#[derive(Debug, Clone, Serialize, Deserialize)]`
5. **Rustdoc comments present** - Comprehensive module and field documentation

## Code Reference

```rust
/// Supervisor detection configuration.
///
/// Controls how NEEDLE detects whether it's running under a supervisor process.
/// Supervisor detection is used for graceful shutdown and resource cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorConfig {
    /// Path to the supervisor's heartbeat file.
    #[serde(default)]
    pub heartbeat_path: Option<PathBuf>,

    /// Path to the supervisor's control socket (Unix domain socket).
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
}
```

Compilation verified with `cargo check` - no errors.
