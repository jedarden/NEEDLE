# Process Spawning Test Catalog

**Generated:** 2026-08-29T18:30:00Z
**Purpose:** Comprehensive catalog of all Command::new invocations in test modules for test isolation analysis and organization planning

## Summary Statistics

| Metric | Count |
|--------|-------|
| **Total Command::new sites** | 23 |
| **Files with process-spawning tests** | 10 |
| **Files with worker-lifecycle tests** | 0 |
| **Test functions spawning processes** | 18 |
| **Helper functions in test modules** | 3 |

### Category Distribution

| Category | Count | Percentage |
|----------|-------|------------|
| Process-spawning | 23 | 100% |
| Worker-lifecycle | 0 | 0% |
| Other | 0 | 0% |

## Category: Process-spawning (23 sites)

**Definition:** Tests that spawn generic processes to exercise system behavior, version control operations, or process lifecycle features.

### Command Type Breakdown

| Command | Count | Purpose |
|---------|-------|---------|
| `git` | 15 | Version control operations in test scenarios |
| `echo` | 2 | Mock bead backend responses for retry timing tests |
| `true` | 2 | Process spawn/shutdown lifecycle tests |
| `which` | 1 | Path resolution testing |
| `exe_path` (dynamic) | 1 | Version probe integration test |
| `true` (tokio) | 1 | Async process spawn test |

---

## Detailed Site Listings

### src/commit_hook.rs (3 sites)

#### test_rewrite_detection_across_rewrites
- **File:** `src/commit_hook.rs:510`
- **Function:** `test_rewrite_detection_across_rewrites`
- **Command:** `Command::new("git")`
- **Purpose:** Tests rewrite detection across multiple rewrite operations
- **Dependencies:** git CLI
- **Location:** Unit test module within source file

#### test_rewrite_detection_edge_cases
- **File:** `src/commit_hook.rs:787`
- **Function:** `test_rewrite_detection_edge_cases`
- **Command:** `Command::new("git")`
- **Purpose:** Tests edge cases in rewrite detection logic
- **Dependencies:** git CLI
- **Location:** Unit test module within source file

#### test_rewrite_detection_integration
- **File:** `src/commit_hook.rs:905`
- **Function:** `test_rewrite_detection_integration`
- **Command:** `Command::new("git")`
- **Purpose:** Integration test for rewrite detection
- **Dependencies:** git CLI
- **Location:** Unit test module within source file
- **Recommendation:** Should move to `tests/commit_hook_tests.rs` (integration target)

---

### src/mitosis/timeout_context.rs (6 sites)

#### test_bead (multiple spawns)
- **File:** `src/mitosis/timeout_context.rs:677, 693, 705, 743, 759, 769`
- **Function:** `test_bead`
- **Command:** `tokio::process::Command::new("git")` (6 invocations)
- **Purpose:** Tests timeout context behavior with git operations
- **Dependencies:** git CLI, tokio async runtime
- **Location:** Unit test module within source file

---

### src/validation/mod.rs (5 sites)

#### test_bead (multiple spawns)
- **File:** `src/validation/mod.rs:1614, 1620, 1626, 1634, 1642`
- **Function:** `test_bead`
- **Command:** `std::process::Command::new("git")` (5 invocations)
- **Purpose:** Tests validation logic with git repository operations
- **Dependencies:** git CLI
- **Location:** Unit test module within source file
- **Recommendation:** Should move to `tests/validation_tests.rs` (integration target)

---

### src/bead_store/mod.rs (2 sites)

#### etxtbsy_retry_sync_exponential_single_retry_timing
- **File:** `src/bead_store/mod.rs:2848`
- **Function:** `etxtbsy_retry_sync_exponential_single_retry_timing`
- **Command:** `std::process::Command::new("echo")`
- **Purpose:** Tests synchronous retry timing with mocked backend response
- **Dependencies:** echo command (system binary)
- **Location:** Unit test module within source file

#### etxtbsy_retry_async_exponential_many_retries_timing
- **File:** `src/bead_store/mod.rs:2962`
- **Function:** `etxtbsy_retry_async_exponential_many_retries_timing`
- **Command:** `std::process::Command::new("echo")`
- **Purpose:** Tests async retry timing with multiple backend response mocks
- **Dependencies:** echo command (system binary), async runtime
- **Location:** Unit test module within source file

---

### src/scratch_sweep.rs (2 sites)

#### Helper function `git`
- **File:** `src/scratch_sweep.rs:835, 850`
- **Function:** Helper `git()` called from test functions
- **Command:** `Command::new("git")` (2 invocations)
- **Purpose:** Git operations helper for scratch sweep tests
- **Dependencies:** git CLI
- **Location:** Unit test module within source file

---

### Individual Sites (1 each)

