# Integration-test target inventory (2026-09-12)

This is the file-by-file input to `needle-c657a2a8`, the integration-test
binary consolidation. It is an inventory, not authorization to remove tests
outside the rows explicitly marked **delete**.

## Snapshot and counting contract

- Repository snapshot: `dc5014464f365624aa8da88985d8756d85ccdbe3`.
- `Cargo.toml` has no `autotests = false`. Cargo metadata therefore reports
  **125 integration-test targets**: the explicitly declared
  `integration_spawn` target and 124 other auto-discovered top-level files.
- The 125 top-level `tests/*.rs` files contain **1,504 literal `#[test]` or
  `#[tokio::test]` annotations**. Nested sources add 18 annotations (14 in
  `tests/helpers/polling.rs`, two in `tests/helpers/retry.rs`, and two in
  `tests/pending/orphaned_bead_recovery_test.rs`), for 1,522 annotations in the
  entire `tests/` tree.
- Annotation count is deliberately a source count, not `cargo test -- --list`:
  cfg-disabled tests are still annotations, generated cases may enumerate more
  than once, and helper modules included by multiple crates enumerate once per
  including crate.
- The five intended consolidation roots are `integration_spawn`,
  `integration_tests`, `p2_integration_tests`, `p3_integration_tests`, and
  `real_br_integration_tests`. The current DoD slow lane runs the latter four
  plus `--lib`; it does not run `integration_spawn`. Consolidation must either
  add that target to the intended lane or deliberately reassign its process
  tests before disabling auto-discovery.

The worktree already had an authorized in-flight deletion of
`verification_failure_aggregation.rs` by the owner retiring the duplicate
verification runner; its useful aggregation assertions moved to
`tests/dod-modes/run.sh`. It remains in this snapshot inventory and is
classified for deletion, so the pre-change 125-target snapshot and the
post-deletion 124-file worktree are both accounted for.

## Meaning of the columns

- **Reference**: `auto` means Cargo auto-discovery is the only reference.
  `module` names source inclusion by another test crate. `root` means an
  intended destination root referenced by Cargo or DoD.
- **Overlap** is conservative. “Topical” means nearby coverage exists but this
  review found distinct scenarios or assertions, so the file is retained.
  “Exact” identifies an actually repeated case. No whole file is deleted merely
  because its subject resembles another suite.
- **Action / destination** is the complete migration decision. “Fold” retains
  the tests as a module below the named root. Roots remain roots.

## Complete map

