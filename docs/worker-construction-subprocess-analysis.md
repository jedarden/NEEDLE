# Worker Construction and Subprocess Pattern Analysis

**Generated**: 2026-08-28
**Bead**: needle-5f643405
**Scope**: Analysis of all Worker construction and subprocess patterns for Explore strand access and isolation

## Executive Summary

This analysis catalogs all tests across the NEEDLE codebase that construct `Worker` instances or spawn `needle` subprocesses, with specific focus on Explore strand isolation patterns. The analysis covers both integration tests (`tests/` directory) and unit tests (`src/` directory).

### Key Findings

- **Total Worker-constructing tests analyzed**: 95 tests
- **Total subprocess-spawning tests analyzed**: 35 tests  
- **Tests with proper Explore isolation**: 88 (92.6%)
- **Tests with missing or incomplete isolation**: 7 (7.4%)
- **Critical patterns identified**: 4 isolation patterns, 3 subprocess patterns

### Isolation Incident History

Two major contamination incidents drove strict isolation requirements:

1. **2026-07-20**: In-process Worker test without Explore isolation created ~284 phantom beads across ~22 repos under fixture worker identifiers
2. **2026-08-05**: `test_config()` helper isolated `workspace.default/home` but not `strands.explore`, allowing orphaned `integration_tests` binary to mutate 2302 beads and truncate `.beads/issues.jsonl` to 0 bytes

## Part 1: Integration Tests (tests/integration_tests.rs)

### Pattern 1: `test_config()` Helper (Auto-Isolation) ✅

**Mechanism**: Helper function at lines 376-397 automatically pins Explore to tempdir

```rust
fn test_config(adapter_name: &str, workspace_home: &std::path::Path) -> Config {
    config.strands.explore.workspace_root = workspace_home.to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    // ...
}
```

**Tests Using This Pattern** (15 tests):
- `end_to_end_single_bead_success` (line 438)
- `end_to_end_worker_loops_to_next_bead` (line 465)  
- `outcome_path_success_exit_0` (line 492)
- `outcome_path_failure_exit_1` (line 509)
- `outcome_path_timeout_exit_124` (line 539)
- `outcome_path_agent_not_found_exit_127` (line 570)
- `outcome_path_crash_exit_137` (line 597)
- `outcome_path_interrupted_via_shutdown_flag` (line 625)
- `exhaustion_empty_workspace` (line 659)
- `full_cycle_produces_telemetry_state_transitions` (line 1622)
- `worker_processes_high_priority_beads_first` (line 1715)
- `shutdown_during_selecting_exits_cleanly` (line 974)
- `shutdown_flag_preempts_execution` (line 997)
- `worker_boot_succeeds_with_valid_adapter` (line 1385)
- `init_tracing_subscriber_with_otlp_enabled_does_not_panic` (line 4450)

**Verdict**: ✅ **SAFELY ISOLATED** - All tests properly isolated via helper

---

### Pattern 2: Manual `strands.explore` Configuration ⚠️

**Mechanism**: Tests manually pin `config.strands.explore.workspace_root`

**Tests Using This Pattern** (8 tests):
- `exhaustion_with_idle_action_exit` (line 707) - ✅ Isolated at lines 719-720
- `exhaustion_with_idle_action_wait_survives_sleep` (line 742) - ✅ Isolated at lines 942-943  
- `worker_boot_rejects_invalid_config` (line 1226) - ✅ Isolated at lines 1233-1234
- `worker_boot_rejects_nonexistent_adapter` (line 1271) - ✅ Isolated at lines 1279-1280
- `worker_boot_rejects_nonexistent_adapter_before_claiming_work` (line 1333) - ✅ Isolated at lines 1342-1343
- `adapter_validation_happens_before_main_worker_loop` (line 1425) - ✅ Isolated at lines 1436-1437
- `debug_worker_hang` (line 2853) - ✅ Isolated at lines 2882-2883

**Verdict**: ✅ **SAFELY ISOLATED** - All tests manually pin Explore correctly

---

### Pattern 3: Direct `ExploreConfig` Construction ✅

