# Testing Isolation Patterns

This document describes the testing isolation patterns used across the NEEDLE codebase to prevent test contamination of production environments.

## Overview

NEEDLE tests often spawn real subprocesses (the `needle` binary itself) which can interact with bead stores, workspaces, and configuration. Without proper isolation, tests can:

- Create phantom beads in real repositories
- Contaminate production bead stores
- Scan and modify real user workspaces
- Leave orphaned state files

## The Three Pillars of Isolation

### 1. HOME Directory Isolation (Required)

All tests that spawn the `needle` binary MUST isolate the HOME directory. The Explore strand (enabled by default) scans `workspace_root` (defaulting to `$HOME`) for bead workspaces.

**Pattern: Set HOME in test environment**

```rust
let temp_home = TempHome::new()?;
cmd.env("HOME", temp_home.path());
```

**Why this matters:**
- The Explore strand auto-discovers workspaces under HOME
- Without isolation, test processes see real repos
- Phantom beads get created under test worker IDs
- Recovery requires manual bead cleanup or database repair

### 2. Workspace/Tempdir Cleanup

All test-created directories MUST be cleaned up, even on panic. Use fixtures that implement Drop for automatic cleanup.

**Pattern: RAII fixtures**

```rust
let temp_home = TempHome::new()?; // Auto-cleanup on drop
let workspace = temp_home.create_workspace("test-ws")?;
```

**Implementation:**
- Use `tempfile::TempDir` for temporary directories
- Implement Drop for custom fixtures
- Never rely on test completion for cleanup

### 3. Process Guarding

Long-running subprocesses MUST be wrapped in guards that ensure cleanup.

**Pattern: ProcessGuard**

```rust
let child = Command::new("needle").spawn()?;
let mut guard = ProcessGuard::new(child);
// Guard kills and waits on drop, preventing zombies
```

**Why this matters:**
- Tests may panic before explicit cleanup
- Zombies accumulate and consume system resources
- Guards ensure cleanup in all code paths

## Available Infrastructure

### Fixtures in `tests/adapter_validation_tests.rs`

- **`TempHome`**: Isolated HOME directory fixture
  - Creates standard subdirectories (`.cache`, `.config`)
  - Auto-cleanup on drop
  - Methods for creating workspaces and configs

- **`TempWorkspace`**: Temporary workspace fixture
  - Creates directory structure
  - Initializes git repository
  - Creates bead store configuration

- **`TempFile`**: Temporary file fixture
  - Creates files with custom content
  - Auto-cleanup on drop
  - Useful for test prompts and configs

### Process Management in `tests/process_guard.rs`

- **`ProcessGuard`**: Child process cleanup guard
  - Automatic kill and wait on drop
  - Explicit control methods
  - Prevents zombie processes

### Workspace Management in `tests/workspace_fixtures.rs`

- **Mock workspace structures**: For testing workspace discovery
- **Scenario builders**: For creating complex test scenarios
- **Mock bead stores**: For testing bead store interactions

## When to Use Each Pattern

| Test Type | HOME Isolation | Workspace Cleanup | Process Guard |
|-----------|----------------|-------------------|---------------|
| Unit tests (no subprocess) | Optional | Optional | Not needed |
| Integration tests (subprocess) | **Required** | **Required** | **Required** |
| Adapter validation | **Required** | **Required** | **Required** |
| Benchmark tests | Recommended | **Required** | **Required** |
| Property tests | **Required** | **Required** | Conditional |

## Examples

### Basic Adapter Validation Test

```rust
use adapter_validation_tests::*;

#[test]
fn test_claude_adapter_config() {
    // 1. Isolate HOME
    let temp_home = TempHome::new().unwrap();
    
    // 2. Create test workspace
    let workspace = temp_home.create_workspace("test-ws").unwrap();
    
    // 3. Validate adapter config
    let adapter_yaml = create_test_adapter("claude-sonnet");
    let validation = validate_adapter_config(&adapter_yaml);
    
    assert!(validation.is_valid);
    // Auto-cleanup happens when fixtures drop
}
```

### Integration Test with Subprocess

```rust
#[test]
fn test_worker_with_real_process() {
    // 1. Isolate HOME
    let temp_home = TempHome::new().unwrap();
    
    // 2. Create workspace and bead store
    let workspace = temp_home.create_workspace("test-ws").unwrap();
    workspace.create_bead_store().unwrap();
    
    // 3. Spawn needle process with isolated HOME
    let child = Command::new("needle")
        .env("HOME", temp_home.path())
        .arg("worker")
        .arg("--once")
        .spawn()
        .unwrap();
    
    // 4. Wrap in process guard
    let mut guard = ProcessGuard::new(child);
    
    // 5. Test logic
    let result = guard.wait().unwrap();
    assert!(result.success());
    
    // 6. All cleanup happens automatically on drop
}
```

## Testing Checklist

Before committing a new test:

- [ ] Does the test spawn any `needle` subprocess? → **Must isolate HOME**
- [ ] Does the test create temporary directories? → **Use fixture with Drop**
- [ ] Does the test spawn long-running processes? → **Use ProcessGuard**
- [ ] Does the test access real bead stores? → **Use mock store instead**
- [ ] Does the test modify real repositories? → **Isolate to temp workspace**

## Anti-Patterns to Avoid

### ❌ Never Use Real HOME

```rust
// BAD: Test contaminates real environment
let home = env::var("HOME").unwrap();
cmd.env("HOME", home);
```

### ❌ Never Skip Process Guarding

```rust
// BAD: Process becomes zombie if test panics
let child = Command::new("needle").spawn()?;
// Test logic...
child.wait()?;
```

### ❌ Never Rely on Manual Cleanup

```rust
// BAD: Cleanup never happens if test panics
let temp_dir = tempdir()?;
// Test logic...
fs::remove_dir_all(temp_dir)?;
```

## Historical Context

The importance of these patterns was established through several incidents:

- **2026-07-20**: Non-isolated test created ~284 phantom beads across ~22 repos
- **2026-08-05**: In-process test leaked into bead-forge store, truncated `.beads/issues.jsonl` to 0 bytes (2302 beads recovered)
- **2026-08-09**: Duplicate bead claims from concurrent workers without proper serialization

See ADR-006, ADR-015, and the testing isolation policy in CLAUDE.md for full postmortems.

## Further Reading

- `tests/adapter_validation_tests.rs` - Comprehensive isolation fixtures
- `tests/process_guard.rs` - Process cleanup implementation
- `tests/workspace_fixtures.rs` - Workspace management utilities
- `docs/adr/006-testing-isolation-policy.md` - ADR on testing isolation
- `docs/adr/015-concurrent-same-repo-worker-isolation.md` - ADR on worker isolation
