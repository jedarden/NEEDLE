# NEEDLE CI Strand Test Coverage Report

**Generated:** 2026-08-28  
**Investigation:** Inventory of current integration test targets and strand coverage

## Executive Summary

The needle-ci workflow currently executes **4 integration test targets** out of **65 total test files** in the repository. All core strand modules (Pluck, Mend, Explore, Weave, Unravel, Pulse, Reflect, Splice, Knot) have behavioral integration tests that run in CI, but **61 specialized test files are not executed by the CI pipeline**.

## Current CI Execution

### Test Targets Run by needle-ci

The `definition-of-done.sh` script (executed by needle-ci verify step) runs:

1. **`cargo test --lib`** - Unit tests (all modules)
2. **`cargo test --test integration_tests`** - Core worker lifecycle and outcomes
3. **`cargo test --test p2_integration_tests`** - Phase 2 strand integration (Explore, Mend)
4. **`cargo test --test p3_integration_tests`** - Phase 3 strand integration (Weave, Unravel, Pulse)
5. **`cargo test --test real_br_integration_tests`** - Real bead-rs backend tests

### CI Configuration
- **Workflow:** `needle-ci` WorkflowTemplate in `iad-ci`
- **Command:** `./scripts/definition-of-done.sh --all`
- **Builder:** `ronaldraygun/needle-ci-builder:0.1.5-with-deps`
- **Timeouts:** 900s per test target (after compilation)

## Strand Coverage Analysis

### All Strand Modules (from `src/strand/`)

| Strand | Purpose | CI Coverage | Test Files |
|--------|---------|-------------|------------|
| **Pluck** | Claims beads from ready frontier | ✅ `integration_tests.rs` | 5 files mention |
| **Mend** | Cleans stale claims/orphaned locks | ✅ `p2_integration_tests.rs`, `real_br_integration_tests.rs` | 5 files mention |
| **Explore** | Discovers work across workspaces | ✅ `p2_integration_tests.rs`, `real_br_integration_tests.rs` | 7 files mention |
| **Weave** | Gap analysis and bead creation | ✅ `p3_integration_tests.rs` | 4 files mention |
| **Unravel** | Alternatives for HUMAN-blocked beads | ✅ `p3_integration_tests.rs` | 3 files mention |
| **Pulse** | Codebase health scans | ✅ `p3_integration_tests.rs` | 3 files mention |
| **Reflect** | Telemetry reflection | ✅ `integration_tests.rs` | 2 files mention |
| **Splice** | Adapter system integration | ✅ `integration_tests.rs` | 3 files mention |
| **Knot** | Exhaustion handling | ✅ `integration_tests.rs` | 2 files mention |
| **Resolve** | Decision flow routing | ✅ (via unit tests) | Utility module |

**✅ All core strands have CI coverage**

## Test Files NOT Executed by CI

**61 out of 65 test files (94%) are not run by needle-ci**

### Major Categories of Non-CI Tests

#### Routing & Adapter Tests (11 files)
- `anthropic_routing_e2e_test.rs`
- `anthropic_routing_verification.rs`
- `dispatch_model_routing_validation.rs`
- `routing_integration.rs`
- `routing_matcher_baseline.rs`
- `routing_telemetry_verification.rs`
- `adapter_validation_tests.rs`
- `needle_transform_claude.rs`
- `template_rendering_tests.rs`

#### Telemetry & Observability (9 files)
- `telemetry_field_verification.rs`
- `test_telemetry_write.rs`
- `test_telemetry_write_debug.rs`
- `otlp_integration.rs`
- `otlp_runtime_test.rs`
- `otlp_transport_seam_tests.rs`
- `file_sink_integration.rs`
- `benchmark_output_format.rs`
- `p95_aggregation.rs`, `p95_correctness.rs`

#### Bead Store & Backend (8 files)
- `bead_backend_descriptors.rs`
- `bead_rehydration_verification.rs`
- `bead_rs_lifecycle.rs`
- `bf_cli_argv_assertions.rs`
- `backend_strategy_validation.rs`
- `claim_strategies.rs`
- `split_strategies.rs`
- `label_import_strategies.rs`

