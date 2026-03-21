# NEEDLE — Project Conventions

## Overview

NEEDLE is a single Rust binary that orchestrates AI agent workers against a
bead queue. Workers select beads, dispatch a headless agent CLI, and handle
outcomes deterministically.

## Module Dependency Graph

Strict layering — no upward imports:

```
cli → worker → strand / claim / prompt / dispatch / outcome
                    ↓               ↓
             bead_store / telemetry / health
                    ↓
              config / types  (leaf — no internal deps)
```

Circular dependencies are a build error. The compiler enforces this.

## Code Conventions

- **Edition:** Rust 2021
- **MSRV:** stable (tracked in `rust-toolchain.toml`)
- **Error handling:** `anyhow::Result` for fallible functions; `thiserror` for
  domain-specific error types that need to be matched by callers
- **Async:** `tokio` runtime; use `async fn` for I/O-bound code
- **Logging:** `tracing` macros (`tracing::info!`, `tracing::debug!`, etc.)
  — never `println!` or `eprintln!` in library code
- **Telemetry:** All state transitions go through `telemetry::Telemetry::emit`
  — never interleave structured events with agent stdout/stderr

## Stub Convention

Unimplemented functions use `todo!("ModuleName::function_name")` with the
bead ID that owns the implementation in a comment above.

## Test Convention

- Unit tests live in `#[cfg(test)]` blocks in the same file
- Integration tests go in `tests/`
- Use `Config::default_for_test()` for test fixtures

## Bead Workflow

1. Pick up a bead from `br ready`
2. Claim it: `br update <id> --status in_progress --assignee <worker-name>`
3. Implement, build, test, clippy
4. Commit: `git add ... && git commit -m "feat(<bead-id>): description"`
5. Push: `git push`
6. Close: `br close <id> --body "Summary"`