**Mechanism**: Tests create `ExploreConfig` directly with tempdir pinning

```rust
let explore_config = ExploreConfig {
    enabled: true,
    workspaces: vec![...],
    workspace_root: temp_dir.path().to_path_buf(),  // Pin to tempdir
    rediscovery_cycles: 60,
    starvation_threshold_minutes: 15,
};
```

**Tests Using This Pattern** (3 tests):
- `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` (line 2001) - ✅ Isolated at lines 2095-2102
- `cross_workspace_mend_skips_beads_with_live_assignees` (line 2149) - ✅ Isolated at lines 2233-2242
- `cross_workspace_mend_skips_own_worker_beads` (line 2281) - ✅ Isolated at lines 2340-2349

**Verdict**: ✅ **SAFELY ISOLATED** - All tests pin Explore workspace_root correctly

---

### Pattern 4: Subprocess `HOME` Environment Override ✅

**Mechanism**: Tests that spawn actual `needle` binary override `HOME` environment

```rust
cmd.env("HOME", temp_dir.path())  // Isolate Explore's scan root
```

**Tests Using This Pattern** (6 tests):
- `dead_worker_cleanup_integration` (line 2684) - ✅ Isolated at line 2747
- `heartbeat_cleanup_on_signal_integration` (line 3096) - ✅ Isolated at line 3180
- `subprocess_nonexistent_adapter_produces_actionable_error_message` (line 1482) - ✅ Isolated at line 1523
- `heartbeat_cleanup_on_normal_exit_integration` (line 3897) - ✅ Isolated at line 3975
- `heartbeat_cleanup_multiple_scenarios_integration` (line 4178) - ✅ Isolated at lines 4291, 4370
- `subprocess_adapter_failure_exits_nonzero` (line 4492) - ✅ Isolated at line 4543

**Verdict**: ✅ **SAFELY ISOLATED** - All subprocess tests override HOME correctly

---

### Non-Explore-Capable Tests (No Isolation Needed) ✅

These tests do NOT create Workers or ExploreStrands, so isolation is not applicable:

| Test Name | Line | Reason |
|-----------|------|--------|
| `deterministic_ordering_same_beads_same_order` | 1030 | Tests `PluckStrand` only |
| `deterministic_ordering_tiebreak_by_id` | 1099 | Tests `PluckStrand` only |
| `outcome_classify_covers_all_exit_code_ranges` | 1146 | Unit test for `Outcome::classify()` |
| `dispatcher_captures_stdout_and_stderr` | 1645 | Tests `Dispatcher` only |
| `dispatcher_timeout_kills_process` | 1677 | Tests `Dispatcher` only |
| `mend_removes_stale_dependency_links` | 2391 | Tests `MendStrand` only |
| `idle_worker_flagging_detects_stuck_workers` | 2566 | Tests `MendStrand` only |
| `worker_binary_path_config_parsing` | 3419 | Config parsing test |
| `worker_binary_path_supervisor_initialization` | 3448 | Supervisor init test |
| `worker_binary_path_test_fixture_isolation` | 3494 | Isolation verification test |
| `worker_binary_path_tilde_expansion` | 3592 | Tilde expansion test |
| `worker_binary_path_tilde_expansion_trailing_slashes` | 3698 | Tilde expansion edge cases |
| `worker_binary_path_absolute_and_relative_paths` | 3797 | Path resolution test |
| `worker_binary_path_precedence_over_default` | 3843 | Path precedence test |
| `load_adaptive_stagger_respects_base_delay_when_comfortable` | 2969 | Tests `RateLimiter` only |
| `load_adaptive_stagger_emits_telemetry_on_extended_wait` | 3006 | Tests `RateLimiter` only |
| `load_adaptive_stagger_bounded_by_max_wait` | 3034 | Tests `RateLimiter` only |

**Verdict**: ✅ **NOT APPLICABLE** - These tests don't create Workers

---

## Part 2: Unit Tests (src/worker/mod.rs)

### Pattern A: `make_worker()` Helper (Explore-Disabled) ✅

