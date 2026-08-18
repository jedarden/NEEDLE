# Process-Spawning Test Catalog

**Generated:** 2026-08-17  
**Scope:** All `Command::new` call sites in `#[cfg(test)]` modules within `src/`

## Summary Statistics

- **Total Command::new sites in test modules:** 13
- **Test helper functions:** 2
- **Test functions using Command::new:** 11
- **Categories:**
  - **Process-spawning:** 9 tests (generic process execution)
  - **Worker-lifecycle:** 1 test (spawns actual NEEDLE workers)
  - **Test helpers:** 2 functions (used by multiple tests)

## Detailed Catalog

### Process-Spawning Tests (9)

These tests spawn generic processes (git, echo, true) for testing purposes. They do NOT spawn actual NEEDLE workers.

#### 1. `src/registry/mod.rs:is_pid_alive_returns_false_for_a_zombie`
- **Line:** 609
- **Process:** `true` (short-lived binary)
- **Purpose:** Validates zombie process detection (ADR-010 / GitHub issue jedarden/NEEDLE#12)
- **Category:** Process-spawning
- **Dependencies:** Linux-specific (`#[cfg(target_os = "linux")]`)
- **Notes:** Spawns `true`, waits for it to become zombie state, verifies `is_pid_alive` treats it as not-alive

#### 2. `src/supervisor/mod.rs:reap_zombie_children_reaps_an_exited_child`
- **Line:** 1734
- **Process:** `true` (short-lived binary)
- **Purpose:** Tests zombie child reaping logic
- **Category:** Process-spawning
- **Dependencies:** Unix-specific (`#[cfg(unix)]`)
- **Notes:** Spawns `true`, waits for zombie state, verifies `reap_children_matching` reaps correctly

#### 3. `src/bead_store/mod.rs:verify_backend_identity_spawn_site_retries_on_etxtbsy`
- **Line:** 2945
- **Process:** `echo beaded 0.1.1`
- **Purpose:** Tests ETXTBSY retry logic at `verify_backend_identity` spawn site
- **Category:** Process-spawning
- **Dependencies:** `spawn_with_etxtbsy_retry_sync_child`, `make_etxtbsy_error`
- **Notes:** Simulates busy binary with ETXTBSY errors, validates retry succeeds after transient failures

#### 4. `src/bead_store/mod.rs:verify_backend_identity_spawn_site_succeeds_on_first_attempt`
- **Line:** 3059
- **Process:** `echo beaded 0.1.1`
- **Purpose:** Tests immediate success path at `verify_backend_identity` spawn site
- **Category:** Process-spawning
- **Dependencies:** `spawn_with_etxtbsy_retry_sync_child`
- **Notes:** Validates no unnecessary retries when binary spawns successfully on first attempt

#### 5. `src/mitosis/timeout_context.rs:git_dirty_paths_filters_untracked`
- **Lines:** 677, 693, 705 (3 git invocations)
- **Process:** `git` (init, add, commit)
- **Purpose:** Validates `git_dirty_paths` filters untracked files correctly
- **Category:** Process-spawning
- **Dependencies:** `tokio::process::Command`, `TempDir`
- **Notes:** Creates isolated git repo with `GIT_DIR`/`GIT_WORK_TREE` env vars, verifies untracked files excluded

#### 6. `src/mitosis/timeout_context.rs:git_dirty_paths_captures_modified`
- **Lines:** 743, 759, 769 (3 git invocations)
- **Process:** `git` (init, add, commit)
- **Purpose:** Validates `git_dirty_paths` captures modified files correctly
- **Category:** Process-spawning
- **Dependencies:** `tokio::process::Command`, `TempDir`
- **Notes:** Creates isolated git repo, modifies committed file, verifies dirty path detection

### Worker-Lifecycle Tests (1)

These tests spawn actual NEEDLE worker binaries.

#### 1. `src/cli/mod.rs:is_needle_inner_true_when_env_set`
- **Line:** 5398
- **Process:** `std::env::current_exe()` (needle binary)
- **Purpose:** Tests `NEEDLE_INNER` environment variable detection
- **Category:** Worker-lifecycle
- **Dependencies:** `spawn_with_etxtbsy_retry`, `std::env::current_exe`
- **Notes:** Spawns needle binary with `NEEDLE_INNER=1` env var, validates detection without mutating test process env

### Test Helper Functions (2)

These are helper functions used by multiple tests, not test functions themselves.

#### 1. `src/commit_hook.rs:tests::run_git`
- **Line:** 294
- **Process:** `git`
- **Purpose:** Helper function to run git commands in test repos
- **Usage:** Used by tests in commit_hook test module
- **Dependencies:** Test fixtures created by `create_git_repo` helper

#### 2. `src/validation/shipped_work.rs:tests::git`
- **Line:** 200
- **Process:** `git`
- **Purpose:** Helper function to run git commands in validation tests
- **Usage:** Used by `init_repo`, `push_upstream`, and other validation tests
- **Dependencies:** `TempDir`, test fixtures

## Files by Category

### Process-spawning tests (6 files)
- `src/registry/mod.rs` - 1 test
- `src/supervisor/mod.rs` - 1 test
- `src/bead_store/mod.rs` - 2 tests
- `src/mitosis/timeout_context.rs` - 2 tests (6 Command::new calls total)

### Worker-lifecycle tests (1 file)
- `src/cli/mod.rs` - 1 test

### Test helpers (2 files)
- `src/commit_hook.rs` - 1 helper
- `src/validation/shipped_work.rs` - 1 helper

## Integration Test Migration Recommendations

Based on this audit, the following tests should be candidates for moving to the `tests/` integration test target:

### High Priority (Move to integration tests)
1. **Worker-lifecycle test:**
   - `src/cli/mod.rs:is_needle_inner_true_when_env_set` - Spawns actual needle binary, better suited for integration testing

### Medium Priority (Consider moving)
2. **Git-dependent tests:**
   - `src/mitosis/timeout_context.rs:git_dirty_paths_filters_untracked`
   - `src/mitosis/timeout_context.rs:git_dirty_paths_captures_modified`
   - These tests spawn real git processes and test filesystem interactions

### Low Priority (Keep in lib tests)
3. **Process-spawning tests:**
   - All other tests in this category use simple utilities (`true`, `echo`) and test internal logic
   - These are appropriate as unit tests in `--lib` target

## Additional Notes

### Test Isolation Patterns
- All git-spawning tests use proper isolation (temp dirs, explicit `GIT_DIR`/`GIT_WORK_TREE`)
- Worker-lifecycle test uses `std::env::current_exe()` to spawn the needle binary under test
- Process-spawning tests use hermetic utilities (`true`, `echo`) that don't depend on external state

### Platform-Specific Tests
- `src/registry/mod.rs:is_pid_alive_returns_false_for_a_zombie` - Linux only (`#[cfg(target_os = "linux")]`)
- `src/supervisor/mod.rs:reap_zombie_children_reaps_an_exited_child` - Unix only (`#[cfg(unix)]`)

## Related Documentation

- [Test Isolation Policy](../testing-isolation-patterns.md) - Comprehensive documentation on test isolation patterns
- [ADR-006](../adr/006-test-contamination-incident.md) - Postmortem of the 2026-07-20 test contamination incident
- [ADR-010](../adr/010-zombie-process-detection.md) - Zombie process detection fix

---

**Total Command::new sites cataloged:** 13  
**Test files affected:** 7  
**Test functions affected:** 11  
**Test helper functions:** 2
