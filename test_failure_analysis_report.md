# NEEDLE Test Failure Analysis Report

**Date:** 2026-08-21
**Bead:** `needle-e416ead5`
**Scope:** Captured library/integration test failures, stack-trace captures, test-result verification notes, and the historical needle-ci investigation.

## Executive summary

The complete current capture is [`test_error_details.md`](test_error_details.md). It records **14 distinct failure records**:

- 3 library-test failures;
- 10 integration-test failures reproduced individually with exit 101; and
- 1 integration test that failed its assertion and also caused the full serial run to hit the 240-second timeout (exit 124).

The failures are not one random cluster:

1. **Five failures are caused by stale bead-forge test fixtures** after this workspace moved to bead-rs. They invoke `br`/`bf` with the wrong binary or the wrong argument dialect.
2. **Two failures invoke a removed CLI command** (`needle worker`); the current CLI exposes `needle run`. One of these also falls back to an older PATH binary because the test does not require `CARGO_BIN_EXE_needle` to be present.
3. **Four failures are deterministic test/schema or implementation mismatches** in config reload bookkeeping, the documented OTLP schema, workspace-name sanitization, and adapter-error redaction.
4. **One failure is intentional**: `ci_verification_test_failure` is explicitly designed to panic so CI failure propagation can be tested. It must not be counted as a product regression.
5. **One failure is timing-sensitive functional behavior**: the worker does not process the delayed bead in `exhaustion_with_idle_action_wait_survives_sleep`; the full run then exceeds its external timeout.
6. **One failure is a test-fixture/backend-binding mismatch**: the supervisor test creates a temporary workspace without the authoritative backend binding now required by `Supervisor::new`.

There is no evidence in the captures of a segmentation fault, OOM kill, or random Rust runtime panic. The Rust test harness reports panics because assertions and `expect` calls fail; the underlying causes are mostly contract/fixture failures. Environment-dependent issues are explicitly flagged below.

## Evidence and capture reconciliation

| Evidence | Date/shape | What it proves | Limitation |
|---|---|---|---|
| `test_error_details.md` | 2026-08-21 | Most complete inventory: 14 records, with independent rerun notes and full traces | It is a captured result, not a new run performed for this report |
| `test_stack_traces.txt` | 2026-08-21 | One status pass listed 10 failed integration tests; it also contains a later six-trace section | The file concatenates multiple passes, so its first-pass count and final organized count differ |
| `test_stack_traces_organized.txt` | 2026-08-21 | Six failures were organized from one targeted capture | Partial snapshot, not the complete current suite |
| `failed_tests_stack_traces.txt`, `test_stack_traces_full.txt` | 2026-08-17 | Earlier five-failure baseline | Superseded by later source changes and the current 14-record capture |
| `verification-failing-tests-list-2026-08-20.md` | 2026-08-20 | Correctly reconciles the older five-failure capture | Does not describe the current suite |
| `verification-failing-tests-list-current-2026-08-21.md` | 2026-08-21 | Identifies documentation drift | Claims nine failures, but omits the three library failures and the delayed-idle timeout; its `adapter_validation_happens_before_main_worker_loop` entry is contradicted by the raw run, where that test is `ok` |
| `docs/needle-ci-failure-investigation-2026-08-16.md` | 2026-08-16 | Historical CI failure categories | Separate from the local test-suite baseline |

For acceptance purposes, the 14-record inventory in `test_error_details.md` is the baseline. The six-record organized file and the nine-record verification note are partial/outdated snapshots, not contradictory evidence of flakiness.

## Current failure inventory

### Library tests