| File | Annotations | Reference | Overlap assessment | Action / destination |
|---|---:|---|---|---|
| `adapter_validation_tests.rs` | 18 | auto; docs example only | Self-tests an otherwise unused test-fixture scaffold; production adapter behavior is covered by routing/adapter suites | **Delete**: orphan fixture scaffold, no code consumer |
| `alert_deduplication_test.rs` | 9 | auto | Topical with alert fingerprint and P3 Knot coverage; distinct suppression-window/store cases | Fold -> `p3_integration_tests` |
| `alert_fingerprint_integration.rs` | 3 | auto | Topical with preceding file; distinct end-to-end alert sequence | Fold -> `p3_integration_tests` |
| `anthropic_routing_e2e_test.rs` | 5 | auto; shell verifier mentions target | Topical with routing suites; distinct built-in Anthropic policy cases | Fold -> `p3_integration_tests` |
| `anthropic_routing_verification.rs` | 10 | auto; shell verifier mentions target | Topical with routing suites; distinct matcher/config assertions | Fold -> `p3_integration_tests` |
| `atomic_spawn_verification.rs` | 2 | auto | Topical with P2 claim tests; distinct immediately-before-spawn race proof | Fold -> `p2_integration_tests` |
| `backend_strategy_validation.rs` | 3 | auto | Topical with backend descriptor tests; distinct strategy validation | Fold -> `integration_tests` |
| `bead_backend_descriptors.rs` | 8 | auto | Topical with P2 backend use; distinct descriptor loading contract | Fold -> `integration_tests` |
| `bead_cli_argv_assertions.rs` | 6 | auto | Distinct exact-argv fixture contract | Fold -> `p2_integration_tests` |
| `bead_cli_config_serde.rs` | 41 | auto | Topical with config suites; distinct backend-config serde matrix | Fold -> `integration_tests` |
| `bead_rehydration_verification.rs` | 3 | auto | Topical with real backend root; distinct rehydration playbook | Fold -> `real_br_integration_tests` |
| `bead_rs_lifecycle.rs` | 4 | auto | Topical with real backend root; distinct ignored/explicit lifecycle contract | Fold -> `real_br_integration_tests` |
| `benchmark_harness_smoke.rs` | 6 | auto | Topical with p95/benchmark output; distinct harness construction | Fold -> `integration_tests` |
| `benchmark_output_format.rs` | 5 | auto | Topical with p95 suites; distinct report-format assertions | Fold -> `integration_tests` |
| `binary_freshness_edge_cases.rs` | 13 | auto | Topical with P3 hot reload; distinct error/boundary matrix | Fold -> `p3_integration_tests` |
| `binary_freshness_fix_loop_e2e.rs` | 6 | auto | Topical with freshness suites; distinct process lifecycle | Fold -> `integration_spawn` |
| `binary_freshness_integration.rs` | 6 | auto | Topical with freshness suites; distinct checker/rotation behavior | Fold -> `p3_integration_tests` |
| `binary_freshness_logging.rs` | 7 | auto | Topical with freshness suites; distinct logging/dedup contract | Fold -> `p3_integration_tests` |
| `checkpoint_dispatch_guard.rs` | 4 | auto | Distinct checkpoint-readiness dispatch contract | Fold -> `real_br_integration_tests` |
| `checkpoint_roundtrip_fidelity.rs` | 5 | auto | Topical with real backend root; distinct full checkpoint round trip | Fold -> `real_br_integration_tests` |
| `claim_cycle_span_depth_regression.rs` | 2 | auto | Distinct regression for tracing span leakage across claims | Fold -> `p2_integration_tests` |
| `claim_strategies.rs` | 4 | auto | Topical with P2 claiming; distinct strategy selection | Fold -> `p2_integration_tests` |
| `cleanup_function_error_handling_tests.rs` | 33 | auto | Topical with panic/process suites; one exact-named already-killed guard case also occurs in `panic_safety_verification`; remaining matrix is distinct | Fold -> `integration_spawn` |
| `cleanup_liveness_regression.rs` | 6 | auto; modules `tmux_fixture` | Distinct real tmux/process-tree cleanup regression | Fold -> `integration_spawn` |
| `cli_bead_store_engine.rs` | 13 | auto | Topical with real backend tests but uses a fixture CLI to assert engine behavior | Fold -> `p2_integration_tests` |
| `cli_integration.rs` | 5 | auto | Distinct real `needle` CLI subprocess coverage | Fold -> `integration_spawn` |
| `compilation_error_detection.rs` | 24 | auto | Distinct Cargo-output parsing and trace cases | Fold -> `integration_tests` |
| `concurrent_startup_test.rs` | 1 | auto | Topical with process discovery; distinct incomplete-cmdline startup race | Fold -> `integration_spawn` |
| `config_cli_tests.rs` | 19 | auto | Topical with config suites; distinct CLI parsing/subprocess cases | Fold -> `integration_spawn` |
| `config_key_path_integration.rs` | 93 | auto | Distinct public key-path matrix | Fold -> `integration_tests` |
| `default_routing_uses_builtin_adapters.rs` | 1 | auto | Topical with routing suites; distinct default-adapter invariant | Fold -> `p3_integration_tests` |
| `descriptor_conformance_tests.rs` | 16 | auto | Topical with backend descriptors; distinct executable descriptor conformance | Fold -> `integration_tests` |
| `dispatch_model_routing_validation.rs` | 15 | auto | Topical with routing suites; distinct dispatcher/model handoff | Fold -> `p3_integration_tests` |
| `doctor_exit_code_tests.rs` | 18 | auto | Distinct real CLI exit-code matrix | Fold -> `integration_spawn` |
| `double_dispatch_prevention.rs` | 4 | auto | Topical with P2 claiming; distinct stale/raced claim prevention | Fold -> `p2_integration_tests` |
| `edge_case_panic_tests.rs` | 37 | auto | Topical with panic suites; broad input/limit matrix is distinct | Fold -> `integration_tests` |
| `end_to_end_telemetry_test.rs` | 5 | auto | Topical with telemetry suites; distinct event pipeline coverage | Fold -> `p3_integration_tests` |
| `error_log_verification.rs` | 17 | auto; modules `log_capture_helper` | Topical with telemetry/log suites; distinct operational error messages | Fold -> `integration_spawn` |
| `error_path_panic_tests.rs` | 18 | auto | Topical with panic suites; distinct propagated-error contexts | Fold -> `integration_tests` |
| `etxtbsy_retry.rs` | 16 | auto | Topical with lib retry tests; distinct public wrapper/integration behavior | Fold -> `integration_spawn` |
| `file_sink_integration.rs` | 6 | auto | Topical with telemetry root; distinct filesystem sink behavior | Fold -> `p3_integration_tests` |
| `gate_health_degradation_integration.rs` | 7 | auto | Topical with alert/Knot root; distinct degradation thresholds | Fold -> `p3_integration_tests` |
| `github_release_upgrade_regression.rs` | 12 | auto | Topical with upgrade tests; distinct GitHub release regression cases | Fold -> `p3_integration_tests` |
| `hard_timeout_tests.rs` | 7 | auto | Topical with timeout config; distinct live hard-deadline execution | Fold -> `integration_spawn` |
| `heartbeat_validation.rs` | 3 | auto; modules `log_capture_helper` | Topical with P2 heartbeat root; distinct log assertions | Fold -> `integration_spawn` |
| `hot_reload_reexec.rs` | 4 | auto | Topical with P3 hot reload; distinct exec/process behavior | Fold -> `integration_spawn` |
| `idle_timeout_tests.rs` | 6 | auto | Topical with timeout config; distinct live idle-reset behavior | Fold -> `integration_spawn` |
| `immediate_check_trigger.rs` | 15 | auto | Topical with interval/polling suites; contains one exact `disabled_poller_never_runs` duplicate with `interval_calculation` but unique checker-call assertions | Fold -> `p3_integration_tests` |
| `init_cli_tests.rs` | 25 | auto | Distinct real init CLI behavior | Fold -> `integration_spawn` |
| `integration_spawn.rs` | 0 | explicit Cargo root | Empty destination stub, not a duplicate test | **Keep root** `integration_spawn` |
| `integration_tests.rs` | 6 | DoD root | Existing quarantine-flow root | **Keep root** `integration_tests` |
| `interval_calculation.rs` | 17 | auto | Topical with immediate/supervisor polling; one exact disabled-poller case, other boundary cases distinct | Fold -> `p3_integration_tests` |
| `label_import_strategies.rs` | 5 | auto | Distinct backend label import strategies | Fold -> `p2_integration_tests` |
| `log_capture_helper.rs` | 9 | auto and module of three suites | Its self-tests are useful; current inclusion executes them redundantly in four crates | Fold once -> `integration_spawn`; make sibling helper for consumers |
| `logs_stats_cli_tests.rs` | 13 | auto | Distinct query CLI output/argument behavior | Fold -> `integration_spawn` |
| `long_lived_worker_binary_rotation.rs` | 7 | auto | Topical with freshness suites; distinct supervisor lifetime cases | Fold -> `p3_integration_tests` |
| `manual_upgrade_path_tests.rs` | 9 | auto | Topical with upgrade suites; distinct manual-channel policy | Fold -> `p3_integration_tests` |
| `mend_multi_claim_staleness.rs` | 19 | auto | Topical with P2 Mend; distinct multi-claim staleness matrix | Fold -> `p2_integration_tests` |
| `multi_iteration_p95_validation.rs` | 7 | auto | Topical with p95 suites; distinct multi-iteration aggregation | Fold -> `integration_tests` |
| `needle_transform_claude.rs` | 4 | auto | Distinct transform-binary CLI behavior | Fold -> `integration_spawn` |
| `otlp_integration.rs` | 3 | auto | Topical with other OTLP suites; distinct event-to-export integration | Fold -> `p3_integration_tests` |
| `otlp_runtime_test.rs` | 4 | auto | Topical with OTLP suites; distinct runtime lifecycle | Fold -> `p3_integration_tests` |
| `otlp_transport_seam_tests.rs` | 5 | auto | Topical with OTLP suites; distinct exporter seam/error cases | Fold -> `p3_integration_tests` |
| `p2_integration_tests.rs` | 27 | DoD root | Existing fleet/Pluck/Mend/Explore/Mitosis root | **Keep root** `p2_integration_tests` |
| `p3_integration_tests.rs` | 25 | DoD root | Existing strand/validation/telemetry/release root | **Keep root** `p3_integration_tests` |
| `p95_aggregation.rs` | 6 | auto | Topical with other p95 suites; distinct aggregation rules | Fold -> `integration_tests` |
| `p95_correctness.rs` | 7 | auto | Topical with other p95 suites; distinct percentile correctness cases | Fold -> `integration_tests` |
| `panic_safety_verification.rs` | 12 | auto | Topical with panic/cleanup suites; one exact-named process-group case but remaining cleanup guarantees distinct | Fold -> `integration_spawn` |
| `panic_stack_trace_capture.rs` | 13 | auto | Distinct panic-hook and stack-trace capture | Fold -> `integration_spawn` |
| `placeholder_validation_tests.rs` | 15 | auto | Topical with template suites; distinct load-time placeholder validation | Fold -> `integration_tests` |
| `polling_infrastructure_skeleton.rs` | 17 | auto; modules nested helpers | Placeholder/TODO scaffold; active cases duplicate interval/immediate/supervisor suites and ignored cases test fake infrastructure | **Delete**: superseded scaffold |
| `post_dispatch_audit_test.rs` | 3 | auto | Distinct verification-child folding audit | Fold -> `p3_integration_tests` |
| `process_discovery_integration.rs` | 2 | auto | Topical with verify-process suites; distinct real non-tmux/reconciliation execution | Fold -> `integration_spawn` |
| `process_guard.rs` | 4 | auto; no module consumer | Tests an orphan custom test-only guard while production `needle::process_guard` has direct coverage | **Delete**: unused duplicate fixture |
| `process_limits_config_tests.rs` | 11 | auto | Topical with timeout config; distinct hard-deadline validation | Fold -> `integration_tests` |
| `property_tests.rs` | 11 | auto | Distinct randomized core invariants | Fold -> `integration_tests` |
| `query_integration_test.rs` | 12 | auto | Topical with logs/stats CLI; distinct query layer behavior | Fold -> `p3_integration_tests` |
| `real_br_integration_tests.rs` | 32 | DoD root | Existing native bead-rs root | **Keep root** `real_br_integration_tests` |
| `remaining_config_tilde_expansion_tests.rs` | 31 | auto | Topical with other tilde suites; distinct remaining config sections | Fold -> `integration_tests` |
| `retry_infrastructure_examples.rs` | 21 | auto; modules `retry_test_helpers` | Exercises only the fake retry framework; production ETXTBSY behavior has lib and integration coverage | **Delete** with helper scaffold |
| `retry_test_helpers.rs` | 58 | auto and module of retry examples | Self-tests an otherwise unused fake retry framework, so its tests currently enumerate twice | **Delete** with example scaffold |
| `routing_integration.rs` | 48 | auto | Topical with P3 routing; broad end-to-end rule matrix is distinct | Fold -> `p3_integration_tests` |
| `routing_matcher_baseline.rs` | 7 | auto | Topical with routing suite; distinct precedence baseline | Fold -> `p3_integration_tests` |
| `routing_telemetry_verification.rs` | 12 | auto | Topical with routing and telemetry; distinct emitted-field contract | Fold -> `p3_integration_tests` |
| `sanitize_latency_assertion.rs` | 7 | auto | Topical with benchmark suites; distinct latency budget gate | Fold -> `integration_tests` |
| `show_method_tests.rs` | 11 | auto | Distinct bead-store `show` fixture-CLI contract | Fold -> `p2_integration_tests` |
| `sigpipe_test.rs` | 1 | auto | Distinct real CLI pipe-close regression | Fold -> `integration_spawn` |
| `sigterm_heartbeat_cleanup.rs` | 10 | auto; modules `log_capture_helper` | Distinct signal/heartbeat process behavior | Fold -> `integration_spawn` |
| `split_strategies.rs` | 5 | auto | Topical with P2 Mitosis; distinct split algorithms | Fold -> `p2_integration_tests` |
| `starvation_tests.rs` | 17 | auto | Topical with P3 Knot; distinct starvation scenario matrix | Fold -> `p3_integration_tests` |
| `stop_kills_process_tree.rs` | 2 | auto | Distinct real process-tree termination regression | Fold -> `integration_spawn` |
| `strand_tilde_expansion_tests.rs` | 21 | auto | Topical with tilde suites; distinct strand fields | Fold -> `integration_tests` |
| `supervisor_periodic_polling.rs` | 22 | auto | Topical with interval/immediate suites; distinct supervisor/backoff state | Fold -> `p3_integration_tests` |
| `telemetry_field_verification.rs` | 12 | auto | Topical with telemetry root; distinct event field assertions | Fold -> `p3_integration_tests` |
| `template_comprehensive_tests.rs` | 34 | auto | Topical with template/placeholder suites; distinct malformed/performance/pipeline matrix | Fold -> `integration_tests` |
| `template_rendering_tests.rs` | 10 | auto | Topical with comprehensive template suite; distinct required-placeholder happy paths | Fold -> `integration_tests` |
| `test_bead_visibility.rs` | 4 | auto | Topical with P2 Pluck; distinct candidate-visibility diagnostics | Fold -> `p2_integration_tests` |
| `test_echo_simple.rs` | 1 | auto | Tests `bash -c 'echo done'`, writes a fixed `/tmp` path, and exercises no NEEDLE behavior | **Delete**: diagnostic scratch test |
| `test_helper_example.rs` | 3 | auto but entirely `cfg(feature = "integration")` | Default build enumerates none; duplicates `src/telemetry/test_utils.rs` self-tests | **Delete**: redundant helper example |
| `test_llms_drift.rs` | 1 | auto | Distinct documentation drift gate | Fold -> `integration_tests` |
| `test_mend_stale_assignee.rs` | 3 | auto | Topical with P2 Mend; distinct live-heartbeat/different-bead regression | Fold -> `p2_integration_tests` |
| `test_no_tmp_in_fixtures.rs` | 1 | auto | Distinct source lint preventing shared `/tmp` bead roots | Fold -> `integration_tests` |
| `test_otlp_config_syntax.rs` | 10 | auto | Topical with config/OTLP suites; distinct syntax matrix | Fold -> `p3_integration_tests` |
| `test_panic_safety_verification.rs` | 15 | auto | Topical with panic suites; distinct predispatch/path/command error matrix | Fold -> `integration_spawn` |
| `test_panic_timestamp_verification.rs` | 19 | auto | Topical with telemetry timestamps; distinct panic formatting/capture | Fold -> `p3_integration_tests` |
| `test_telemetry_write.rs` | 0 | auto | Standalone `main` diagnostic, no test or assertion contract; writes below real `HOME` | **Delete**: unsafe debug program |
| `test_telemetry_write_debug.rs` | 0 | auto | Standalone fixed-`/tmp` diagnostic, no test annotations | **Delete**: debug program |
| `timeout_config_integration.rs` | 39 | auto | Heavy topical overlap with `timeout_config_integration_tests`, but unique worker/validation/real-world and policy cases remain | Fold -> `integration_tests`; dedupe cases during move |
| `timeout_config_integration_tests.rs` | 28 | auto | Heavy topical overlap with preceding file, but unique aliases/source-order/boundary cases remain | Fold -> `integration_tests`; dedupe cases during move |
| `timestamp_telemetry_tests.rs` | 12 | auto | Topical with panic timestamp/telemetry suites; distinct sink and ISO-format behavior | Fold -> `p3_integration_tests` |
| `tmux_fixture.rs` | 6 | auto and module of cleanup liveness | Reusable fixture self-tests currently enumerate twice; not duplicate product coverage | Fold once -> `integration_spawn`; keep as sibling helper |
| `unbuffer_regression_test.rs` | 1 | auto | Distinct real adapter exit-code subprocess regression | Fold -> `integration_spawn` |
| `upgrade_check_integration.rs` | 10 | auto | Topical with supervisor/upgrade suites; distinct CLI/config/poller integration | Fold -> `p3_integration_tests` |
| `verification_failure_aggregation.rs` | 8 | retired | Tested the duplicate configurable shell authority rather than the active DoD script | **Deleted** with retired runner |
| `verification_fingerprint_replay.rs` | 4 | auto; fixture README references it | Distinct incident replay for failure classification | Fold -> `p3_integration_tests` |
| `verify_bash_wrapper_exclusion.rs` | 1 | auto | Topical with discovery suites; distinct wrapper exclusion regression | Fold -> `integration_spawn` |
| `verify_bf_4390q.rs` | 2 | auto | Bead-specific name, but distinct public CargoTest combined trace-output contract | Fold -> `integration_tests` |
| `verify_deleted_binary_hot_reload.rs` | 3 | auto | Topical with hot reload; distinct deleted-binary process regression | Fold -> `integration_spawn` |
| `verify_process_discovery.rs` | 3 | auto | Topical with process discovery; distinct status/list reconciliation and descendant filtering | Fold -> `integration_spawn` |
| `version_probe_comprehensive.rs` | 34 | auto | Heavy topical overlap with `version_probe_test`; unique malformed-output, timeout, availability and recovery matrix remains | Fold -> `integration_tests`; dedupe cases during move |
| `version_probe_test.rs` | 16 | auto | Heavy topical overlap with comprehensive suite; unique real git integration and binary-name behavior remains | Fold -> `integration_tests`; dedupe cases during move |
| `workspace_equality_tests.rs` | 19 | auto | Topical with checkpoint fidelity; distinct field-level workspace comparison | Fold -> `real_br_integration_tests` |
| `workspace_fixtures.rs` | 18 | auto; no code consumer | Self-tests a 1,500-line fake workspace framework that no other test imports | **Delete**: orphan fixture scaffold |
| `workspace_tilde_expansion_tests.rs` | 12 | auto | Topical with tilde suites; distinct workspace home/default fields | Fold -> `integration_tests` |
| `zcode_headless_adapter.rs` | 1 | auto | Distinct headless adapter invocation contract | Fold -> `integration_spawn` |

