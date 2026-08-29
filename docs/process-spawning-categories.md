# Process Spawning Categories

**Generated:** 2026-08-29T18:25:00Z
**Source:** `docs/process-spawning-test-catalog-raw.md`

## Overview

All 23 Command::new sites in test modules fall into a single category. This catalog analyzed each invocation against three possible categories:

- **Process-spawning**: Generic process execution (git, cargo, sh, other binaries)
- **Worker-lifecycle**: Spawns actual NEEDLE worker processes via `Command::new(CARGO_BIN_EXE_needle)`
- **Other**: Command::new for non-process purposes (e.g., mocking, testing command construction itself)

## Category: Process-spawning

**Count:** 23 sites (100% of all Command::new invocations in test modules)

**Definition:** Tests that spawn generic processes to exercise system behavior, version control operations, or process lifecycle features.

### By Command Type

| Command | Count | Purpose |
|---------|-------|---------|
| `git` | 15 | Version control operations in test scenarios |
| `echo` | 2 | Mock bead backend responses for retry timing tests |
| `true` | 2 | Process spawn/shutdown lifecycle tests |
| `which` | 1 | Path resolution testing |
| `exe_path` | 1 | Version probe integration test (dynamic binary) |
| `true` (tokio) | 1 | Async process spawn test |
| `echo` (std) | 2 | Synchronous retry timing tests |

### By Test Function

#### src/commit_hook.rs (3 sites)
- `test_rewrite_detection_across_rewrites` — Line 510
- `test_rewrite_detection_edge_cases` — Line 787
- `test_rewrite_detection_integration` — Line 905

#### src/mitosis/timeout_context.rs (6 sites)
- `test_bead` — Lines 677, 693, 705, 743, 759, 769

#### src/validation/mod.rs (5 sites)
- `test_bead` — Lines 1614, 1620, 1626, 1634, 1642

#### src/bead_store/mod.rs (2 sites)
- `etxtbsy_retry_sync_exponential_single_retry_timing` — Line 2848
- `etxtbsy_retry_async_exponential_many_retries_timing` — Line 2962

#### src/scratch_sweep.rs (2 sites)
- Helper function `git` within test module — Lines 835, 850

#### Individual sites (1 each)
- `src/bead_store/cli_store.rs:1146` — `test_store`
- `src/ci.rs:1555` — `commit_correlation_requires_one_unambiguous_trailer`
- `src/cli/mod.rs:6107` — `test_version_probe_integration`
- `src/registry/mod.rs:771` — `is_pid_alive_returns_false_for_a_zombie`
- `src/supervisor/mod.rs:1876` — `test_supervisor_spawn_and_shutdown`
- `src/util.rs:2271` — `test_bead_cli_backend_name_mapping`
- `src/validation/shipped_work.rs:289` — Helper function `git` within test module

## Category: Worker-lifecycle

**Count:** 0 sites

**Definition:** Tests that spawn actual NEEDLE worker processes using `Command::new(CARGO_BIN_EXE_needle)`.

**Finding:** No test in the codebase currently spawns a real NEEDLE worker subprocess. All worker testing is done in-process, not via binary subprocess execution.

This means:
- The `$HOME` isolation clause in the Test Isolation Policy targets a threat that **does not currently exist** in the codebase
- Existing worker tests (if any) must be using in-process instantiation, not subprocess spawning
- If worker subprocess tests are added in the future, they will immediately fall under the isolation requirements

## Category: Other

**Count:** 0 sites

**Definition:** Command::new invocations for non-execution purposes (e.g., testing command builder logic, mocking command construction, validating argument parsing without actual execution).

**Finding:** All Command::new sites in test modules are intended to execute real processes, not to test command construction itself.

## Key Findings

1. **100% generic process execution**: Every Command::new in test code spawns a system binary (git, echo, true, which) or a dynamic binary path for functional testing.

2. **No worker subprocess tests**: Despite NEEDLE being a worker binary, no test spawns it as a subprocess. Worker logic is tested in-process or through integration tests that don't use `Command::new(CARGO_BIN_EXE_needle)`.

3. **Isolation policy targets non-existent threat**: The `$HOME` isolation requirement in the Test Isolation Policy is written for a pattern (worker subprocess spawning) that doesn't appear in the current test suite. The policy may be:
   - Forward-looking (anticipating future worker subprocess tests)
   - Defensive (based on a 2026-07-20 contamination incident that involved a different mechanism)
   - Over-cautious (protecting against a pattern that could be introduced)

4. **git dominates**: 15 of 23 sites (65%) spawn `git`, reflecting the heavy use of version control operations across commit hooks, validation, and timeout context tests.

## Recommendations

1. **Audit the isolation policy**: Since no worker subprocess tests exist, verify whether the `$HOME` isolation requirement is:
   - Still necessary (forward-looking protection)
   - Can be relaxed to apply only when `CARGO_BIN_EXE_needle` is detected
   - Should be kept as-is (defense-in-depth)

2. **Document the in-process testing approach**: If worker logic is tested without subprocess spawning, document that approach so future contributors understand why no worker subprocess tests exist.

3. **Consider worker subprocess test coverage**: If worker subprocess behavior is important, add explicit tests using `Command::new(CARGO_BIN_EXE_needle)` — these would then immediately require `$HOME` isolation per the policy.