| Test | Type | Classification | Analysis |
|---|---|---|---|
| `config::config_tests::changed_sections_detects_multiple_section_changes` (`src/config/mod.rs:11807`) | Assertion | **Test setup defect / deterministic** | The test clears `config2.limits.providers`, but `LimitsConfig` derives `Default` and starts with an empty provider map (`src/config/mod.rs:5131-5138`). That edit does not change the section, so expecting at least three changed sections is invalid. The two real edits should produce two changes; this is not environment-dependent. |
| `config::config_tests::test_otlp_config_matches_plan_md` (`src/config/mod.rs:11531`) | YAML deserialization assertion | **Documentation/schema drift / deterministic** | The test feeds `telemetry.otlp`, while the current config field is `telemetry.otlp_sink`. The failure reports `unknown field 'otlp'`. This is a real contract mismatch between the test/documented plan and the current schema, not a host issue. |
| `strand::pluck::tests::sanitize_workspace_name_handles_various_paths` (`src/strand/pluck.rs:2445`) | Assertion | **Implementation edge-case bug / deterministic** | `sanitize_workspace_name` uses `rsplit('/').next().unwrap_or("unknown")`; an empty input returns `Some("")`, not `None`, so it returns `""` instead of the test's expected `"unknown"`. No filesystem or environment dependency is involved. |

### Integration tests

| Test | Type / observed result | Classification | Analysis |
|---|---|---|---|
| `adapter_validation_rejects_special_characters` (`tests/integration_tests.rs:2005`) | Assertion, exit 101 | **Test expectation defect / deterministic** | The worker correctly rejects the adapter. The assertion bans the literal substring `etc/passwd`, even though the safe error includes the user-supplied adapter name. The capture shows no evidence that `whoami`, shell substitution, or a command executed; the assertion is broader than the security property it intends to test. |
| `ci_verification_test_failure` (`tests/integration_tests.rs:7025-7027`) | Intentional `panic!`, exit 101 | **Expected test fixture** | The source says this test is intentionally broken to verify that needle-ci surfaces failures and retry behavior. Exclude it from product-failure counts; run it only in the CI verification scenario that expects failure. |
| `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` (`tests/integration_tests.rs:2596`) | CLI setup failure / assertion | **Bead backend migration mismatch; environment-dependent** | The test hardcodes `/home/coding/.local/bin/br`, uses bead-forge-style `create --type=task`, then builds a `bead-forge` store through a helper that resolves `bf`. The current workspace is explicitly `bead-rs`; `bf` is absent, and the installed `br` path is a compatibility alias to bead-rs whose `create` syntax does not accept `--type`. |
| `cross_workspace_mend_skips_beads_with_live_assignees` (`tests/integration_tests.rs:2746`) | CLI setup failure / assertion | **Bead backend migration mismatch; environment-dependent** | Same shared root cause as the previous test: hardcoded legacy CLI and bead-forge store setup. The failure occurs before the cross-workspace behavior is exercised. |
| `cross_workspace_mend_skips_own_worker_beads` (`tests/integration_tests.rs:2879`) | CLI setup failure / assertion | **Bead backend migration mismatch; environment-dependent** | Same shared root cause. The test’s bead creation path is legacy and fails before the worker-assignee logic runs. |
| `idle_worker_flagging_detects_stuck_workers` (`tests/integration_tests.rs:3167`) | Missing executable panic, exit 101 | **Bead backend migration mismatch; environment-dependent** | `configured_forge_store` resolves `bf` and falls back to `/home/coding/.local/bin/bf`; the current host has no such executable. This is a fixture/backend-selection failure, not a failure of idle-worker flagging. |
| `mend_removes_stale_dependency_links` (`tests/integration_tests.rs:2992`) | CLI argument failure / assertion | **Bead backend migration mismatch; environment-dependent** | The test mixes bead-rs-incompatible forms: `br dep add <blocker> --blocks <blocked>` instead of bead-rs’s `<blocked> <blocker> --kind blocks`, then invokes `/home/coding/.local/bin/bf close`. It cannot reach stale-dependency logic in the current environment. |
| `dead_worker_cleanup_integration` (`tests/integration_tests.rs:3285`) | Child exit status 2 reported as `ExitStatus(unix_wait_status(512))` | **Removed CLI contract; deterministic test defect** | The test launches `needle worker --once`, but the current CLI has `needle run` and no `worker` subcommand. Status 512 is the encoded child exit code 2 (CLI usage), not a crash or signal. The test fails before dead-worker cleanup is exercised. |
| `subprocess_adapter_failure_exits_nonzero` (`tests/integration_tests.rs:6840`) | Assertion, exit 101 | **Removed CLI contract plus environment-sensitive binary selection** | It also launches `needle worker`. Its fallback `"needle"` is used when `CARGO_BIN_EXE_needle` is unavailable; the PATH binary on this host exposes `run`, not `worker`, producing `unrecognized subcommand 'worker'`. The test’s intended adapter-validation assertion is therefore never reached. |
| `worker_binary_path_supervisor_initialization` (`tests/integration_tests.rs:3963`) | Supervisor initialization error, exit 101 | **Fixture/backend-binding mismatch** | `Supervisor::new` now discovers the store using the workspace’s authoritative backend binding. The test only runs `br init` in a temporary directory and does not create the required binding, so initialization fails before worker-binary-path behavior is tested. |
| `exhaustion_with_idle_action_wait_survives_sleep` (`tests/integration_tests.rs:874`) | Assertion plus full-run timeout (exit 124) | **Functional/timing-sensitive failure; environment-dependent timeout** | The delayed mock store is intended to expose a bead after the first idle cycle, but `worker.beads_processed()` remains 0. In the full serial run this scenario ran for more than 60 seconds and the enclosing 240-second timeout fired. The assertion indicates a worker idle/wake or mock-call sequencing defect; the timeout duration is harness/environment-dependent and should not be treated as proof of a deadlock without a targeted run. |

