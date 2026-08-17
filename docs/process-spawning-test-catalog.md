# Process-Spawning Test Catalog (lib target)

**Generated:** 2026-08-15  
**Scope:** All `Command::new` sites in `src/` (--lib target)  
**Total sites:** 49

## Summary

- **Total Command::new sites:** 49
- **In test modules (#[cfg(test)]):** 6
- **In production code:** 43
- **Comments/docs only:** 2

## Categories

### Process-Spawning Tests
Tests that spawn external processes (git, cargo, bash, true, etc.) but do NOT spawn full NEEDLE workers.

### Worker-Lifecycle Tests
Tests that spawn actual NEEDLE worker binaries (needle, testing binaries).

### Production Code
Command::new sites used in runtime code (not tests).

### Comments/Documentation
Command::new references in comments or documentation only.

---

## Detailed Catalog

### 1. src/commit_hook.rs (Production)

**Lines 109, 150, 171, 237, 294: Command::new("git")**

- **Line 109:** `inject_bead_id_trailer()` - Production function
  - Spawns: `git commit --amend --no-edit --trailer "Bead-Id: <id>"`
  - Purpose: Inject bead ID trailer into commits

- **Line 150:** `git_head()` - Production function
  - Spawns: `git rev-parse HEAD`
  - Purpose: Get current HEAD SHA

- **Line 171:** `git_head_subject()` - Production function
  - Spawns: `git log -1 --format=%s`
  - Purpose: Get HEAD commit subject

- **Line 237:** `inject_bead_id_trailer()` - Production function (duplicate)
  - Spawns: `git rev-parse HEAD`
  - Purpose: Get current HEAD for validation

**Line 294: run_git() - Test helper function**
- **Location:** Test module (#[cfg(test)])
- **Function:** `run_git()`
- **Test functions using it:**
  - `already_has_trailer_logic()` (unit test)
  - `empty_head_means_no_op()` (unit test)
  - `concurrent_inject_never_cross_tags()` (#[tokio::test])
- **Spawns:** `git` with various arguments
- **Purpose:** Git operations in test repositories
- **Category:** Process-spawning

### 2. src/workspace_equality.rs (Production)

**Line 368: Command::new("bf")**

- **Function:** `check_workspace_equality()`
- **Purpose:** Check bead workspace equality
- **Spawns:** `bf list --json`
- **Category:** Production code (not a test)

### 3. src/test_runner.rs (Production)

**Line 439: Command::new("cargo")**

- **Function:** `run_tests()` (impl TestRunner)
- **Purpose:** Run cargo test commands
- **Spawns:** `cargo test` with workspace and args
- **Category:** Production code (not a test, though used for testing)

### 4. src/canary/mod.rs (Production - Canary Testing)

**Line 386: Command::new(testing_binary)**

- **Function:** `run_single_test()` (impl CanaryRunner)
- **Purpose:** Spawn testing binary for canary validation
- **Spawns:** `needle run --workspace <canary> --identifier <id> --count 1`
- **Category:** Worker-lifecycle (spawns NEEDLE worker)
- **Note:** Production code used for testing, but spawns actual workers

**Line 490: Command::new(&binary)**

- **Function:** `get_actual_outcome()` (impl CanaryRunner)
- **Purpose:** Query bead store for test results
- **Spawns:** `bead show <id> --json` (bead-rs or bf)
- **Category:** Process-spawning (bead CLI, not needle worker)

### 5. src/cargo_test.rs (Production)

**Line 699: Command::new("cargo")**

- **Function:** `run()` (impl CargoTest)
- **Purpose:** Run cargo test with timeout handling
- **Spawns:** `cargo test` with workspace and test args
- **Category:** Production code (test orchestration, not unit test)

### 6. src/mitosis/timeout_context.rs (Test Module)

**Lines 413, 677, 693, 705, 743, 759, 769: Command::new("git")**

**Line 413:** `run_git_raw()` - Helper function (production)
- **Purpose:** Run git commands in workspace
- **Spawns:** `git` with various args
- **Category:** Production helper

**Test module (#[cfg(test)]) lines 677, 693, 705, 743, 759, 769:**

- **Function:** `run_git()` - Test helper
- **Test functions using it:**
  - `git_dirty_paths_filters_untracked()` (#[tokio::test])
  - `git_dirty_paths_includes_unstaged()` (#[tokio::test])
  - `git_dirty_paths_includes_unstaged_duplicate_paths()` (#[tokio::test])
  - `compute_committed_work_returns_none_on_same_sha()` (#[tokio::test])
  - `compute_committed_work_counts_commits()` (#[tokio::test])
  - `compute_committed_work_returns_summary()` (#[tokio::test])
- **Spawns:** `git` commands (init, config, add, commit, status, log, diff, rev-list)
- **Purpose:** Git operations in test repositories
- **Category:** Process-spawning

### 7. src/registry/mod.rs (Test Module)

**Line 609: Command::new("true")**

- **Location:** Test module (#[cfg(test)])
- **Test function:** `is_pid_alive_returns_false_for_a_zombie()`
- **Purpose:** Create zombie process for testing
- **Spawns:** `true` command (immediate exit)
- **Category:** Process-spawning
- **Note:** Creates zombie process to validate is_pid_alive() behavior

### 8. src/supervisor/mod.rs (Production + Test)

**Line 645: Command::new(&worker_binary)**

- **Function:** `spawn_worker()` (impl Supervisor)
- **Purpose:** Spawn NEEDLE worker process
- **Spawns:** `needle run --workspace <ws> --agent <agent> --identifier <id> --count 1`
- **Category:** Worker-lifecycle
- **Note:** Production code that spawns actual workers

**Line 1120: Command::new("true")**

- **Location:** Test module (#[cfg(test)])
- **Test function:** `reap_zombie_children_reaps_an_exited_child()`
- **Purpose:** Create zombie process for testing reap_children_matching()
- **Spawns:** `true` command (immediate exit)
- **Category:** Process-spawning
- **Note:** Tests zombie child reaping logic (ADR-010)

### 9. src/telemetry/mod.rs (Production)

**Line 2816: Command::new("sh")**

- **Function:** `run_hook()` (private helper in webhook sink)
- **Purpose:** Execute shell hook commands
- **Spawns:** `sh -c <command>`
- **Category:** Production code
- **Note:** Runs in background writer task for webhook telemetry

### 10. src/bead_store/mod.rs (Production)

**Lines 110, 173: Command::new(binary)**

- **Line 110:** `verify_backend_identity()` - Production function
  - Spawns: `<binary> --version`
  - Purpose: Verify bead CLI backend identity

- **Line 173:** `verify_bead_rs_capabilities()` - Production function
  - Spawns: `<binary> capabilities --profile native-v1`
  - Purpose: Verify bead-rs capabilities

### 11. src/strand/weave.rs (Production)

**Line 632: Command::new("bash")**

- **Function:** Production code
- **Purpose:** Spawn bash for strand operations
- **Category:** Production code

### 12. src/strand/unravel.rs (Production)

**Line 525: Command::new("bash")**

- **Function:** Production code
- **Purpose:** Spawn bash for strand operations
- **Category:** Production code

### 13. src/strand/reflect.rs (Production)

**Line 150: Command::new("bash")**

- **Function:** Production code
- **Purpose:** Spawn bash for strand operations
- **Category:** Production code

### 14. src/strand/pulse.rs (Production)

**Line 155: Command::new("sh")**

- **Function:** Production code
- **Purpose:** Spawn shell for pulse operations
- **Category:** Production code

### 15. src/dispatch/mod.rs (Production)

**Lines 1027, 1120, 1993, 2014: Command::new("bash")**

- **Functions:** Production code
- **Purpose:** Spawn bash for dispatch operations
- **Category:** Production code

### 16. src/validation/mod.rs (Production)

**Line 337: Command::new("sh")**

- **Function:** Production code
- **Purpose:** Spawn shell for validation operations
- **Category:** Production code

### 17. src/validation/predispatch.rs (Production)

**Line 139: Command::new(&bin)**

- **Function:** Production code
- **Purpose:** Spawn binary for predispatch validation
- **Category:** Production code

### 18. src/validation/shipped_work.rs (Production)

**Lines 170, 200: Command::new("git")**

- **Functions:** Production code
- **Purpose:** Git operations for validation
- **Category:** Production code

### 19. src/upgrade/mod.rs (Production)

**Line 627: Command::new(&stable_path)**

- **Function:** Production code
- **Purpose:** Spawn stable binary during upgrade
- **Category:** Production code

### 20. src/kubectl.rs (Production)

**Line 167: Command::new("kubectl")**

- **Function:** Production code
- **Purpose:** Kubectl operations
- **Category:** Production code

### 21. src/util.rs (Production)

**Line 225: Command::new("timeout")**

- **Function:** Production code
- **Purpose:** Timeout command wrapper
- **Category:** Production code

### 22. src/cli/mod.rs (Production)

**Lines 3480, 3919, 5265: Command::new()**

- **Line 3480:** `sqlite3` - Production CLI command
- **Line 3919:** `df` - Production CLI command  
- **Line 5265:** `exe_path` - Production CLI command
- **Category:** Production code

### 23. src/tmux_socket.rs (Production)

**Line 20: Command::new("tmux")**

- **Function:** Production code
- **Purpose:** Tmux socket operations
- **Category:** Production code

**Line 8: Comment**

- **Purpose:** Documentation comment
- **Category:** Comments/Documentation

### 24. src/bead_store/cli_store.rs (Production)

**Line 165: Command::new(&binary)**

- **Function:** Production code
- **Purpose:** Bead CLI operations
- **Category:** Production code

### 25. src/config/mod.rs (Comment)

**Line 244: Comment**

- **Purpose:** Documentation comment
- **Category:** Comments/Documentation

---

## Test Module Summary

### Process-Spawning Tests (6 sites)

1. **src/commit_hook.rs (line 294):** `run_git()` helper
   - Tests: 3 test functions
   - Spawns: git

2. **src/mitosis/timeout_context.rs (lines 677, 693, 705):** Direct `tokio::process::Command::new("git")` in tests
   - Tests: 2 test functions (git_dirty_paths_filters_untracked, git_dirty_paths_includes_unstaged)
   - Spawns: git

3. **src/registry/mod.rs (line 609):** `is_pid_alive_returns_false_for_a_zombie()`
   - Tests: 1 test function
   - Spawns: true

4. **src/supervisor/mod.rs (line 1120):** `reap_zombie_children_reaps_an_exited_child()`
   - Tests: 1 test function
   - Spawns: true

**Total process-spawning test sites:** 6

### Worker-Lifecycle Tests (0 in lib)

No tests in the lib target spawn actual NEEDLE workers. Worker spawning happens only in production code:
- `src/canary/mod.rs` (line 386): Canary runner spawns testing binary
- `src/supervisor/mod.rs` (line 645): Supervisor spawns worker binary

### Tests That Should Move to Integration Target

The following test modules spawn processes and should be moved to `tests/` (integration target):

1. **src/commit_hook.rs (tests module):** 3 tests spawning git
   - `already_has_trailer_logic()`
   - `empty_head_means_no_op()`
   - `concurrent_inject_never_cross_tags()`

2. **src/mitosis/timeout_context.rs (tests module):** 2 tests spawning git
   - `git_dirty_paths_filters_untracked()`
   - `git_dirty_paths_includes_unstaged()`

3. **src/registry/mod.rs (tests module):** 1 test spawning true (zombie test)
   - `is_pid_alive_returns_false_for_a_zombie()`

4. **src/supervisor/mod.rs (tests module):** 1 test spawning true (zombie test)
   - `reap_zombie_children_reaps_an_exited_child()`

**Recommended action:** Move these 7 test functions to `tests/` target to:
- Isolate process-spawning tests from pure unit tests
- Allow cargo test --lib to run without external dependencies
- Match project policy for process-spawning tests

---

## Verification

### Command to verify counts

```bash
# Total Command::new in src/
grep -rn "Command::new" src/ --include="*.rs" | wc -l
# Expected: 49

# In test modules
grep -rn "#\[cfg(test)\]" src/ --include="*.rs" -A 200 | grep -c "Command::new"
# Expected: 6 (actual test module Command::new sites)
```

### Files with no process-spawning in tests

The following files have Command::new but NOT in test modules:
- src/tmux_socket.rs
- src/kubectl.rs
- src/util.rs
- src/bead_store/cli_store.rs
- src/strand/weave.rs
- src/upgrade/mod.rs
- src/validation/shipped_work.rs
- src/validation/mod.rs
- src/strand/unravel.rs
- src/bead_store/mod.rs (except verify_backend_identity which is production)
- src/strand/reflect.rs
- src/dispatch/mod.rs
- src/cli/mod.rs
- src/canary/mod.rs (production code, not test module)
- src/cargo_test.rs (production code, not test module)
- src/telemetry/mod.rs (production code)
- src/config/mod.rs (comment only)

---

## Notes

1. **Worker-lifecycle code is in production**, not tests. The canary runner and supervisor both spawn workers as part of their normal operation.

2. **Git operations are common** in tests - 2 test modules use `Command::new("git")` for repository operations (commit_hook, mitosis/timeout_context).

3. **Zombie process tests** appear twice (registry and supervisor) - both test `is_pid_alive()` behavior with actual zombie children.

4. **No true unit tests in lib spawn workers** - worker spawning is exclusively production code for the canary runner and supervisor.

5. **Tests are reasonably isolated** - most use `tempfile::TempDir` for workspace isolation.

6. **mitosis/timeout_context has additional git helpers in production code** - the `run_git_raw()` helper at line 413 is production code, not a test, even though it's used by tests.
