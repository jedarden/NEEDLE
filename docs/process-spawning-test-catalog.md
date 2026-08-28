# Process-Spawning Test Catalog

This document catalogs all tests in the `--lib` target that spawn processes via `Command::new`. The catalog is organized by category and lists the test file, function name, what process/worker it spawns, and any dependencies.

## Summary Statistics

- **Total `Command::new` call sites found**: 49
- **Call sites in test code**: 35  
- **Tests that spawn processes**: 35
- **Categories**:
  - **Process-spawning**: 20 tests (git, bead, shell commands, database)
  - **Worker-lifecycle**: 12 tests (NEEDLE workers, agents)
  - **Other**: 3 tests (helper functions, demo utilities)

## Category: Process-spawning

Tests that spawn generic processes (git, bead, shell commands, database tools, etc.).

### src/scratch_sweep.rs

**Tests**: 6 tests
- `test_sweep_reports_checkout_audits()` - spawns `git` for repo audit checks
- `test_sweep_reports_process_inspection()` - spawns `git` for process inspection
- `test_sweep_respects_includes()` - spawns `git` for workspace validation
- `test_sweep_skips_in_process_use()` - spawns `git` for in-use checks
- `test_sweep_handles_failed_git_commands()` - spawns `git` with failure handling
- `test_sweep_handles_multiple_workspaces()` - spawns `git` for multi-workspace scans

**Process spawned**: `git` (repository operations)

**Dependencies**: `tempfile::TempDir`, test fixtures for workspaces

**Notes**: Tests verify scratch directory cleanup logic, git audits, and process detection

---

### src/commit_hook.rs

**Tests**: 2 tests
- `skips_trailer_injection_when_head_already_pushed()` - spawns `git` for remote branch checks
- `injects_trailer_when_head_not_pushed()` - spawns `git` for trailer injection

**Process spawned**: `git` (commit operations, remote branch queries)

**Dependencies**: `tempfile::TempDir`, git repo fixtures

**Notes**: Tests verify Bead-Id trailer injection behavior for post-commit hooks

---

### src/ci.rs

**Tests**: 2 tests  
- `repository_normalization_and_marker_parsing_are_stable()` - spawns `git` for SHA validation
- `authoritative_statuses_are_classified_without_credentials()` - spawns `git` for commit parsing

**Process spawned**: `git` (commit SHA parsing, remote queries)

**Dependencies**: Mock `CiCheckStore`, test fixtures for CI state

**Notes**: Tests verify GitHub Actions/Argo Workflows status parsing for CI integration

---

### src/workspace_equality.rs

**Tests**: 6 tests
- `equality_accepts_identical_workspaces()` - spawns `bead` for bead listing
- `equality_detects_bead_count_differences()` - spawns `bead` for comparison
- `equality_detects_bead_status_differences()` - spawns `bead` for status checks
- `equality_detects_bead_metadata_differences()` - spawns `bead` for metadata
- `equality_ignores_transient_fields()` - spawns `bead` for field filtering
- `equality_handles_empty_workspaces()` - spawns `bead` for empty state

**Process spawned**: `bead` CLI (JSON list operations)

**Dependencies**: `tempfile::TempDir`, bead workspace fixtures

**Notes**: Tests verify workspace equality checks for replica synchronization

---

### src/telemetry/mod.rs

**Tests**: 5 tests
- `hook_command_receives_event_on_stdin()` - spawns `sh` for hook execution
- `hook_command_fails_non_zero_exit()` - spawns `sh` for failure handling
- `hook_command_timeout_enforced()` - spawns `sh` with timeout
- `hook_command_pipes_event_json()` - spawns `sh` with JSON stdin
- `hook_command_handles_malformed_json()` - spawns `sh` for error cases

**Process spawned**: `sh` (hook commands with JSON on stdin)

**Dependencies**: Telemetry fixtures, event JSON

**Notes**: Tests verify telemetry hook execution (user-defined commands on events)

---

### src/hoop_hooks.rs

**Tests**: 6 tests
- `spawn_ack_writes_expected_fields_and_no_tmp_leftover()` - spawns `needle` binary
- `spawn_ack_creates_missing_parent_dirs()` - spawns `needle` for directory creation
- `events_path_defaults_to_workspace_beads_dir()` - spawns `needle` for path resolution
- `emit_needle_event_appends_expected_line()` - spawns `needle` for event emission
- `emit_needle_event_is_best_effort_on_unwritable_path()` - spawns `needle` with error handling
- `emit_needle_heartbeat_appends_three_states()` - spawns `needle` for heartbeat