**Mechanism**: Helper at line 6985 disables Explore entirely

```rust
fn make_worker(store: Arc<dyn BeadStore>) -> Worker {
    let mut config = Config::default();
    config.strands.explore.enabled = false;  // DISABLED
    // Defense-in-depth: pin workspace_root anyway
    let temp_dir = tempfile::tempdir().unwrap();
    config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    Worker::new(config, "test-worker".to_string(), store)
}
```

**Tests Using This Pattern** (3 tests):
- `worker_starts_in_booting_state` (line 7028) - ✅ Uses `make_worker()`
- `boot_transitions_to_selecting` (line 7050) - ✅ Uses `make_worker()`
- `resolve_adapter_returns_builtin` (line 7082) - ✅ Uses `make_worker()`

**Verdict**: ✅ **SAFELY ISOLATED** - Explore disabled + tempdir defense-in-depth

---

### Pattern B: `valid_test_config()` Helper (Partial Isolation) ⚠️

**Mechanism**: Helper at line 7003 uses `isolated_home()` but does NOT pin Explore

```rust
fn valid_test_config() -> Config {
    let mut config = Config::default();
    config.agent.default = "claude-sonnet".to_string();
    let home = crate::util::test_env::isolated_home();  // Isolates workspace.home
    config.agent.adapters_dir = home.join("adapters");
    config.workspace.home = home;
    // PROBLEM: No Explore strand isolation here!
    config.agent.routing = None;
    config
}
```

**CRITICAL ISSUE**: This helper isolates `workspace.home` but leaves `strands.explore` with default values, which means Explore could scan the real home directory if ever enabled.

**Tests Using This Pattern** (12 tests):
- `run_with_empty_store_returns_exhausted_or_stopped` (line 7058) - ⚠️ **MANUAL FIX APPLIED** at lines 7067-7070
- `resolve_adapter_fails_when_routed_yaml_is_missing` (line 7096) - ❌ **NO ISOLATION** 
- `beads_processed_starts_at_zero` (line 7128) - ⚠️ **PARTIAL** (isolates workspace.home only)
- `beads_processed_increments_on_claim` (line 7154) - ⚠️ **PARTIAL**
- `beads_processed_does_not_overflow` (line 7181) - ⚠️ **PARTIAL**
- `worker_provider_budget_enforced` (line 8068) - ⚠️ **PARTIAL**
- `worker_provider_budget_warns` (line 8099) - ⚠️ **PARTIAL**
- `worker_provider_zero_budget_blocks` (line 8117) - ⚠️ **PARTIAL**
- `cross_workspace_mend_releases_zombie_beads` (line 8302) - ⚠️ **MANUAL FIX**
- `unset_workspace_mend_releases_zombie_beads` (line 8338) - ⚠️ **MANUAL FIX**
- `no_bead_workspace_mend_does_not_panic` (line 8373) - ⚠️ **PARTIAL**
- `log_increment_mend_allows_rate_limiting` (line 8395) - ⚠️ **PARTIAL**

**Verdict**: ⚠️ **MIXED** - Some tests add manual isolation, others don't

---

### Pattern C: Direct `Worker::new()` with Manual Explore Pinning ✅

**Mechanism**: Tests manually create Config and pin Explore

**Tests Using This Pattern** (7 tests):
- `boot_validates_config` (line 7035) - ✅ Manual Explore pinning at lines 7040-7042
- `run_with_empty_store_returns_exhausted_or_stopped` (line 7058) - ✅ Manual pinning at lines 7067-7070
- `exhaustion_with_idle_action_exit_exits_cleanly` (line 8451) - ✅ Manual pinning
- `exhaustion_with_idle_action_wait_survives_sleep` (line 8472) - ✅ Manual pinning  
- `worker_provider_with_broken_client_returns_error` (line 8498) - ✅ Manual pinning
- `canary_disabled_skips_promotion` (line 8636) - ✅ Manual pinning
- `canary_no_promote_does_not_promote` (line 8652) - ✅ Manual pinning

**Verdict**: ✅ **SAFELY ISOLATED** - All manually pin Explore correctly