## Reconciliation with the old “1,711 tests” comment

The file assignment rolls up as follows:

| Decision | Files | Top-level annotations |
|---|---:|---:|
| Keep/fold under `integration_spawn` | 31 | 262 |
| Keep/fold under `integration_tests` | 30 | 550 |
| Keep/fold under `p2_integration_tests` | 13 | 105 |
| Keep/fold under `p3_integration_tests` | 34 | 372 |
| Keep/fold under `real_br_integration_tests` | 6 | 67 |
| Delete | 11 | 148 |
| **Total** | **125** | **1,504** |

The comment in the iad-ci WorkflowTemplate was added on 2026-08-05 and says a
failed run forced “local reproduction of a 1,711-test suite.” It is a dated
runtime observation, not an assertion about `tests/*.rs` and not the baseline
for the September consolidation epic.

At the end of 2026-08-05 (`d0ab4abe`) the repository contained 30 top-level
integration files with 297 literal test annotations, while `src/` contained
1,563. Runtime enumeration does not equal their sum because cfg selection,
module inclusion, and generated cases intervene. By the inventory snapshot,
top-level integration annotations alone had grown to 1,504 and `src/` held
3,819. The old number is therefore stale both in date and in what it measures.

The consolidation preservation baseline is:

1. 1,504 top-level annotations before migration.
2. 148 annotations in the eleven explicitly deleted scaffold/debug/retired
   files above.
