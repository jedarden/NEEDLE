# Bead bf-5io3: expand_tilde helper function skeleton

## Status: Already Complete

The `expand_tilde` function skeleton already existed in `src/config/mod.rs` at lines 2317-2329.

## Function Details

```rust
/// Expand a leading tilde (~) in a string to the user's home directory.
///
/// This is a stub helper that will be implemented to expand tildes in path strings.
/// Currently returns the path unchanged.
///
/// # Arguments
/// * `path` - The path string to expand
///
/// # Returns
/// * `String` - The path unchanged (stub implementation)
pub fn expand_tilde(path: &str) -> String {
    path.to_string()
}
```

## Acceptance Criteria Met

1. ✅ Function exists with correct signature in src/config/mod.rs
2. ✅ Has doc comment explaining the function purpose
3. ✅ Returns path unchanged (stub implementation)
4. ✅ No unwrap/expect in the code
5. ✅ Compiles with cargo check (part of existing codebase)

## Notes

The function is used in the environment variable override section (lines 2096, 2100, 2104, 2108) to expand tilde paths from config values like `supervisor.heartbeat_path`, `supervisor.socket_path`, `workspace.home`, and `workspace.default`.

The stub implementation simply returns the path unchanged. The full implementation will expand `~` to the user's home directory using `$HOME`.
