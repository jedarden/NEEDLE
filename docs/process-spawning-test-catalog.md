# Process-Spawning Test Catalog

## Overview

This catalog documents all `Command::new` call sites within `#[cfg(test)]` modules in the NEEDLE codebase (`src/`). These are tests that spawn subprocesses during execution.

**Summary:**
- **Total Command::new sites in src/:** 52
- **Command::new sites in test modules:** 12 (23%)
- **Command::new sites in production code:** 40 (77%)

## Categorization

Tests are categorized as:
- **Process-spawning:** Generic process execution (git, echo, sh, etc.)
- **Worker-lifecycle:** Spawns actual NEEDLE worker processes
- **Other:** Command::new for non-process purposes (comments, documentation)

## Test Module Catalog

### 1. src/bead_store/mod.rs (2 sites)

#### Test: `verify_backend_identity_spawn_site_retries_on_etxtbsy`
- **Line:** 2945
- **Category:** Process-spawning
- **Process spawned:** `echo` (with args: `["bead 0.1.1"]`)
- **Purpose:** Tests ETXTBSY retry logic at the verify_backend_identity spawn site
- **Dependencies:** Uses `spawn_with_etxtbsy_retry_sync_child` wrapper, custom `make_etxtbsy_error()` fixture
- **Notes:** Mock bead CLI output for identity verification; creates 3 attempts (2 failures + 1 success)

#### Test: `verify_backend_identity_spawn_site_succeeds_on_first_attempt`
- **Line:** 3059
- **Category:** Process-spawning
- **Process spawned:** `echo` (with args: `["bead 0.1.1"]`)
- **Purpose:** Tests immediate success path without retries
- **Dependencies:** Uses `spawn_with_etxtbsy_retry_sync_child` wrapper
- **Notes:** Verifies healthy binary succeeds on first attempt (no retries needed)

### 2. src/cli/mod.rs (1 site)

#### Test: `is_needle_inner_true_when_env_set`
- **Line:** 5382
- **Category:** Worker-lifecycle
- **Process spawned:** `std::env::current_exe()` (NEEDLE binary itself)
- **Purpose:** Tests NEEDLE_INNER environment variable detection
- **Dependencies:** Uses `spawn_with_etxtbsy_retry` async wrapper, `is_needle_inner()` utility
- **Notes:** Spawns needle binary with `--help` flag to verify env var propagation; uses subprocess to avoid test race conditions

### 3. src/mitosis/timeout_context.rs (6 sites)

#### Test: `git_dirty_paths_filters_untracked`
- **Lines:** 677, 693, 705 (3 sites)
- **Category:** Process-spawning
- **Processes spawned:** `git` (init, add, commit)
- **Purpose:** Tests that untracked files don't appear in dirty path detection
- **Dependencies:** `TempDir`, `git_dirty_paths()` function
- **Notes:** Creates isolated git repo with proper GIT_DIR/GIT_WORK_TREE isolation; verifies clean worktree after commit

#### Test: `git_dirty_paths_captures_modified`
- **Lines:** 743, 759, 769 (3 sites)
- **Category:** Process-spawning
- **Processes spawned:** `git` (init, add, commit)
- **Purpose:** Tests that modified files appear in dirty path detection
- **Dependencies:** `TempDir`, `git_dirty_paths()` function
- **Notes:** Creates isolated git repo, commits file, modifies it, verifies dirty path capture

### 4. src/registry/mod.rs (1 site)

#### Test: `is_pid_alive_returns_false_for_a_zombie`
- **Line:** 609
- **Category:** Process-spawning
- **Process spawned:** `true` (Unix utility)
- **Purpose:** Tests zombie process detection logic (ADR-010 / GitHub issue jedarden/NEEDLE#12)
- **Dependencies:** `is_zombie_linux()` function, `/proc/<pid>/stat` reading
- **Notes:** Spawns short-lived child, waits for zombie state, verifies `is_pid_alive()` returns false for zombies; platform-specific to Linux

### 5. src/supervisor/mod.rs (1 site)

#### Test: `reap_zombie_children_reaps_an_exited_child`
- **Line:** 1340
- **Category:** Process-spawning
- **Process spawned:** `true` (Unix utility)
- **Purpose:** Tests zombie child reaping logic (ADR-010)
- **Dependencies:** `reap_children_matching()` function, `/proc/<pid>/stat` reading
- **Notes:** Exercises reap_children_matching scoped to own PID (not `-1` sweep) to avoid race conditions with other tests; uses real short-lived child process

### 6. src/validation/shipped_work.rs (1 site)

#### Helper Function: `git()`
- **Line:** 200
- **Category:** Process-spawning
- **Process spawned:** `git` (with configurable args)
- **Purpose:** Test helper for git operations in shipped work validation
- **Dependencies:** Used by `init_repo()`, `commit_files()`, `push_upstream()` test helpers
- **Notes:** Not a test itself, but a helper used across multiple tests in the module

## Integration Test Migration Candidates

The following tests spawn real NEEDLE worker processes and should be considered for migration to `tests/integration_tests.rs` if they're not already there:

1. **src/cli/mod.rs::is_needle_inner_true_when_env_set** - Spawns needle binary with env vars
   - Already in lib tests, but spawns real binary
   - Consider moving to integration target for cleaner separation

## Notes on Isolation

Most tests already use proper isolation:
- `tempfile::TempDir` for temporary workspaces
- Environment variable isolation via subprocess spawning
- Git repo isolation via `GIT_DIR` and `GIT_WORK_TREE` environment variables

The mitosis/timeout_context tests specifically demonstrate good isolation patterns that should be followed when adding new process-spawning tests.

## Production Code Command::new Sites (40 total)

The remaining 40 `Command::new` sites are in production code (outside `#[cfg(test)]` modules). These include:
- Git operations (commit_hook, kubectl, workspace_equality, shipped_work, upgrade, mitosis)
- Process spawning utilities (canary, supervisor, strand, dispatch, telemetry)
- CLI utilities (df, sqlite3, bash, sh)
- Worker lifecycle management (supervisor, bead_store)

These production sites are **not** part of this catalog's scope, which focuses solely on test modules.

## Generated: 2026-08-17

This catalog was generated as part of bead needle-5476041b to audit and categorize all process-spawning tests in the NEEDLE codebase.