## Failure-type totals

These totals classify the **underlying** failure rather than the Rust harness’s common `panicked at` presentation:

| Underlying type | Count | Tests / evidence |
|---|---:|---|
| Deterministic assertion or schema/implementation mismatch | 4 | `changed_sections...`, `test_otlp_config...`, `sanitize_workspace_name...`, `adapter_validation_rejects_special_characters` |
| Stale external CLI/backend fixture | 5 | Three cross-workspace mend tests, `idle_worker_flagging_detects_stuck_workers`, `mend_removes_stale_dependency_links` |
| Removed CLI invocation / subprocess contract | 2 | `dead_worker_cleanup_integration`, `subprocess_adapter_failure_exits_nonzero` |
| Backend-binding fixture failure | 1 | `worker_binary_path_supervisor_initialization` |
| Timing-sensitive worker behavior | 1 | `exhaustion_with_idle_action_wait_survives_sleep` |
| Intentional panic test | 1 | `ci_verification_test_failure` |
| **Total current records** | **14** | Includes the delayed-idle record once, even though it also timed out in the full run |

There are no captured segmentation faults or explicit OOM failures. The `ExitStatus(512)` report is a normal child exit code 2 encoded by Unix wait status. The full-run exit 124 is an external `timeout` termination, not a test-generated exit code.

## Environment-dependent issues

### Confirmed current environment dependencies

- **Missing bead-forge binary:** the current host has `bead` and `/home/coding/.local/bin/br` (a bead-rs-compatible alias), but no `/home/coding/.local/bin/bf`. Tests and helpers still select the bead-forge descriptor and `bf` path. This explains the missing-executable failure and contributes to the cross-workspace/mend failures.
- **CLI dialect drift:** the installed alias currently reports bead-rs commands. For example, its `create` command accepts `--issue-type`, not the old `--type`; its dependency command accepts two positional IDs and `--kind`, not `--blocks`. Tests are therefore sensitive to which backend binary is installed and selected.
- **Binary fallback sensitivity:** tests use `std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string())`. If Cargo does not provide the variable, PATH resolution can select an older installed binary. In this environment that binary exposes `run` and not the test’s requested `worker` subcommand.
- **Backend binding in temporary workspaces:** current supervisor/store discovery requires an authoritative workspace binding. A temporary directory initialized only by the legacy CLI is not a complete bead-rs fixture.
- **OTLP collector unavailable during captured tests:** `test_stack_traces.txt` contains repeated non-fatal `tcp connect error` / `Connection refused` messages for `127.0.0.1:4317`. These are exporter-environment warnings; affected tests still reported `ok`, so they are not test failures but can add noise and latency.
- **Full-run timeout sensitivity:** the delayed-idle test exceeded the enclosing 240-second timeout. CPU contention, shared worker activity, and serial-test duration can change whether the harness reports a timeout versus the underlying assertion, so both outcomes should be tracked separately.