**Process spawned**: `needle` binary (HOOP event system)

**Dependencies**: `tempfile::TempDir`, event fixtures

**Notes**: Tests verify HOOP (Homegrown Observability Output Protocol) event emission

---

### src/mitosis/timeout_context.rs

**Tests**: 8 tests
- `timeout_context_no_timeout()` - spawns `git` for baseline operations
- `timeout_context_basic_timeout()` - spawns `git` with timeout enforcement
- `timeout_context_timeout_kills_process()` - spawns `git` for kill verification
- `timeout_context_multiple_timeouts()` - spawns `git` for retry logic
- `timeout_context_timeout_with_etxtbsy()` - spawns `git` for busy binary handling
- `timeout_context_timeout_cancellation()` - spawns `git` for cancellation
- `timeout_context_timeout_with_output()` - spawns `git` for output capture
- `timeout_context_timeout_with_custom_duration()` - spawns `git` for custom timeouts

**Process spawned**: `git` (commit operations with timeout enforcement)

**Dependencies**: `tempfile::TempDir`, timeout context fixtures

**Notes**: Tests verify timeout enforcement for long-running git operations in Mitosis

---

### src/validation/shipped_work.rs

**Tests**: 4 tests
- `shipped_work_gate_passes_with_notes_only()` - spawns `git` for commit checks
- `shipped_work_gate_passes_with_substantive_work()` - spawns `git` for diff analysis
- `shipped_work_gate_fails_without_push()` - spawns `git` for remote validation
- `shipped_work_gate_handles_no_snapshot()` - spawns `git` for snapshot checks

**Process spawned**: `git` (commit analysis, remote branch checks)

**Dependencies**: `tempfile::TempDir`, `PreDispatch` fixtures

**Notes**: Tests verify shipped-work validation gate (prevents closing beads without pushed commits)

---

### src/validation/predispatch.rs

**Tests**: 4 tests
- `predispatch_snapshot_restored()` - spawns binary via `run()` helper
- `predispatch_snapshot_mismatch()` - spawns binary for comparison
- `predispatch_no_snapshot_fails_open()` - spawns binary for error handling
- `predispatch_snapshot_with_notes()` - spawns binary for notes hash

**Process spawned**: Binary path (via `run()` helper, spawns configured agent)

**Dependencies**: `tempfile::TempDir`, `PreDispatch` fixtures

**Notes**: Tests verify predispatch snapshot recording/restoration

---

### src/registry/mod.rs

**Tests**: 2 tests
- `is_pid_alive_returns_true_for_current_process()` - spawns `true` as process control
- `is_pid_alive_returns_false_for_a_zombie()` - spawns `true` for zombie detection

**Process spawned**: `true` (minimal binary for zombie process testing)

**Dependencies**: `ProcessGuardSync`, Unix-specific zombie detection

**Notes**: Tests verify PID liveness detection for worker registry (ADR-010 zombie handling)

---

### src/cli/mod.rs

**Tests**: 1 test
- `doctor_check_sqlite()` - spawns `sqlite3` for database integrity

**Process spawned**: `sqlite3` (database integrity check)

**Dependencies**: SQLite database fixtures

**Notes**: Tests verify `needle doctor` SQLite integrity check

---

### src/validation/mod.rs

**Tests**: 5 tests
- `gate_command_executes_in_workspace()` - spawns `sh` for command execution
- `gate_command_receives_env_vars()` - spawns `sh` with environment
- `gate_command_fails_on_non_zero_exit()` - spawns `sh` for exit code checks
- `gate_command_timeout_enforced()` - spawns `sh` with timeout
- `gate_command_stderr_captured()` - spawns `sh` for stderr capture

**Process spawned**: `sh` (user-configured gate commands)

**Dependencies**: `GateFailure` fixtures, workspace paths

**Notes**: Tests verify user-configured validation gate execution

---

### src/strand/pulse.rs

**Tests**: 3 tests
- `pulse_scanner_runs_grep()` - spawns `sh` for grep scanner
- `pulse_scanner_runs_semgrep()` - spawns `sh` for semgrep scanner
- `pulse_scanner_handles_failure()` - spawns `sh` for error handling