#### test_store
- **File:** `src/bead_store/cli_store.rs:1146`
- **Function:** `test_store`
- **Command:** `tokio::process::Command::new("true").spawn()`
- **Purpose:** Tests CLI store with process spawn
- **Dependencies:** true command (system binary), tokio
- **Location:** Unit test module within source file

#### commit_correlation_requires_one_unambiguous_trailer
- **File:** `src/ci.rs:1555`
- **Function:** `commit_correlation_requires_one_unambiguous_trailer`
- **Command:** `std::process::Command::new("git")`
- **Purpose:** Tests commit trailer correlation logic
- **Dependencies:** git CLI
- **Location:** Unit test module within source file

#### test_version_probe_integration
- **File:** `src/cli/mod.rs:6107`
- **Function:** `test_version_probe_integration`
- **Command:** `tokio::process::Command::new(&exe_path)`
- **Purpose:** Tests version probe with dynamic binary execution
- **Dependencies:** Dynamic binary path (exe_path), tokio
- **Location:** Unit test module within source file

#### is_pid_alive_returns_false_for_a_zombie
- **File:** `src/registry/mod.rs:771`
- **Function:** `is_pid_alive_returns_false_for_a_zombie`
- **Command:** `std::process::Command::new("true")`
- **Purpose:** Tests PID aliveness detection for zombie processes
- **Dependencies:** true command (system binary)
- **Location:** Unit test module within source file

#### test_supervisor_spawn_and_shutdown
- **File:** `src/supervisor/mod.rs:1876`
- **Function:** `test_supervisor_spawn_and_shutdown`
- **Command:** `std::process::Command::new("true")`
- **Purpose:** Tests supervisor spawn and shutdown lifecycle
- **Dependencies:** true command (system binary)
- **Location:** Unit test module within source file

#### test_bead_cli_backend_name_mapping
- **File:** `src/util.rs:2271`
- **Function:** `test_bead_cli_backend_name_mapping`
- **Command:** `std::process::Command::new("which")`
- **Purpose:** Tests CLI backend name mapping via path resolution
- **Dependencies:** which command (system binary)
- **Location:** Unit test module within source file

#### Helper function `git` (shipped_work validation)
- **File:** `src/validation/shipped_work.rs:289`
- **Function:** Helper `git()` within test module
- **Command:** `std::process::Command::new("git")`
- **Purpose:** Git operations helper for shipped work validation tests
- **Dependencies:** git CLI
- **Location:** Unit test module within source file

---

## Category: Worker-lifecycle (0 sites)

**Definition:** Tests that spawn actual NEEDLE worker processes using `Command::new(CARGO_BIN_EXE_needle)`.

### Finding

**Count:** 0 sites

**Analysis:** No test in the codebase currently spawns a real NEEDLE worker subprocess. All worker testing is done in-process, not via binary subprocess execution.

**Implications:**
- The `$HOME` isolation clause in the Test Isolation Policy targets a threat that **does not currently exist** in the codebase
- Existing worker tests (if any) must be using in-process instantiation, not subprocess spawning
- If worker subprocess tests are added in the future, they will immediately fall under the isolation requirements

**Recommendation:** Consider adding explicit worker subprocess tests using `Command::new(CARGO_BIN_EXE_needle)` to test:
- Worker process startup/shutdown lifecycle
- Inter-process communication patterns
- Signal handling and graceful shutdown
- Worker multiprocess isolation

---

## Category: Other (0 sites)

**Definition:** Command::new invocations for non-execution purposes (e.g., testing command builder logic, mocking command construction, validating argument parsing without actual execution).

### Finding

**Count:** 0 sites

**Analysis:** All Command::new sites in test modules are intended to execute real processes, not to test command construction itself.

---

## File Distribution Summary

| File | Command::new Count | Primary Commands |
|------|-------------------|------------------|
| `src/mitosis/timeout_context.rs` | 6 | git |
| `src/validation/mod.rs` | 5 | git |
| `src/commit_hook.rs` | 3 | git |
| `src/bead_store/mod.rs` | 2 | echo |
| `src/scratch_sweep.rs` | 2 | git (helper) |
| `src/validation/shipped_work.rs` | 1 | git (helper) |
| `src/bead_store/cli_store.rs` | 1 | true (tokio) |
| `src/ci.rs` | 1 | git |
| `src/cli/mod.rs` | 1 | exe_path (dynamic) |
| `src/registry/mod.rs` | 1 | true |
| `src/supervisor/mod.rs` | 1 | true |
| `src/util.rs` | 1 | which |

---

## Test Organization Recommendations

### Should Move to `tests/` Integration Target

The following tests spawn real processes and exercise multiple modules, making them better suited as integration tests:

1. **`src/commit_hook.rs:test_rewrite_detection_integration`**
   - **Reason:** Full git lifecycle test with multiple rewrite operations
   - **Target:** `tests/commit_hook_tests.rs`

2. **`src/validation/mod.rs:test_bead`** (all 5 git invocations)
   - **Reason:** Integration test spanning validation logic and git operations
   - **Target:** `tests/validation_tests.rs`

3. **`src/ci.rs:commit_correlation_requires_one_unambiguous_trailer`**
   - **Reason:** CI integration test with git trailer parsing
   - **Target:** `tests/ci_integration_tests.rs`

### Should Remain as Unit Tests

Tests that use simple commands (`echo`, `true`, `which`) for timing, lifecycle, or path resolution are appropriate as unit tests:

- Retry timing tests (`echo` mocks)
- Process spawn/shutdown tests (`true`)
- Path resolution tests (`which`)
- Single-command git operations within the same module

---

## Key Findings

### 1. 100% Generic Process Execution

Every Command::new in test code spawns a system binary (git, echo, true, which) or a dynamic binary path for functional testing. No tests spawn the NEEDLE worker binary itself.

### 2. Git Dominance

15 of 23 sites (65%) spawn `git`, reflecting the heavy use of version control operations across:
- Commit hook tests (3)
- Timeout context tests (6)
- Validation tests (5)
- CI tests (1)
- Scratch sweep tests (2)
- Shipped work validation tests (1)

### 3. Isolation Policy Targets Non-Existent Threat

The `$HOME` isolation requirement in the Test Isolation Policy (`docs/testing-isolation-patterns.md`) is written for a pattern (worker subprocess spawning via `CARGO_BIN_EXE_needle`) that doesn't appear in the current test suite.

**Policy context:** Based on 2026-07-20 contamination incident where an orphaned binary roamed into live bead store. That incident involved in-process `Worker` instantiation, not subprocess spawning.

**Current policy status:**
- Forward-looking (anticipating future worker subprocess tests)
- Defensive (protecting against the 2026-07-20 pattern recurrence)
- Potentially over-cautious (no current worker subprocess tests exist)

### 4. No Mock Command Construction Tests

All Command::new invocations are intended for actual process execution. There are no tests that validate command construction logic itself (e.g., testing argument escaping, environment variable passing, or working directory handling without execution).

---

## Action Items

### High Priority

1. **Audit isolation policy scope**
   - Verify if `$HOME` isolation should apply only when `CARGO_BIN_EXE_needle` is detected
   - Consider making the isolation requirement conditional on the spawned binary type
   - Document the rationale for the current broad requirement

2. **Move integration tests to `tests/`**
   - Relocate `test_rewrite_detection_integration` to `tests/commit_hook_tests.rs`
   - Relocate `test_bead` (validation module) to `tests/validation_tests.rs`
   - Update CI configuration if needed

### Medium Priority

3. **Add worker subprocess test coverage**
   - Create explicit tests using `Command::new(CARGO_BIN_EXE_needle)`
   - Test worker process startup/shutdown lifecycle
   - Verify signal handling and graceful shutdown
   - These would immediately require `$HOME` isolation

4. **Document in-process testing approach**
   - Add documentation explaining why worker logic is tested in-process
   - Clarify when subprocess spawning vs. in-process instantiation should be used

### Low Priority

5. **Consider adding command construction tests**
   - Add tests for command builder logic without actual execution
   - Test argument escaping, environment passing, working directory handling
   - These would fall under the "Other" category

---

## Verification Methodology

This catalog was generated by:

1. **Identifying test files:** Finding all files with `#[cfg(test)]` modules
2. **Locating invocations:** Searching for `Command::new`, `tokio::process::Command::new`, and `std::process::Command::new`
3. **Filtering by context:** Including only calls AFTER the `#[cfg(test)]` marker
4. **Extracting metadata:** Capturing file path, line number, function name, and command target
5. **Categorization:** Classifying each site into process-spawning, worker-lifecycle, or other
6. **Manual verification:** Validating edge cases and helper functions

**Line numbers are accurate as of 2026-08-29.**

---

## Related Documentation

- `docs/process-spawning-categories.md` — Category analysis and methodology
- `docs/process-spawning-test-catalog-raw.md` — Raw unstructured listing
- `docs/testing-isolation-patterns.md` — Test isolation policy and `$HOME` requirements
- `CLAUDE.md` — Project testing conventions and CI/CD workflow

---

## Change Log

- **2026-08-29T18:30:00Z** — Initial comprehensive catalog created from categorized data
- **2026-08-29T18:25:00Z** — Categories established in `process-spawning-categories.md`
- **2026-08-29T18:15:00Z** — Raw catalog generated in `process-spawning-test-catalog-raw.md`