#### Process Management & Timeouts (10 files)
- `timeout_config_integration.rs`
- `timeout_config_integration_tests.rs`
- `hard_timeout_tests.rs`
- `idle_timeout_tests.rs`
- `heartbeat_validation.rs`
- `heartbeat_state_during_dispatch.rs`
- `sigterm_heartbeat_cleanup.rs`
- `stop_kills_process_tree.rs`
- `process_guard.rs`
- `process_discovery_integration.rs`

#### Error Handling & Edge Cases (7 files)
- `double_dispatch_prevention.rs`
- `starvation_tests.rs`
- `etxtbsy_retry.rs`
- `compilation_error_detection.rs`
- `cleanup_liveness_regression.rs`
- `github_release_upgrade_regression.rs`
- `upgrade_check_integration.rs`

#### Configuration & Fixtures (6 files)
- `config_cli_tests.rs`
- `workspace_fixtures.rs`
- `workspace_equality_tests.rs`
- `placeholder_validation_tests.rs`
- `sanitize_latency_assertion.rs`
- `property_tests.rs`

#### Integration Spawn & Miscellaneous (10 files)
- `integration_spawn.rs`
- `cli_bead_store_engine.rs`
- `mixed_backend_isolation.rs`
- `tmux_fixture.rs`
- `verify_bash_wrapper_exclusion.rs`
- `verify_bf_4390q.rs`
- `verify_deleted_binary_hot_reload.rs`
- `verify_process_discovery.rs`
- `test_mend_stale_assignee.rs`
- `test_helper_example.rs`

## Coverage Gap Summary

### ✅ What IS Covered by CI
- All 9 core strand behaviors (Pluck, Mend, Explore, Weave, Unravel, Pulse, Reflect, Splice, Knot)
- End-to-end worker lifecycle (single worker, all outcomes, exhaustion, shutdown)
- Multi-workspace scenarios
- Real bead-rs backend integration
- Unit tests across all modules

### ❌ What is NOT Covered by CI
- **Adapter system** routing and validation
- **Telemetry** field verification and OTLP transport
- **Bead store** CLI argument assertions and rehydration
- **Timeout behaviors** (idle, hard, heartbeat cleanup)
- **Error recovery** (double dispatch, starvation, ETXTBSY)
- **Configuration** loading and validation
- **Process management** edge cases

### Risk Assessment

**High-Risk Gaps (not tested in production-like environment):**
- Adapter routing logic ( Anthropic vs default routing)
- Telemetry field verification (OTLP transport)
- Timeout and heartbeat edge cases
- Double dispatch prevention
- Bead store CLI correctness

**Medium-Risk Gaps:**
- Configuration validation
- Process cleanup scenarios
- Error handling paths

**Low-Risk Gaps:**
- Helper functions and utilities
- Mock infrastructure tests

## Recommendations

### Option 1: Expand CI Coverage (High Cost)
Add selective non-strand tests to CI with individual timeouts:
```bash
run_check "adapter routing tests" timeout --kill-after=30 300 cargo test --test routing_integration
run_check "telemetry field tests" timeout --kill-after=30 300 cargo test --test telemetry_field_verification
run_check "timeout behavior tests" timeout --kill-after=30 300 cargo test --test timeout_config_integration
```

**Pros:** Catches regressions in critical paths  
**Cons:** Increases CI runtime (add 15-30 min), may need timeout tuning

### Option 2: Create Separate Validation Workflow (Medium Cost)
Create `needle-ci-validation` WorkflowTemplate for comprehensive checks:
- Runs weekly or on-demand
- Executes all 65 test targets
- Longer timeouts (no release blocking)

**Pros:** Comprehensive coverage without blocking releases  
**Cons:** Separate workflow to maintain, not run on every commit

### Option 3: Strand-Focused Coverage (Recommended)
Maintain current strand-only CI focus:
- Rationale: Strands are the core delivery mechanism
- Non-strand tests cover edge cases and infrastructure
- These can be run locally by developers working on those areas

**Pros:** Fast CI, focused on core value delivery  
**Cons:** Relies on developer discipline for edge case testing

## Conclusion

**Current state:** All 9 core NEEDLE strands have behavioral integration tests running in CI. The gap is 61 specialized test files covering routing, telemetry, error handling, and infrastructure that are not executed automatically.

**Risk level:** Medium - Core strand functionality is well-tested, but edge cases in adapters, telemetry, and error recovery are not validated in the CI environment.

**Recommended action:** Option 3 (maintain current strand-focused CI) with documented local testing practices for non-strand modules.