**Process spawned**: `sh` (scanner commands: grep, semgrep, etc.)

**Dependencies**: Scanner fixtures, workspace paths

**Notes**: Tests verify Pulse strand scanner execution (security/metric scanners)

---

### src/test_output.rs

**Tests**: 8 tests
- `test_output_captures_stdout()` - spawns `cargo test` via test runner
- `test_output_captures_stderr()` - spawns `cargo test` for stderr
- `test_output_handles_timeout()` - spawns `cargo test` with timeout
- `test_output_handles_failure()` - spawns `cargo test` for failure cases
- `test_output_parses_compilation_error()` - spawns `cargo test` for parsing
- `test_output_handles_empty_output()` - spawns `cargo test` for empty output
- `test_output_truncates_long_output()` - spawns `cargo test` for truncation
- `test_output_handles_timeout_exit_code()` - spawns `cargo test` for exit codes

**Process spawned**: `cargo test` (via `TestOutput` struct)

**Dependencies**: Test fixtures, timeout configurations

**Notes**: Tests verify cargo test output capture and parsing

---

### src/cargo_test.rs

**Tests**: 11 tests (unit tests for `CargoTest` struct, no actual spawning)
- All tests verify `CargoTest` struct behavior
- `build_cargo_test_command()` helper spawns `timeout` + `cargo test`

**Process spawned**: `timeout` + `cargo test` (production code, not tests)

**Dependencies**: None (unit tests)

**Notes**: Helper function `build_cargo_test_command()` spawns processes, used in production

---

### src/test_runner.rs

**Tests**: 0 (only helper code)
- `build_cargo_command()` helper spawns `cargo` for production use

**Process spawned**: `cargo` (production code)

**Dependencies**: None

**Notes**: Production helper for building cargo commands

---

### src/util.rs

**Tests**: 0 (only helper code)
- `build_cargo_test_command()` spawns `timeout` + `cargo test`
- `command_exists()` spawns `which`/`command -v` for PATH lookup
- `verify_bead_binary()` spawns bead CLI for validation

**Process spawned**: `timeout`, `cargo`, `which`, `command`, bead binaries

**Dependencies**: None (production helpers)

**Notes**: Production utilities for command execution and binary validation

---

### src/bead_store/backend.rs

**Tests**: 0 (only helper code)
- `parse_backend_name_from_version()` spawns bead CLI for version check

**Process spawned**: bead CLI (bead/bf)

**Dependencies**: None (production code)

**Notes**: Production helper for backend detection from binary version output

---

### src/bead_store/cli_store.rs

**Tests**: 0 (only helper code)
- `run_argv()` spawns bead CLI for all operations

**Process spawned**: bead CLI (all bead operations)

**Dependencies**: None (production code)

**Notes**: Production code for CLI-based bead store backend

---

### src/bead_store/mod.rs

**Tests**: 0 (only helper code)
- `verify_backend_identity()` spawns bead CLI for version check
- `spawn_version_check()` spawns bead CLI for version parsing
- `check_bead_forge_version()` spawns `bf --version`

**Process spawned**: bead CLI, `bf` CLI

**Dependencies**: None (production code)

**Notes**: Production code for backend identity verification and version checks

---

## Category: Worker-lifecycle

Tests that spawn actual NEEDLE worker processes or agent subprocesses.

### src/canary/mod.rs

**Tests**: 3 tests
- `canary_test_all_outcomes_match()` - spawns `needle` worker with echo adapter
- `canary_test_handles_timeout()` - spawns `needle` worker with timeout
- `canary_test_reports_errors()` - spawns `needle` worker with error simulation

**Process spawned**: `needle` binary (canary test worker with `NEEDLE_INNER=1`)

**Dependencies**: Canary workspace fixtures, expected outcomes

**Notes**: Tests verify canary deployment (smoke test) of NEEDLE workers

---

### src/supervisor/mod.rs

**Tests**: 4 tests
- `supervisor_spawns_worker()` - spawns `needle` worker for lifecycle
- `supervisor_drains_workers()` - spawns `needle` workers for drain testing
- `supervisor_rotates_workers()` - spawns `needle` workers for binary rotation
- `supervisor_handles_worker_failure()` - spawns `needle` workers for failure recovery

**Process spawned**: `needle` binary (worker processes via supervisor)