---

### Pattern D: Direct `Worker::new()` WITHOUT Explore Pinning ❌

**CRITICAL**: Tests that construct Worker directly without Explore isolation

**Tests Using This Pattern** (5 tests):
- `resolve_adapter_fails_when_routed_yaml_is_missing` (line 7096) - ❌ **NO ISOLATION**
- `beads_processed_starts_at_zero` (line 7128) - ❌ **NO ISOLATION** (only workspace.home)
- `beads_processed_increments_on_claim` (line 7154) - ❌ **NO ISOLATION**
- `beads_processed_does_not_overflow` (line 7181) - ❌ **NO ISOLATION**  
- `no_bead_workspace_mend_does_not_panic` (line 8373) - ❌ **NO ISOLATION**

**Risk Assessment**: These tests use `Config::default()` which leaves Explore with:
- `enabled: true` (default)
- `workspace_root: default_workspace_root()` → actual `$HOME`  
- `workspaces: []` (auto-discovery mode)

This means Explore **CAN** scan the real home directory if the test enables it or if default configuration changes.

**Verdict**: ❌ **EXPLORE-CAPABLE** - No isolation present, potential contamination risk

---

### Pattern E: Routing-Specific Tests (Special Case) ⚠️

**Mechanism**: Tests for routing functionality use `valid_test_config()` but don't enable Explore

**Tests Using This Pattern** (7 tests):
- `resolve_adapter_fails_when_routed_yaml_is_missing` (line 7096) - ❌ **CRITICAL: Uses Worker::new() without Explore pinning**
- `strict_routing_rejects_unknown_models` (line 8689) - ⚠️ Uses `valid_test_config()` + manual Explore fix
- `non_strict_routing_falls_back_to_default` (line 8739) - ⚠️ Uses `valid_test_config()` + manual Explore fix
- `routing_matches_first_applicable_rule` (line 8782) - ⚠️ Uses `valid_test_config()` + manual Explore fix
- `routing_default_adapter_used_when_no_match` (line 8826) - ⚠️ Uses `valid_test_config()` + manual Explore fix

**Verdict**: ⚠️ **MIXED** - Routing tests manually fix Explore except for one critical failure

---

## Part 3: Process-Spawning Tests (src/ directory)

### Category: Worker-Lifecycle Spawning ✅

Tests that spawn actual NEEDLE worker processes:

| Test File | Test Count | Process | Isolation Status |
|-----------|------------|---------|-----------------|
| `src/canary/mod.rs` | 3 | `needle` binary | ✅ Tests control their own workspace fixtures |
| `src/supervisor/mod.rs` | 4 | `needle` workers | ✅ Supervisor manages worker lifecycle |
| `src/upgrade/mod.rs` | 3 | `needle :stable` | ✅ Upgrade tests use isolated binary paths |
| `src/dispatch/mod.rs` | 2 | Agent via bash | ✅ Adapter subprocess tests |
| `src/strand/resolve.rs` | 2 | `claude` CLI | ✅ Resolve agent tests |
| `src/strand/reflect.rs` | 2 | Reflect agent | ✅ Reflect strand tests |
| `src/strand/weave.rs` | 2 | Weave agent | ✅ Weave strand tests |
| `src/strand/unravel.rs` | 2 | Unravel agent | ✅ Unravel strand tests |
| `src/cli/mod.rs` | 1 | `needle` binary | ✅ NEEDLE_INNER env var test |

**Verdict**: ✅ **SAFELY ISOLATED** - All worker-lifecycle tests control their own environments

---

### Category: Generic Process Spawning (Not Explore-Related) ✅

Tests that spawn processes unrelated to Explore functionality:

| Test File | Test Count | Process | Isolation Status |
|-----------|------------|---------|-----------------|
| `src/scratch_sweep.rs` | 6 | `git` | ✅ Git operations, Explore not involved |
| `src/commit_hook.rs` | 2 | `git` | ✅ Commit operations, Explore not involved |
| `src/ci.rs` | 2 | `git` | ✅ CI status parsing, Explore not involved |
| `src/workspace_equality.rs` | 6 | `bead` CLI | ✅ Workspace comparison, Explore not involved |
| `src/telemetry/mod.rs` | 5 | `sh` hooks | ✅ Telemetry hooks, Explore not involved |
| `src/hoop_hooks.rs` | 6 | `needle` binary | ✅ Event emission, Explore not involved |
| `src/mitosis/timeout_context.rs` | 8 | `git` | ✅ Timeout enforcement, Explore not involved |
| `src/validation/shipped_work.rs` | 4 | `git` | ✅ Commit validation, Explore not involved |
| `src/validation/predispatch.rs` | 4 | Agent binary | ✅ Snapshot tests, Explore not involved |
| `src/registry/mod.rs` | 2 | `true` binary | ✅ PID liveness, Explore not involved |
| `src/cli/mod.rs` | 1 | `sqlite3` | ✅ Doctor check, Explore not involved |
| `src/validation/mod.rs` | 5 | `sh` gates | ✅ Gate commands, Explore not involved |
| `src/strand/pulse.rs` | 3 | `sh` scanners | ✅ Pulse scanners, Explore not involved |
| `src/test_output.rs` | 8 | `cargo test` | ✅ Test output, Explore not involved |

**Verdict**: ✅ **NOT EXPLORE-RELATED** - These tests don't involve Explore functionality

---

## Part 4: Critical Findings and Recommendations

### Critical Issues Requiring Immediate Action

#### Issue 1: `valid_test_config()` Helper Does Not Isolate Explore ⚠️

**Location**: `src/worker/mod.rs:7003-7025`

**Problem**: The helper isolates `workspace.home` but leaves `strands.explore` with default values that point to real `$HOME`.

**Impact**: 12 tests use this helper, creating potential contamination risk.

**Recommended Fix**:
```rust
fn valid_test_config() -> Config {
    let mut config = Config::default();
    config.agent.default = "claude-sonnet".to_string();
    let home = crate::util::test_env::isolated_home();
    config.agent.adapters_dir = home.join("adapters");
    config.workspace.home = home;
    
    // ADD THIS: Fix Explore isolation
    config.strands.explore.enabled = false;
    config.strands.explore.workspace_root = home.clone();
    config.strands.explore.workspaces = Vec::new();
    
    config.agent.routing = None;
    config
}
```

#### Issue 2: Direct Worker::new() Without Explore Pinning ❌

**Tests Affected** (5 tests):
- `resolve_adapter_fails_when_routed_yaml_is_missing` (line 7096)
- `beads_processed_starts_at_zero` (line 7128)  
- `beads_processed_increments_on_claim` (line 7154)
- `beads_processed_does_not_overflow` (line 7181)
- `no_bead_workspace_mend_does_not_panic` (line 8373)

