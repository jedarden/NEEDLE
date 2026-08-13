# Explore Isolation Comment Templates

Standardized comment templates for documenting Explore strand isolation in in-process Worker tests.

## Purpose

In-process tests that build a `Worker` directly (e.g., via `test_config()` helper) **must pin the Explore strand's scan root** to prevent scanning real user directories.

Without explicit pinning, tests inherit `ExploreConfig::default()` — `workspaces: []` (auto-discover) with `workspace_root` from `default_workspace_root()` → `dirs_or_home("")`, the real home directory. This causes tests to scan and mutate production bead stores.

## Why This Matters

### 2026-08-05 Contamination Incident

The `test_config()` helper in `tests/integration_tests.rs` isolated `workspace.default` and `workspace.home` but **not** `strands.explore`. An orphaned local `integration_tests` binary scanned bead-forge's live store and:
- Mutated 2302 beads to `in_progress` under assignee `echo-test-test-worker`
- Truncated `.beads/issues.jsonl` to 0 bytes
- Required recovery from git history

The subprocess isolation clause (`cmd.env("HOME", temp_dir.path())`) did **not** cover this shape, because the test never spawned a subprocess — it built the Worker in-process.

## Template Variants

### Short Variant (Single-Line)

```rust
// Isolate Explore scan root to prevent real user directory scans (see CLAUDE.md Test Isolation Policy)
```

### Long Variant (Multi-Line)

```rust
// ISOLATION REQUIRED: In-process Worker tests must pin Explore strand's scan root.
//
// Without explicit pinning, ExploreConfig::default() resolves workspace_root to the real
// home directory via default_workspace_root() → dirs_or_home(""), causing tests to scan
// and mutate production bead stores.
//
// 2026-08-05 incident: test_config() isolated workspace.default/home but not strands.explore,
// letting an orphaned integration_tests binary mutate 2302 beads to in_progress under
// assignee echo-test-test-worker and truncate .beads/issues.jsonl to 0 bytes (recovered from git).
//
// See CLAUDE.md Test Isolation Policy for full details.
```

### With Context (For Test Setup Functions)

```rust
// ISOLATION REQUIRED: In-process Worker tests must pin Explore strand's scan root.
//
// This applies to tests that build a Worker in-process (e.g., via test_config() helper).
// Subprocess tests use cmd.env("HOME", ...) instead; this clause covers the in-process
// shape where HOME isolation does not apply (no subprocess is ever spawned).
//
// Without explicit pinning, ExploreConfig::default() resolves workspace_root to the real
// home directory via default_workspace_root() → dirs_or_home(""), causing tests to scan
// and mutate production bead stores.
//
// 2026-08-05 incident: test_config() isolated workspace.default/home but not strands.explore,
// letting an orphaned integration_tests binary mutate 2302 beads to in_progress under
// assignee echo-test-test-worker and truncate .beads/issues.jsonl to 0 bytes (recovered from git).
//
// See CLAUDE.md Test Isolation Policy for full details.
config.strands.explore.workspace_root = temp_home.to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

## Style Guide

### When to Use Each Variant

| Situation | Template | Rationale |
|-----------|----------|-----------|
| Field initialization near other test setup | Short | Avoids cluttering straightforward setup code |
| Standalone isolation block or test helper | Long | Provides full context for readers unfamiliar with the policy |
| Test setup functions that create Workers | With Context | Explains why subprocess isolation doesn't apply here |

### Placement Guidelines

1. **Immediately before** the isolation code (not after — comment should precede the action it explains)
2. **No blank line** between comment and code — keeps them visually paired
3. **Consistent indentation** with the isolated code

### Example Usage

```rust
#[tokio::test]
async fn test_worker_claim() {
    let temp_home = TempDir::new().unwrap();

    // ISOLATION REQUIRED: In-process Worker tests must pin Explore strand's scan root.
    config.strands.explore.workspace_root = temp_home.to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let worker = Worker::new(config).await.unwrap();
    // ... test continues
}
```

### Anti-Patterns to Avoid

❌ **Don't place the comment after the code:**
```rust
config.strands.explore.workspace_root = temp_home.to_path_buf();
// ISOLATION REQUIRED: ...  // ← Wrong: comment follows the action
```

❌ **Don't separate comment from code with a blank line:**
```rust
// ISOLATION REQUIRED: ...

config.strands.explore.workspace_root = temp_home.to_path_buf();  // ← Wrong: gap breaks visual pairing
```

❌ **Don't use the short variant in test helpers without surrounding context:**
```rust
pub fn test_config() -> Config {
    // Isolate Explore scan root to prevent real user directory scans  // ← Wrong: insufficient context for reusable helper
    let mut config = Config::default();
    config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
    // ...
}
```

## References

- **CLAUDE.md Test Isolation Policy**: Full policy documentation with subprocess and in-process clauses
- **ADR-006**: Complete postmortem of the 2026-08-05 contamination incident and recovery
- **docs/testing-isolation-patterns.md**: Comprehensive coverage of all 4 isolation patterns with decision trees and examples

## Maintenance

When updating this template:
1. Preserve the core incident reference (2026-08-05, 2302 beads) — it's the canonical example
2. Keep references to CLAUDE.md and ADR-006 — they anchor the policy in documented history
3. Update code snippets if the isolation pattern changes (e.g., field renames in `ExploreConfig`)
4. Add new variants only if they serve a distinct communicative purpose (avoid template proliferation)