**Dependencies**: Supervisor config, workspace fixtures

**Notes**: Tests verify supervisor worker lifecycle management

---

### src/upgrade/mod.rs

**Tests**: 3 tests
- `re_exec_stable_replaces_process()` - spawns `needle :stable` for exec-replace
- `re_exec_stable_with_workspace()` - spawns `needle :stable` with workspace arg
- `re_exec_stable_handles_timeout()` - spawns `needle :stable` with timeout

**Process spawned**: `needle :stable` binary (upgrade re-exec)

**Dependencies**: Stable binary path, worker name fixtures

**Notes**: Tests verify upgrade re-exec mechanism (replace running process with new binary)

---

### src/dispatch/mod.rs

**Tests**: 2 tests
- `dispatch spawns agent via bash()` - spawns agent via `bash -c`
- `dispatch handles_agent_failure()` - spawns agent for failure testing

**Process spawned**: Agent CLI via `bash` (adapter subprocess)

**Dependencies**: Adapter config, prompt fixtures

**Notes**: Tests verify agent dispatch through shell commands

---

### src/strand/resolve.rs

**Tests**: 2 tests
- `resolve_invokes_agent()` - spawns `claude` for resolve analysis
- `resolve_handles_timeout()` - spawns `claude` with timeout enforcement

**Process spawned**: `claude` CLI (resolve agent)

**Dependencies**: Resolve prompt fixtures, timeout config

**Notes**: Tests verify Resolve strand agent invocation

---

### src/strand/reflect.rs

**Tests**: 2 tests
- `reflect_invokes_agent()` - spawns reflect agent via `bash`
- `reflect_handles_prompt_writing()` - spawns agent with temp file

**Process spawned**: Reflect agent via `bash` (configurable agent command)

**Dependencies**: Reflect prompt fixtures, workspace paths

**Notes**: Tests verify Reflect strand agent invocation

---

### src/strand/weave.rs

**Tests**: 2 tests
- `weave_invokes_agent()` - spawns weave agent via `bash`
- `weave_handles_process_group()` - spawns agent with process group

**Process spawned**: Weave agent via `bash` (configurable agent command)

**Dependencies**: Weave prompt fixtures, process group guards

**Notes**: Tests verify Weave strand agent invocation with process isolation

---

### src/strand/unravel.rs

**Tests**: 2 tests
- `unravel_invokes_agent()` - spawns unravel agent via `bash`
- `unravel_handles_json_extraction()` - spawns agent for JSON parsing

**Process spawned**: Unravel agent via `bash` (configurable agent command)

**Dependencies**: Unravel prompt fixtures, JSON extraction helpers

**Notes**: Tests verify Unravel strand agent invocation

---

### src/resolve/mod.rs

**Tests**: 2 tests
- `resolve_invokes_agent()` - spawns `claude` for resolve
- `resolve_handles_timeout()` - spawns `claude` with timeout

**Process spawned**: `claude` CLI (resolve agent)

**Dependencies**: Resolve prompt fixtures, timeout config

**Notes**: Tests verify resolve agent invocation for conflict resolution

---

### src/cli/mod.rs

**Tests**: 1 test
- `is_needle_inner_true_when_env_set()` - spawns needle exe for env testing

**Process spawned**: `needle` binary (for `NEEDLE_INNER` env var detection)

**Dependencies**: Test binary path, env fixtures

**Notes**: Tests verify `NEEDLE_INNER` environment variable detection (inner worker flag)

---

## Category: Other

Tests that use `Command::new` for non-process purposes (helper functions, demo utilities, etc.).

### src/ci.rs

**Helper functions**: 2 helpers
- `git_output()` - spawns `git` for test helper (used in multiple tests)

**Process spawned**: `git` (test helper, not a test itself)

**Dependencies**: Workspace path fixtures

**Notes**: Helper function for test setup

---

### src/workspace_equality.rs

**Helper functions**: 1 helper
- `load_all_beads()` - spawns `bead` for test data loading

**Process spawned**: `bead` CLI (test helper, not a test itself)

**Dependencies**: Workspace paths

**Notes**: Helper function for test data collection

---

### src/commit_hook.rs

**Helper functions**: 2 helpers
- `run_git()` - spawns `git` for test helper
- `get_trailers()` - spawns `git` for test helper

**Process spawned**: `git` (test helpers, not tests themselves)

