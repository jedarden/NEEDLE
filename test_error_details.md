# Test Error Details

**Generated:** 2026-08-21

**Test suite:** `integration_tests`
**Purpose:** Merge the current failing-test list, captured error messages, and
stack traces into one test-keyed report.

## Source files

| Source | Contents used |
| --- | --- |
| [`verification-failing-tests-list-current-2026-08-21.md`](verification-failing-tests-list-current-2026-08-21.md) | Current list of 9 failing tests, their status, and the reported test-level causes. |
| [`test_error_messages.txt`](test_error_messages.txt) | CI error output from `cargo fmt -- --check`; this is a run-level error and is not attributed to one test. |
| [`test_stack_traces.txt`](test_stack_traces.txt) | Current raw capture containing 6 test-specific panic/backtrace blocks. |
| [`test_stack_traces_organized.txt`](test_stack_traces_organized.txt) | Organized cross-check of the same 6 current trace blocks. |

## Reconciliation notes

- The current failing-test list reports 9 tests, but the current stack-trace
  capture contains 6 traces. The three listed tests without a matching trace
  are retained below and marked as having no captured details.
- The list's first subsection spells one name
  `cross_workspace_skips_own_worker_beads`; its detailed nine-test section and
  the trace capture use the canonical name
  `cross_workspace_mend_skips_own_worker_beads`. This report uses the canonical
  name and records the spelling discrepancy rather than treating it as a tenth
  test.
- `test_error_messages.txt` describes a formatting-gate failure that occurred
  before tests could run. It is included in the report-level section and is not
  incorrectly duplicated as the cause of every test failure.

## Consolidated index

