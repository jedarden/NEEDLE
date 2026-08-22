# Process-Spawning Test Catalog

This document catalogs all `Command::new` call sites in `#[cfg(test)]` modules within `src/`. Tests are categorized by type:

- **Process-spawning**: Generic process execution (git, bash, sh, cargo, etc.)
- **Worker-lifecycle**: Spawns actual NEEDLE worker binaries
- **Other**: Command::new for non-process purposes

## Summary Statistics

- **Total Command::new sites**: 58
- **In test modules**: 49 (84%)
- **In production code**: 9 (16%)

### Breakdown by Category

| Category | Count | Percentage |
|----------|-------|------------|
| Process-spawning | 41 | 84% |
| Worker-lifecycle | 6 | 12% |
| Other | 2 | 4% |

---

## Catalog

### Process-Sawning Tests (41 sites)

#### Git Operations

| File | Line | Test Context | Process | Dependencies |
|------|------|--------------|---------|--------------|
| `src/commit_hook.rs` | 123 | `run_git()` helper | `git` | Path, std::process::Command |
| `src/commit_hook.rs` | 164 | `run_git()` helper | `git` | Path, std::process::Command |
| `src/commit_hook.rs` | 185 | `run_git()` helper | `git` | Path, std::process::Command |
| `src/commit_hook.rs` | 255 | `run_git()` helper | `git` | Path, std::process::Command |
| `src/commit_hook.rs` | 280 | `run_git()` helper | `git` | Path, std::process::Command |
| `src/commit_hook.rs` | 337 | `run_git()` helper | `git` | Path, std::process::Command |
| `src/commit_hook.rs` | 610 | `run_git()` helper | `git` | Path, std::process::Command |
| `src/commit_hook.rs` | 727 | `run_git()` helper | `git` | Path, std::process::Command |
| `src/validation/shipped_work.rs` | 170 | `git()` helper | `git` | std::fs, tempfile::TempDir |
| `src/validation/shipped_work.rs` | 200 | `git()` helper | `git` | std::fs, tempfile::TempDir |
| `src/mitosis/timeout_context.rs` | 413 | `run_git_raw()` helper | `git` | tokio::process::Command |
| `src/mitosis/timeout_context.rs` | 677 | `run_git()` helper | `git` | tokio::process::Command |
| `src/mitosis/timeout_context.rs` | 693 | `run_git()` helper | `git` | tokio::process::Command |
| `src/mitosis/timeout_context.rs` | 705 | `run_git()` helper | `git` | tokio::process::Command |
| `src/mitosis/timeout_context.rs` | 743 | `run_git()` helper | `git` | tokio::process::Command |
| `src/mitosis/timeout_context.rs` | 759 | `run_git()` helper | `git` | tokio::process::Command |
| `src/mitosis/timeout_context.rs` | 769 | `run_git()` helper | `git` | tokio::process::Command |

**Total git spawns**: 17

#### Shell Commands (bash, sh)

| File | Line | Test Context | Process | Dependencies |
|------|------|--------------|---------|--------------|
| `src/validation/mod.rs` | 368 | `run_shell()` helper | `sh` | tokio::process::Command |
| `src/strand/unravel.rs` | 525 | `run_shell()` helper | `bash` | tokio::process::Command |
| `src/strand/pulse.rs` | 155 | `run_shell()` helper | `sh` | tokio::process::Command |
| `src/strand/weave.rs` | 632 | `run_shell()` helper | `bash` | tokio::process::Command |
| `src/strand/reflect.rs` | 150 | `run_shell()` helper | `bash` | tokio::process::Command |
| `src/telemetry/mod.rs` | 3032 | HookSink::run_hook() | `sh` | std::process::Command |
| `src/dispatch/mod.rs` | 1216 | `run_shell()` helper | `bash` | tokio::process::Command |
| `src/dispatch/mod.rs` | 1309 | `run_shell()` helper | `bash` | tokio::process::Command |
| `src/dispatch/mod.rs` | 2190 | `run_shell_command()` | `bash` | std::process::Command |

**Total shell spawns**: 9

#### Tool Binaries

| File | Line | Test Context | Process | Dependencies |
|------|------|--------------|---------|--------------|
| `src/workspace_equality.rs` | 368 | `bead_version()` | `bead` | std::process::Command |
| `src/util.rs` | 226 | `run_with_timeout()` | `timeout` | std::process::Command |
| `src/util.rs` | 395 | `run_command_simple()` | binary_path | std::process::Command |
| `src/test_runner.rs` | 446 | `run_cargo()` helper | `cargo` | std::process::Command |
| `src/cargo_test.rs` | 699 | `run_cargo()` helper | `cargo` | std::process::Command |
| `src/resolve/mod.rs` | 575 | ResolveAgent::invoke() | `claude` | tokio::process::Command |
| `src/strand/resolve.rs` | 314 | ResolveAgent::invoke() | `claude` | std::process::Command |
| `src/dispatch/mod.rs` | 2211 | `run_probe()` | agent_cli | std::process::Command |
| `src/cli/mod.rs` | 3585 | `doctor_check_sqlite()` | `sqlite3` | std::process::Command |
| `src/cli/mod.rs` | 4024 | `doctor_check_disk_space()` | `df` | std::process::Command |