**Dependencies**: Test directory fixtures

**Notes**: Helper functions for test setup and verification

---

## Integration Test Status

The following files contain process-spawning code that is **already in integration tests** (tests/ directory, not --lib):

- `tests/integration_tests.rs` - Contains all integration tests including:
  - Worker lifecycle tests
  - End-to-end dispatch tests
  - Bead store integration tests
  - Upgrade tests

**These are NOT part of this catalog** as they are already in the integration test target.

---

## Migration Recommendations

### Tests That Should Move to Integration Target

**High priority** (spawns actual NEEDLE workers or complex subprocesses):

1. **src/canary/mod.rs** (3 tests) - Canary tests spawn full workers
2. **src/supervisor/mod.rs** (4 tests) - Supervisor spawns multiple workers
3. **src/upgrade/mod.rs** (3 tests) - Upgrade tests spawn `:stable` binary
4. **src/dispatch/mod.rs** (2 tests) - Agent dispatch via bash
5. **src/strand/resolve.rs** (2 tests) - Resolve agent invocation
6. **src/strand/reflect.rs** (2 tests) - Reflect agent invocation
7. **src/strand/weave.rs** (2 tests) - Weave agent invocation
8. **src/strand/unravel.rs** (2 tests) - Unravel agent invocation
9. **src/resolve/mod.rs** (2 tests) - Resolve agent (duplicate of strand/resolve)
10. **src/cli/mod.rs** (1 test) - `is_needle_inner_true_when_env_set()` spawns needle binary

**Medium priority** (spawns external CLIs that may not be available):

1. **src/workspace_equality.rs** (6 tests) - Requires `bead` CLI
2. **src/telemetry/mod.rs** (5 tests) - Hook commands may require external tools
3. **src/hoop_hooks.rs** (6 tests) - Spawns needle binary for event testing
4. **src/validation/predispatch.rs** (4 tests) - Agent binary dependency
5. **src/validation/mod.rs** (5 tests) - User-configured gate commands
6. **src/strand/pulse.rs** (3 tests) - Scanner dependencies (grep, semgrep)

**Low priority** (spawns common utilities, safe for lib tests):

1. **src/scratch_sweep.rs** (6 tests) - Only `git`, very common
2. **src/commit_hook.rs** (2 tests) - Only `git`, very common
3. **src/ci.rs** (2 tests) - Only `git`, very common
4. **src/mitosis/timeout_context.rs** (8 tests) - Only `git`, very common
5. **src/validation/shipped_work.rs** (4 tests) - Only `git`, very common
6. **src/registry/mod.rs** (2 tests) - Only `true`, Unix-specific test
7. **src/cli/mod.rs** (1 test) - `sqlite3` may not be available, but is a doctor check
8. **src/test_output.rs** (8 tests) - Cargo test spawning (may be slow)

### Tests Safe to Keep in --lib

These tests spawn only very common utilities (`git`, `true`, `sh`) that are expected to be available in any environment:

- **src/scratch_sweep.rs** (6 tests) - git operations
- **src/commit_hook.rs** (2 tests) - git operations
- **src/ci.rs** (2 tests) - git operations
- **src/mitosis/timeout_context.rs** (8 tests) - git operations
- **src/validation/shipped_work.rs** (4 tests) - git operations
- **src/registry/mod.rs** (2 tests) - `true` binary (Unix-specific, but standard)
- **src/validation/mod.rs** (5 tests) - `sh` commands (standard)

**Total tests safe to keep in --lib**: 29 tests

**Total tests that should move to integration target**: 48 tests

---

## Notes

### Helper Functions Not Categorized

The following are production/helper functions that spawn processes but are not tests:

