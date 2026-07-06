# Heartbeat Cleanup Function Examination (bf-5uagm)

## Location

**File:** `src/peer/mod.rs`
**Function:** `remove_heartbeat_file`
**Lines:** 258-274

## Current Implementation

```rust
/// Remove a heartbeat file (best-effort).
fn remove_heartbeat_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::debug!(path = %path.display(), "removed stale heartbeat file");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Already gone — not an error.
            tracing::debug!(path = %path.display(), "heartbeat file already removed");
            Ok(())
        }
        Err(e) => {
            Err(e).with_context(|| format!("failed to remove heartbeat file: {}", path.display()))
        }
    }
}
```

## Usage Context

The function is called from `handle_crashed_peer` (line 238) during crashed peer cleanup:

```rust
async fn handle_crashed_peer(&self, peer: &StalePeer) -> Result<bool> {
    // 1. Release the claimed bead (if any) - with proper error handling
    if let Some(ref bead_id) = peer.current_bead {
        match self.store.release(bead_id).await {
            Ok(()) => { ... }
            Err(e) => {
                tracing::warn!(...);
                // Continues despite release failure
            }
        }
    }

    // 2. Remove the heartbeat file.
    remove_heartbeat_file(&peer.heartbeat_file)?;  // <-- Fails entire operation on error

    // 3. Deregister from the worker registry - with proper error handling
    if let Err(e) = self.registry.deregister(dereg_id) {
        tracing::warn!(...);
    }

    Ok(bead_released)
}
```

## Current Error Handling Status

**Has partial error handling:**
- ✅ `NotFound` errors are handled (file already gone)
- ✅ Uses `.with_context()` for error messages
- ❌ Other errors (permission denied, directory not writable, etc.) return `Err`, which propagates up and terminates the entire cleanup operation

## What Needs to Be Added

The `remove_heartbeat_file` function should follow the same pattern as steps 1 and 3 in `handle_crashed_peer`:

1. Log a warning on failure (non-critical operation)
2. Return `Ok(())` instead of `Err(e)` to allow cleanup to continue
3. Preserve the `NotFound` handling (already correct)

This matches the pattern established in the recent commit (03ecd40 "feat(needle-bf-14r4): add error handling to heartbeat cleanup") which followed this same best-effort cleanup approach.

## Tests

The function already has comprehensive tests:
- Line 714-717: `remove_heartbeat_file_nonexistent_is_ok` - verifies NotFound handling
- Line 720-728: `remove_heartbeat_file_removes_existing` - verifies successful removal