### Historical environment/infrastructure failures

The 2026-08-16 CI investigation adds three environment-level patterns outside the 14-record local baseline:

| Pattern | Classification | Evidence |
|---|---|---|
| Git clone exit 128 with Forgejo HTTP 503 (`remote: no available server`) | **Transient CI/Forgejo infrastructure** | `docs/needle-ci-failure-investigation-2026-08-16.md`, affected workflows `needle-ci-6gxtz`, `fz6m8`, `scskd`, `9r7h6`, `9thnk`, `hhbx7`; retries succeeded |
| Historical CI exit 101 with logs unavailable after pod retention | **Observability/retention issue** | Same report: failed pod/logs were deleted after roughly two hours, preventing root-cause analysis |
| Historical exit 1 from `cargo fmt --check` | **Deterministic repository quality failure** | Same report lists formatting changes in `src/canary/mod.rs`, `src/cli/mod.rs`, `src/config/mod.rs`, and `src/health/mod.rs`; not an environmental outage |
| Test-capture command initially passed `--test-threads=1` to Cargo instead of the test binary | **Capture-script defect** | `.beads/traces/needle-ebede907/trace.jsonl`; Cargo reported `unexpected argument '--test-threads'` |
| Capture worker exited with `ENOSPC` because its task filesystem had `0MB free` | **Environment-only storage failure** | `.beads/traces/needle-ebede907/trace.jsonl`; output was lost while writing stdout/stderr |

The current host snapshot has adequate available memory and tens of GB of free disk space, so the historical `ENOSPC` incident should not be generalized to the current machine state. It remains relevant because it can invalidate a capture without representing a code failure.

## Pattern conclusions

- **Not “all checkpoint tests” and not random single-test noise.** The failures cluster by shared dependency: five tests use the same obsolete bead-forge fixture path, two use the same obsolete `needle worker` command, and the four deterministic failures each map to a specific source/test contract.
- **The six-failure organized capture is incomplete, not evidence that four failures disappeared randomly.** The raw file contains multiple passes with different states; `test_error_details.md` adds the library suite, the delayed-idle timeout, and independently rerun results.
- **The current verification note is also incomplete.** Its nine-test list omits the three library failures and the delayed-idle timeout and incorrectly labels a passing adapter-preflight test as new. It should not be used as the baseline.
- **Environment dependence is concentrated in external-process setup and capture infrastructure.** The core deterministic failures can be reproduced without a bead CLI, network service, or filesystem scan. The backend and subprocess failures cannot be interpreted as worker-strand regressions until fixtures use the configured backend and current CLI surface.

## Recommended follow-up order

1. Split intentional CI-failure tests from normal verification or mark them as expected failures.
2. Migrate integration fixtures to the configured bead-rs descriptor and `bead` binary; remove hardcoded `/home/coding/.local/bin/br` and `/home/coding/.local/bin/bf` paths.
3. Replace `needle worker` subprocess invocations with the current direct-worker test interface or `needle run` command, and require/use the Cargo-provided binary path.
4. Add an explicit bead-rs binding to temporary supervisor fixtures.
5. Fix the four deterministic config/validation mismatches, then isolate and rerun the delayed-idle test with a test-local timeout and call-count diagnostics.
6. Preserve CI logs/artifacts beyond the two-hour pod-retention window and keep capture output on a filesystem with a monitored free-space threshold.

## Verification status

This report is an analysis/documentation deliverable. It does not claim that the captured tests are fixed. The repository had unrelated concurrent `.beads` and capture-artifact modifications before this report was created; only this report is intended to be staged for the bead.