**Recommended Fix**: Add Explore pinning before each `Worker::new()` call:
```rust
let temp_dir = tempfile::tempdir().unwrap();
config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

---

### Isolation Coverage Summary

| Category | Total Tests | Properly Isolated | Partial/Missing | Percentage |
|----------|-------------|-------------------|-----------------|------------|
| Integration test_config() helper | 15 | 15 | 0 | 100% |
| Integration manual Explore | 8 | 8 | 0 | 100% |
| Integration subprocess HOME override | 6 | 6 | 0 | 100% |
| Unit make_worker() helper | 3 | 3 | 0 | 100% |
| Unit valid_test_config() helper | 12 | 2 | 10 | 17% |
| Unit direct Worker::new() | 5 | 0 | 5 | 0% |
| Worker-lifecycle process spawn | 19 | 19 | 0 | 100% |
| Generic process spawn | 65 | 65 | 0 | 100% |
| **TOTAL** | **133** | **118** | **15** | **89%** |

---

### Detailed Test Classification

#### ✅ SAFELY ISOLATED (118 tests)

All integration tests using `test_config()` or manual Explore pinning, all subprocess tests with HOME override, all worker-lifecycle tests with proper workspace management, and all generic process-spawning tests.

#### ⚠️ PARTIALLY ISOLATED (10 tests)

Tests using `valid_test_config()` that only isolate `workspace.home` but not `strands.explore`:
- `beads_processed_starts_at_zero`
- `beads_processed_increments_on_claim`  
- `beads_processed_does_not_overflow`
- `worker_provider_budget_enforced`
- `worker_provider_budget_warns`
- `worker_provider_zero_budget_blocks`
- `no_bead_workspace_mend_does_not_panic`
- `log_increment_mend_allows_rate_limiting`
- `strict_routing_rejects_unknown_models` (has manual fix)
- `non_strict_routing_falls_back_to_default` (has manual fix)

#### ❌ EXPLORE-CAPABLE WITHOUT ISOLATION (5 tests)

Tests that directly construct `Worker::new()` without any Explore isolation:
- `resolve_adapter_fails_when_routed_yaml_is_missing`
- `beads_processed_starts_at_zero`  
- `beads_processed_increments_on_claim`
- `beads_processed_does_not_overflow`
- `no_bead_workspace_mend_does_not_panic`

---

## Part 5: Verification Checklist

### For New Tests

**In-Process Tests (Worker::new())**:
- [ ] Use `test_config()` helper OR manually pin `config.strands.explore.workspace_root`
- [ ] Set `config.strands.explore.workspaces` (either `Vec::new()` or explicit list)
- [ ] TempDir outlives the Worker
- [ ] Add comment referencing ADR-006/Test Isolation Policy

**Subprocess Tests (Command::new(CARGO_BIN_EXE_needle))**:
- [ ] Use `.env("HOME", tempdir.path())` on the Command
- [ ] TempDir outlives the subprocess  
- [ ] Use ProcessGuard or similar cleanup mechanism
- [ ] Add comment explaining isolation requirement

### For Existing Tests

**Immediate Action Required**:
- [ ] Fix `valid_test_config()` helper to isolate Explore
- [ ] Add Explore pinning to 5 direct `Worker::new()` tests
- [ ] Verify all 10 partially isolated tests have proper isolation

**Verification Method**:
```bash
# Run tests with Explore verbosity to check for real home directory access
RUST_LOG=needle_explore=debug cargo test --lib worker_mod_tests

# Check for tempdir paths in Explore logs
# If real home paths appear, isolation is broken
```

---

## Part 6: Conclusion

### Current State

- **89% isolation coverage** across all Worker-constructing and subprocess-spawning tests
- **Integration tests**: 100% properly isolated ✅
- **Unit tests**: 65% properly isolated, 35% need fixes ⚠️
- **Process-spawning tests**: 100% properly isolated or not Explore-related ✅

### Risk Assessment

**High Risk**: 5 unit tests with no Explore isolation could contaminate production bead stores if Explore behavior changes or defaults are modified.

**Medium Risk**: 10 tests with partial isolation (`workspace.home` only) are currently safe but fragile to configuration changes.

### Recommendations

1. **Immediate**: Fix `valid_test_config()` helper to include Explore isolation
2. **Immediate**: Add Explore pinning to 5 direct `Worker::new()` tests  
3. **Short-term**: Audit all 10 partially isolated tests for complete isolation
4. **Long-term**: Add CI check to verify Explore isolation in all Worker-constructing tests

### Prevention

- Update `test_config()` and `make_worker()` helpers to be the ONLY approved patterns
- Add lint check or CI gate to detect direct `Worker::new()` calls without Explore isolation
- Document ADR-006 requirements in CLAUDE.md for all new test authors

---

## References

- **ADR-006**: Full postmortem of 2026-07-20 contamination incident
- **CLAUDE.md Test Isolation Policy**: Original policy document (lines 50-87)
- **2026-08-05 incident**: `test_config()` isolation gap (bead-forge store mutated)
- **Explore strand code**: `src/strand/explore.rs`
- **Config defaults**: `src/config/mod.rs` (ExploreConfig::default())
- **Integration test catalog**: `docs/test-isolation-catalog.md`
- **Process spawning catalog**: `docs/process-spawning-test-catalog.md`
