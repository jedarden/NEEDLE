# Bead bf-3cyy: Add heartbeat_path field to HealthMonitor struct

## Status: Already Complete

The `heartbeat_path` field already exists in the `HealthMonitor` struct.

### Evidence

**Location:** `src/health/mod.rs` line 109

```rust
pub struct HealthMonitor {
    heartbeat_dir: PathBuf,
    heartbeat_interval: Duration,
    heartbeat_ttl: Duration,
    worker_id: String,
    qualified_id: String,
    workspace: PathBuf,
    started_at: DateTime<Utc>,
    shared_state: Arc<Mutex<SharedHeartbeatState>>,
    shutdown: Arc<AtomicBool>,
    emitter_handle: Option<std::thread::JoinHandle<()>>,
    /// Path to this worker's heartbeat file (computed during construction).
    heartbeat_path: PathBuf,  // <-- Already exists
}
```

### Acceptance Criteria Verification

1. ✓ Field exists: `heartbeat_path: PathBuf` at line 109
2. ✓ Private field (no `pub` keyword)
3. ✓ Proper Rust type: `PathBuf`
4. ✓ Compiles successfully: `cargo check` passes with no errors

### Additional Implementation Details

- Initialized in constructor `new()` at line 139:
  ```rust
  let heartbeat_path = heartbeat_dir.join(format!("{}.json", qualified_id));
  ```

- Public accessor method at lines 336-345:
  ```rust
  pub fn heartbeat_path(&self) -> PathBuf {
      self.heartbeat_path.clone()
  }
  ```

## Conclusion

No changes required. The task is already complete.