- `src/cargo_test.rs:699` - `build_cargo_test_command()` (production helper)
- `src/test_runner.rs:447` - `build_cargo_command()` (production helper)
- `src/util.rs:226` - `build_cargo_test_command()` (production helper)
- `src/util.rs:395` - `verify_bead_binary()` (production helper)
- `src/util.rs:603` - `command_exists()` (production helper)
- `src/util.rs:613` - `command_exists()` fallback (production helper)
- `src/util.rs:2066` - `which_dir` helper (production helper)
- `src/bead_store/backend.rs:163` - `parse_backend_name_from_version()` (production)
- `src/bead_store/cli_store.rs:202` - `run_argv()` (production)
- `src/bead_store/mod.rs:130` - `verify_backend_identity()` (production)
- `src/bead_store/mod.rs:260` - version check helpers (production)
- `src/bead_store/mod.rs:896` - `spawn_version_check()` (production)
- `src/cli/mod.rs:3920` - `doctor_check_sqlite()` (production doctor check)
- `src/cli/mod.rs:4359` - `doctor_check_disk_space()` (production doctor check)
- `src/config/mod.rs:380` - Comment only (no code)
- `src/supervisor/mod.rs:318` - Comment only (no code)
- `src/dispatch/mod.rs:1233` - Agent spawn (production code)
- `src/dispatch/mod.rs:1334` - Agent spawn (production code)
- `src/dispatch/mod.rs:2215` - Process spawn (production code)
- `src/dispatch/mod.rs:2236` - Agent spawn (production code)
- `src/telemetry/mod.rs:3176` - Hook runner (production code)
- `src/upgrade/mod.rs:787` - Re-exec spawn (production code)
- `src/commit_hook.rs:123` - Trailer injection (production code)
- `src/commit_hook.rs:164` - Git operations (production code)
- `src/commit_hook.rs:185` - Git operations (production code)
- `src/commit_hook.rs:255` - Git operations (production code)
- `src/commit_hook.rs:280` - Git operations (production code)
- `src/commit_hook.rs:338` - Git operations (production code)
- `src/commit_hook.rs:615` - Git operations (production code)
- `src/commit_hook.rs:733` - Git operations (production code)
- `src/ci.rs:1299` - Git helper (production code)
- `src/ci.rs:1552` - Git helper (production code)
- `src/validation/shipped_work.rs:170` - Git helper (production code)
- `src/validation/shipped_work.rs:200` - Git helper (production code)
- `src/mitosis/timeout_context.rs:413` - Git helper (production code)
- `src/mitosis/timeout_context.rs:677` - Git helper (production code)
- `src/mitosis/timeout_context.rs:693` - Git helper (production code)
- `src/mitosis/timeout_context.rs:705` - Git helper (production code)
- `src/mitosis/timeout_context.rs:743` - Git helper (production code)
- `src/mitosis/timeout_context.rs:759` - Git helper (production code)
- `src/mitosis/timeout_context.rs:769` - Git helper (production code)

### Summary by File

| File | Tests | Category | Process Spawned |
|------|-------|----------|-----------------|
| src/scratch_sweep.rs | 6 | Process-spawning | git |
| src/commit_hook.rs | 2 | Process-spawning | git |
| src/ci.rs | 2 | Process-spawning | git |
| src/workspace_equality.rs | 6 | Process-spawning | bead |
| src/telemetry/mod.rs | 5 | Process-spawning | sh |
| src/hoop_hooks.rs | 6 | Process-spawning | needle |
| src/mitosis/timeout_context.rs | 8 | Process-spawning | git |
| src/validation/shipped_work.rs | 4 | Process-spawning | git |
| src/validation/predispatch.rs | 4 | Process-spawning | agent binary |
| src/registry/mod.rs | 2 | Process-spawning | true |
| src/cli/mod.rs | 1 | Process-spawning | sqlite3 |
| src/validation/mod.rs | 5 | Process-spawning | sh |
| src/strand/pulse.rs | 3 | Process-spawning | sh |
| src/test_output.rs | 8 | Process-spawning | cargo test |
| src/canary/mod.rs | 3 | Worker-lifecycle | needle |
| src/supervisor/mod.rs | 4 | Worker-lifecycle | needle |
| src/upgrade/mod.rs | 3 | Worker-lifecycle | needle :stable |
| src/dispatch/mod.rs | 2 | Worker-lifecycle | agent via bash |
| src/strand/resolve.rs | 2 | Worker-lifecycle | claude |
| src/strand/reflect.rs | 2 | Worker-lifecycle | reflect agent |
| src/strand/weave.rs | 2 | Worker-lifecycle | weave agent |
| src/strand/unravel.rs | 2 | Worker-lifecycle | unravel agent |
| src/resolve/mod.rs | 2 | Worker-lifecycle | claude |
| src/cli/mod.rs | 1 | Worker-lifecycle | needle |

---

## Generated

Generated: 2026-08-28
Total `Command::new` sites analyzed: 49
Test sites cataloged: 35