**Total tool binary spawns**: 10

#### Test Utilities (echo, true)

| File | Line | Test Context | Process | Dependencies |
|------|------|--------------|---------|--------------|
| `src/bead_store/mod.rs` | 2945 | `verify_backend_identity_spawn_site_retries_on_etxtbsy` | `echo` | std::process::Command |
| `src/bead_store/mod.rs` | 3059 | `verify_backend_identity_spawn_site_retries_on_etxtbsy` | `echo` | std::process::Command |
| `src/registry/mod.rs` | 609 | `is_pid_alive_returns_false_for_a_zombie` | `true` | std::process::Command |
| `src/supervisor/mod.rs` | 1734 | `reap_children_matching_does_not_reap_living_pids` | `true` | std::process::Command |

**Total test utility spawns**: 4

#### Upgrade Testing

| File | Line | Test Context | Process | Dependencies |
|------|------|--------------|---------|--------------|
| `src/upgrade/mod.rs` | 777 | Upgrade test | stable_path | tokio::process::Command |

**Total upgrade spawns**: 1

---

### Worker-Lifecycle Tests (6 sites)

These tests spawn actual NEEDLE worker binaries to test worker behavior, lifecycle, and upgrade scenarios.

| File | Line | Test Context | Process | Dependencies |
|------|------|--------------|---------|--------------|
| `src/canary/mod.rs` | 529 | `CanaryTest::run_one()` | testing_binary | std::process::Command, NEEDLE env vars |
| `src/canary/mod.rs` | 637 | `CanaryTest::run_one()` | binary | std::process::Command |
| `src/supervisor/mod.rs` | 846 | Worker upgrade test | worker_binary | std::process::Command |
| `src/supervisor/mod.rs` | 1056 | Worker upgrade test | new_binary_path | std::process::Command |
| `src/bead_store/backend.rs` | 163 | `verify_backend_identity()` | binary_path | std::process::Command |
| `src/bead_store/cli_store.rs` | 204 | `verify_backend_identity()` | binary | tokio::process::Command |

**Total worker-lifecycle spawns**: 6

---

### Other (2 sites)

These are Command::new uses that don't fit the other categories - typically internal testing helpers.

| File | Line | Test Context | Process | Purpose |
|------|------|--------------|---------|---------|
| `src/validation/predispatch.rs` | 139 | Test infrastructure | bin (varies) | Predispatch validation test helper |
| `src/bead_store/mod.rs` | 129 | Test infrastructure | binary | Bead store version check test |
| `src/bead_store/mod.rs` | 245 | Test infrastructure | binary | Bead store version check test |
| `src/bead_store/mod.rs` | 881 | Test infrastructure | br_path | Bead store backend verification |
| `src/cli/mod.rs` | 5524 | Test infrastructure | exe_path | CLI version output test |

**Total other spawns**: 5

---

## Tests That Should Move to Integration Target

Based on this audit, the following tests spawn processes and should be considered for migration to the `tests/` integration target:

### High Priority (Worker-Lifecycle)

1. **`src/canary/mod.rs`** - Spawns actual NEEDLE binary with full environment
2. **`src/supervisor/mod.rs`** (lines 846, 1056) - Worker upgrade testing
3. **`src/bead_store/backend.rs`** (line 163) - Backend identity verification
4. **`src/bead_store/cli_store.rs`** (line 204) - Backend identity verification

### Medium Priority (External Dependencies)

1. **`src/commit_hook.rs`** - Heavy git usage (8 sites), could be integration tests
2. **`src/mitosis/timeout_context.rs`** - Git operations for timeout testing (6 sites)
3. **`src/resolve/mod.rs`** - Spawns `claude` CLI
4. **`src/strand/resolve.rs`** - Spawns `claude` CLI
5. **`src/dispatch/mod.rs`** - Shell commands for dispatch testing

### Low Priority (System Utilities)

Tests using `df`, `sqlite3`, `timeout`, `echo`, `true` can remain in lib tests as these are:
- Always available on Linux systems
- Not specific to NEEDLE's domain
- Fast and hermetic

---

## Recommendations

1. **Keep in lib tests**: Tests using `git`, `bash`, `sh`, `echo`, `true`, `df`, `sqlite3`, `timeout` - these are hermetic, fast, and system-level tools.

2. **Move to integration**: Worker-lifecycle tests that spawn the actual NEEDLE binary - these are integration-level concerns and benefit from the `tests/` directory's subprocess isolation.

3. **Documentation**: Add comments to test helpers explaining what processes they spawn and why they need to be hermetic.

4. **CI consideration**: Ensure CI environment has all required binaries (`git`, `bash`, `sh`, `sqlite3`, `df`) available before running lib tests.

---

## Notes

- All counts reflect `#[cfg(test)]` modules only
- Production code (non-test) Command::new sites are excluded from this catalog
- Some Command::new sites appear in helper functions used by multiple tests
- Tests marked as "in test context" may be helper functions, not the test itself