| # | Test name | List status | Test-specific error message | Stack trace |
| ---: | --- | --- | --- | --- |
| 1 | `adapter_validation_happens_before_main_worker_loop` | New failure | Not present in supplied error/trace sources | Not captured |
| 2 | `adapter_validation_rejects_special_characters` | Still failing | `error message should not execute injected payloads for adapter: '../../../etc/passwd'` | [`test_stack_traces.txt:308`](test_stack_traces.txt#L308) |
| 3 | `ci_verification_test_failure` | New failure | `CI VERIFICATION TEST FAILURE - This test is meant to fail to verify needle-ci retryStrategy fix surfaces test failures correctly` | [`test_stack_traces.txt:378`](test_stack_traces.txt#L378) |
| 4 | `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` | Still failing | `br create failed:` | [`test_stack_traces.txt:440`](test_stack_traces.txt#L440) |
| 5 | `cross_workspace_mend_skips_beads_with_live_assignees` | Still failing | `br create failed` | [`test_stack_traces.txt:502`](test_stack_traces.txt#L502) |
| 6 | `cross_workspace_mend_skips_own_worker_beads` | Still failing | `br create failed` | [`test_stack_traces.txt:564`](test_stack_traces.txt#L564) |
| 7 | `dead_worker_cleanup_integration` | Still failing | `needle worker failed with exit status: ExitStatus(unix_wait_status(512))` | [`test_stack_traces.txt:626`](test_stack_traces.txt#L626) |
| 8 | `idle_worker_flagging_detects_stuck_workers` | New failure | Not present in supplied error/trace sources | Not captured |
| 9 | `mend_removes_stale_dependency_links` | New failure | Not present in supplied error/trace sources | Not captured |

## Test-by-test details

### 1. `adapter_validation_happens_before_main_worker_loop`

- **Status:** New failure in the 2026-08-21 list.
- **Error message:** No test-specific message for this name appears in
  `test_error_messages.txt` or the current stack-trace capture.
- **Stack trace:** Not captured in the supplied sources.
- **Source status:** The list records the failure, but no supporting error or
  stack trace was available to merge.

### 2. `adapter_validation_rejects_special_characters`

- **Status:** Still failing.
- **Panic location:** `tests/integration_tests.rs:2005:9`.
- **Error message:**

  ```text
  error message should not execute injected payloads for adapter: '../../../etc/passwd'
  ```

- **Stack trace:** The complete captured block is in
  [`test_stack_traces.txt:308-375`](test_stack_traces.txt#L308). Its key frames
  identify `integration_tests::adapter_validation_rejects_special_characters`
  at `tests/integration_tests.rs:2005:9` and the test entry point at
  `tests/integration_tests.rs:1985:50`.

### 3. `ci_verification_test_failure`

- **Status:** New failure.
- **Panic location:** `tests/integration_tests.rs:7027:5`.
- **Error message:**

  ```text
  CI VERIFICATION TEST FAILURE - This test is meant to fail to verify needle-ci retryStrategy fix surfaces test failures correctly
  ```

- **Stack trace:** The complete captured block is in
  [`test_stack_traces.txt:378-436`](test_stack_traces.txt#L378). The panic
  originates in `integration_tests::ci_verification_test_failure` at
  `tests/integration_tests.rs:7027:5`.

### 4. `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead`

- **Status:** Still failing.
- **Panic location:** `tests/integration_tests.rs:2634:5`.
- **Error message:**

  ```text
  br create failed:
  ```

- **Stack trace:** The complete captured block is in
  [`test_stack_traces.txt:440-498`](test_stack_traces.txt#L440). The test
  function later appears at `tests/integration_tests.rs:2713:18`.

### 5. `cross_workspace_mend_skips_beads_with_live_assignees`

- **Status:** Still failing.
- **Panic location:** `tests/integration_tests.rs:2782:5`.
- **Error message:**

  ```text
  br create failed
  ```

- **Stack trace:** The complete captured block is in
  [`test_stack_traces.txt:502-560`](test_stack_traces.txt#L502). The test
  function later appears at `tests/integration_tests.rs:2876:6`.

### 6. `cross_workspace_mend_skips_own_worker_beads`

- **Status:** Still failing.
- **Panic location:** `tests/integration_tests.rs:2904:5`.
- **Error message:**

  ```text
  br create failed
  ```

- **Stack trace:** The complete captured block is in
  [`test_stack_traces.txt:564-622`](test_stack_traces.txt#L564). The test
  function later appears at `tests/integration_tests.rs:2984:6`.

### 7. `dead_worker_cleanup_integration`

- **Status:** Still failing.
- **Panic location:** `tests/integration_tests.rs:3379:5`.
- **Error message:**

  ```text
  needle worker failed with exit status: ExitStatus(unix_wait_status(512))
  ```

- **Stack trace:** The complete captured block is in
  [`test_stack_traces.txt:626-685`](test_stack_traces.txt#L626). The test
  function later appears at `tests/integration_tests.rs:3409:6`.

### 8. `idle_worker_flagging_detects_stuck_workers`

- **Status:** New failure in the 2026-08-21 list.
- **Error message:** No test-specific message for this name appears in
  `test_error_messages.txt` or the current stack-trace capture.
- **Stack trace:** Not captured in the supplied sources.
- **Source status:** The list records the failure, but no supporting error or
  stack trace was available to merge.

### 9. `mend_removes_stale_dependency_links`

- **Status:** New failure in the 2026-08-21 list.
- **Error message:** No test-specific message for this name appears in
  `test_error_messages.txt` or the current stack-trace capture.
- **Stack trace:** Not captured in the supplied sources.
- **Source status:** The list records the failure, but no supporting error or
  stack trace was available to merge.

## Run-level error messages

The supplied error-message source contains one CI verification failure rather
than errors keyed to individual tests:

```text
CI FAILURE: cargo fmt -- --check (Formatting Check)
The CI pipeline failed during formatting validation. The following files have
formatting differences that must be fixed before the tests can run:
```

The source reports 20 formatting differences across these four files:

| File | Differences |
| --- | ---: |
| `src/bead_store/mod.rs` | 5 |
| `src/canary/mod.rs` | 5 |
| `src/cli/mod.rs` | 2 |
| `src/health/mod.rs` | 8 |
| **Total** | **20** |

The recorded CI failure metadata is:

```text
FIX INSTRUCTIONS: Run `cargo fmt` to automatically reformat all files according to rustfmt's style guide.
CI WORKFLOW: needle-ci-glr9w
FAILED STEP: verify (cargo fmt -- --check)
EXIT CODE: 1
TIMESTAMP: 2026-08-16T05:08:12.968Z
```

The complete diff hunks and original wording remain available in
[`test_error_messages.txt`](test_error_messages.txt).

## Coverage summary

| Acceptance item | Result |
| --- | --- |
| Failing test list included | 9 current list entries are included and keyed by canonical test name. |
| Error messages included | Six test-specific messages plus the complete run-level CI error summary are included. |
| Stack traces included | Six current trace blocks are linked by test name, panic location, and source line range. |
| Organized by test name | All listed tests have their own section; missing source details are explicitly marked. |