3. 1,356 retained top-level annotations expected after migration, before any
   separately reviewed per-case deduplication.
4. 18 nested annotations must be handled deliberately: polling/retry helpers
   disappear with their scaffolds, while the two pending recovery tests require
   an explicit lane decision because their nested path is not currently an
   auto-discovered Cargo target.

Actual execution preservation should additionally compare `cargo test --
--list` before and after consolidation, per feature set and per intended root;
the annotation baseline catches silent source loss but cannot prove runtime
enumeration equivalence by itself.

## Migration cautions discovered by the inventory

- `log_capture_helper`, `tmux_fixture`, and `retry_test_helpers` are both
  top-level Cargo targets and `mod`-included sources today. This repeats their
  internal tests in multiple binaries. Move shared helpers once and import them
  from their destination tree rather than copying their `mod` declarations.
- The duplicate function names in polling and cleanup suites will collide only
  if files are textually concatenated. Keeping one module per source file avoids
  that; per-case deduplication can follow separately.
- `integration_spawn` is an explicit but empty target and is omitted from the
  current DoD slow lane. Do not move process tests beneath it until its execution
  policy is fixed.
- Feature-gated, ignored, real-CLI, tmux, timing, and documentation tests should
  retain those semantics after moving; “compiled” is not the same as “executed.”
