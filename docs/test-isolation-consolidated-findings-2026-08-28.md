# Test Isolation Requirements - Consolidated Findings

**Generated:** 2026-08-28
**Bead:** needle-1ec2b938
**Source Analyses:**
- Config analysis: needle-4beeff65 (docs/tilde-expansion-config-fields.md)
- Worker analysis: needle-5f643405 (docs/worker-construction-subprocess-analysis.md)

## Executive Summary

This report consolidates findings from two comprehensive analyses of NEEDLE's test isolation requirements:

1. **Config Analysis:** Surveyed all 18 config path fields requiring tilde expansion
2. **Worker Analysis:** Audited 133 tests that construct Workers or spawn subprocesses

### Overall Statistics

| Category | Total Tests | Properly Isolated | Need Fixes | Coverage |
|----------|-------------|-------------------|------------|----------|
| **Integration tests** | 32 | 32 | 0 | 100% ✅ |
| **Unit tests (Worker)** | 27 | 17 | 10 | 63% ⚠️ |
| **Process-spawning** | 35 | 35 | 0 | 100% ✅ |
| **Config field coverage** | 18 fields | 16 | 2 | 89% ⚠️ |
| **TOTAL** | **112** | **100** | **12** | **89%** |

### Critical Findings

**IMMEDIATE ACTION REQUIRED:**
1. **15 tests** with missing or incomplete Explore strand isolation
2. **2 config fields** missing tilde expansion test coverage
3. **1 helper function** (`valid_test_config()`) that leaves Explore unisolated

### Incident Context

Two major contamination incidents established strict isolation requirements:

1. **2026-07-20:** In-process Worker test without Explore isolation created ~284 phantom beads across ~22 repos
2. **2026-08-05:** `test_config()` isolated `workspace.default/home` but not `strands.explore`, causing 2302 bead mutations and `.beads/issues.jsonl` truncation

---

## Part 1: Tests Requiring Isolation Fixes

### Category A: Direct Worker::new() Without Explore Pinning ❌

**Risk:** These tests use `Config::default()` which leaves Explore enabled with:
- `enabled: true` (default)
- `workspace_root: default_workspace_root()` → actual `$HOME`
- `workspaces: []` (auto-discovery mode)

**Impact:** 5 tests can scan and mutate real bead stores

#### Test Details

| Test Name | File | Line | Pattern | Required Fix |
|-----------|------|------|---------|--------------|
| `resolve_adapter_fails_when_routed_yaml_is_missing` | src/worker/mod.rs | 7096 | Direct `Worker::new()` | Add Explore pinning |
| `beads_processed_starts_at_zero` | src/worker/mod.rs | 7128 | Direct `Worker::new()` | Add Explore pinning |
| `beads_processed_increments_on_claim` | src/worker/mod.rs | 7154 | Direct `Worker::new()` | Add Explore pinning |
| `beads_processed_does_not_overflow` | src/worker/mod.rs | 7181 | Direct `Worker::new()` | Add Explore pinning |
| `no_bead_workspace_mend_does_not_panic` | src/worker/mod.rs | 8373 | Direct `Worker::new()` | Add Explore pinning |

#### Required Code Changes

**Before each `Worker::new()` call, add:**

```rust
let temp_dir = tempfile::tempdir().unwrap();
config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
config.strands.explore.workspaces = Vec::new();
// Ensure temp_dir outlives the Worker
```

**Example fix for `beads_processed_starts_at_zero`:**

```rust
#[test]
fn beads_processed_starts_at_zero() {
    let store = Arc::new(new_store());
    let mut config = Config::default();
    config.agent.default = "test-adapter".to_string();

    // ADD THIS: Explore isolation
    let temp_dir = tempfile::tempdir().unwrap();
    config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();

    let worker = Worker::new(config, "test-worker".to_string(), store);
    // ... rest of test

    // Keep temp_dir alive
    drop(temp_dir);
}
```

---

### Category B: Tests Using `valid_test_config()` Helper ⚠️

**Risk:** The `valid_test_config()` helper (src/worker/mod.rs:7003-7025) isolates `workspace.home` but leaves `strands.explore` with default values pointing to real `$HOME`.

**Impact:** 10 tests use this helper without adding manual Explore fixes

#### Test Details

| Test Name | File | Line | Status | Required Action |
|-----------|------|------|--------|-----------------|
| `worker_provider_budget_enforced` | src/worker/mod.rs | 8068 | Partial | Add manual Explore pinning |
| `worker_provider_budget_warns` | src/worker/mod.rs | 8099 | Partial | Add manual Explore pinning |
| `worker_provider_zero_budget_blocks` | src/worker/mod.rs | 8117 | Partial | Add manual Explore pinning |
| `beads_processed_starts_at_zero` | src/worker/mod.rs | 7128 | Duplicate | Also in Category A |
| `beads_processed_increments_on_claim` | src/worker/mod.rs | 7154 | Partial | Add manual Explore pinning |
| `beads_processed_does_not_overflow` | src/worker/mod.rs | 7181 | Partial | Add manual Explore pinning |
| `no_bead_workspace_mend_does_not_panic` | src/worker/mod.rs | 8373 | Duplicate | Also in Category A |
| `log_increment_mend_allows_rate_limiting` | src/worker/mod.rs | 8395 | Partial | Add manual Explore pinning |
| `strict_routing_rejects_unknown_models` | src/worker/mod.rs | 8689 | Has manual fix | Verify fix is correct |
| `non_strict_routing_falls_back_to_default` | src/worker/mod.rs | 8739 | Has manual fix | Verify fix is correct |

#### Recommended Fix: Update the Helper

**Location:** `src/worker/mod.rs:7003-7025`

**Current code:**
```rust
fn valid_test_config() -> Config {
    let mut config = Config::default();
    config.agent.default = "claude-sonnet".to_string();
    let home = crate::util::test_env::isolated_home();
    config.agent.adapters_dir = home.join("adapters");
    config.workspace.home = home;
    // PROBLEM: No Explore strand isolation here!
    config.agent.routing = None;
    config
}
```

**Fixed code:**
```rust
fn valid_test_config() -> Config {
    let mut config = Config::default();
    config.agent.default = "claude-sonnet".to_string();
    let home = crate::util::test_env::isolated_home();
    config.agent.adapters_dir = home.join("adapters");
    config.workspace.home = home;

    // FIX: Add Explore strand isolation
    config.strands.explore.enabled = false;
    config.strands.explore.workspace_root = home.clone();
    config.strands.explore.workspaces = Vec::new();

    config.agent.routing = None;
    config
}
```

**Alternative:** For each test using this helper, add manual Explore pinning after calling it:

```rust
let mut config = valid_test_config();
let temp_dir = tempfile::tempdir().unwrap();
config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

---

### Category C: Subprocess Tests ✅ (All Properly Isolated)

**All 35 subprocess-spawning tests** properly override `HOME` environment variable:

**Required Pattern:**
```rust
cmd.env("HOME", temp_dir.path())  // Isolate Explore's scan root
```

**Tests using this pattern** (6 integration tests):
- `dead_worker_cleanup_integration` (line 2747)
- `heartbeat_cleanup_on_signal_integration` (line 3180)
- `subprocess_nonexistent_adapter_produces_actionable_error_message` (line 1523)
- `heartbeat_cleanup_on_normal_exit_integration` (line 3975)
- `heartbeat_cleanup_multiple_scenarios_integration` (lines 4291, 4370)
- `subprocess_adapter_failure_exits_nonzero` (line 4543)

**Status:** ✅ NO FIXES NEEDED - All properly isolated

---

## Part 2: Config Fields Missing Tilde Expansion Coverage

### Fields WITH Test Coverage (16/18 = 89%)

| Field | Type | Test Coverage | Test Location |
|-------|------|---------------|----------------|
| `workspace.default` | PathBuf | ✅ Yes | src/config/mod.rs:11552 |
| `workspace.home` | PathBuf | ✅ Yes | src/config/mod.rs:11552 |
| `worker.worker_binary_path` | Option<PathBuf> | ✅ Yes | src/config/mod.rs:11698 |
| `agent.adapters_dir` | PathBuf | ✅ Yes | src/config/mod.rs:11552 |
| `bead_cli.path` | Option<PathBuf> | ✅ Yes | src/config/mod.rs:11682 |
| `strands.explore.workspace_root` | PathBuf | ✅ Yes | src/config/mod.rs:11578 |
| `strands.explore.workspaces` | Vec<PathBuf> | ✅ Yes | src/config/mod.rs:11578 |
| `strands.weave.exclude_workspaces` | Vec<PathBuf> | ✅ Yes | src/config/mod.rs:11578 |
| `strands.splice.report_workspace` | Option<PathBuf> | ✅ Yes | src/config/mod.rs:11578 |
| `strands.learning.global_learnings_file` | PathBuf | ✅ Yes | src/config/mod.rs:11613 |
| `telemetry.file_sink.log_dir` | Option<PathBuf> | ✅ Yes | src/config/mod.rs:11738 |
| `health.heartbeat_dir` | Option<PathBuf> | ✅ Yes | src/config/mod.rs:11613 |
| `supervisor.heartbeat_path` | Option<PathBuf> | ✅ Yes | src/config/mod.rs:11613 |
| `supervisor.socket_path` | Option<PathBuf> | ✅ Yes | src/config/mod.rs:11613 |
| `prompt.context_files` | Vec<PathBuf> | ✅ Yes | src/config/mod.rs:11714 |
| `self_modification.canary_workspace` | PathBuf | ✅ Yes | src/config/mod.rs:11754 |

### Fields MISSING Test Coverage (2/18 = 11%)

| Field | Type | Why Missing | Required Test |
|-------|------|-------------|----------------|
| `post_push_ci.state_dir` | Option<PathBuf> | No unit test exists | `test_config_expand_tildes_post_push_ci_state_dir()` |
| `prompt.variants[].content_file` | PathBuf | No unit test exists | `test_config_expand_tildes_prompt_variants_content_file()` |

#### Required Test: post_push_ci.state_dir

**Location:** `src/config/mod.rs` (after line 11738)

```rust
#[test]
fn test_config_expand_tildes_post_push_ci_state_dir() {
    let home = isolate_env();
    let mut config = Config::default();
    config.post_push_ci.state_dir = Some(PathBuf::from("~/state"));
    config.expand_tildes(&home);

    assert_eq!(
        config.post_push_ci.state_dir,
        Some(home.join("state"))
    );
}
```

#### Required Test: prompt.variants[].content_file

**Location:** `src/config/mod.rs` (after line 11754)

```rust
#[test]
fn test_config_expand_tildes_prompt_variants_content_file() {
    let home = isolate_env();
    let mut config = Config::default();

    // Add a variant with tilde path
    let variant_name = "test_variant".to_string();
    let variant = PromptVariant {
        content_file: Some(PathBuf::from("~/prompts/test.md")),
        content: None,
    };
    config.prompt.variants.insert(variant_name, variant);

    config.expand_tildes(&home);

    // Verify expansion
    let expanded = config.prompt.variants.get("test_variant").unwrap();
    assert_eq!(
        expanded.content_file,
        Some(home.join("prompts/test.md"))
    );
}
```

---

## Part 3: Integration Tests - All Properly Isolated ✅

### Pattern 1: test_config() Helper (15 tests)

**Location:** `tests/integration_tests.rs:376-397`

**Mechanism:** Helper automatically pins Explore to tempdir

**Tests using this pattern:**
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

**Status:** ✅ ALL PROPERLY ISOLATED - NO FIXES NEEDED

### Pattern 2: Manual Explore Pinning (8 tests)

**Tests using this pattern:**
- `exhaustion_with_idle_action_exit` (line 707) - ✅ Isolated at lines 719-720
- `exhaustion_with_idle_action_wait_survives_sleep` (line 742) - ✅ Isolated at lines 942-943
- `worker_boot_rejects_invalid_config` (line 1226) - ✅ Isolated at lines 1233-1234
- `worker_boot_rejects_nonexistent_adapter` (line 1271) - ✅ Isolated at lines 1279-1280
- `worker_boot_rejects_nonexistent_adapter_before_claiming_work` (line 1333) - ✅ Isolated at lines 1342-1343
- `adapter_validation_happens_before_main_worker_loop` (line 1425) - ✅ Isolated at lines 1436-1437
- `debug_worker_hang` (line 2853) - ✅ Isolated at lines 2882-2883

**Status:** ✅ ALL PROPERLY ISOLATED - NO FIXES NEEDED

### Pattern 3: Direct ExploreConfig Construction (3 tests)

**Tests using this pattern:**
- `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` (line 2001) - ✅ Isolated
- `cross_workspace_mend_skips_beads_with_live_assignees` (line 2149) - ✅ Isolated
- `cross_workspace_mend_skips_own_worker_beads` (line 2281) - ✅ Isolated

**Status:** ✅ ALL PROPERLY ISOLATED - NO FIXES NEEDED

### Non-Explore-Capable Tests (No Isolation Needed)

These tests don't create Workers or ExploreStrands:

| Test Name | Line | Reason |
|-----------|------|--------|
| `deterministic_ordering_same_beads_same_order` | 1030 | Tests PluckStrand only |
| `deterministic_ordering_tiebreak_by_id` | 1099 | Tests PluckStrand only |
| `outcome_classify_covers_all_exit_code_ranges` | 1146 | Unit test for Outcome::classify() |
| `dispatcher_captures_stdout_and_stderr` | 1645 | Tests Dispatcher only |
| `dispatcher_timeout_kills_process` | 1677 | Tests Dispatcher only |
| `mend_removes_stale_dependency_links` | 2391 | Tests MendStrand only |
| `idle_worker_flagging_detects_stuck_workers` | 2566 | Tests MendStrand only |
| `worker_binary_path_config_parsing` | 3419 | Config parsing test |
| `worker_binary_path_supervisor_initialization` | 3448 | Supervisor init test |
| `worker_binary_path_test_fixture_isolation` | 3494 | Isolation verification test |
| `worker_binary_path_tilde_expansion` | 3592 | Tilde expansion test |
| `worker_binary_path_tilde_expansion_trailing_slashes` | 3698 | Tilde expansion edge cases |
| `worker_binary_path_absolute_and_relative_paths` | 3797 | Path resolution test |
| `worker_binary_path_precedence_over_default` | 3843 | Path precedence test |
| `load_adaptive_stagger_respects_base_delay_when_comfortable` | 2969 | Tests RateLimiter only |
| `load_adaptive_stagger_emits_telemetry_on_extended_wait` | 3006 | Tests RateLimiter only |
| `load_adaptive_stagger_bounded_by_max_wait` | 3034 | Tests RateLimiter only |

**Status:** ✅ NOT APPLICABLE - These tests don't create Workers

---

## Part 4: Unit Tests - Mixed Isolation Status

### Pattern A: make_worker() Helper (3 tests) ✅

**Location:** `src/worker/mod.rs:6985`

**Mechanism:** Helper disables Explore entirely

**Tests using this pattern:**
- `worker_starts_in_booting_state` (line 7028) - ✅ Uses `make_worker()`
- `boot_transitions_to_selecting` (line 7050) - ✅ Uses `make_worker()`
- `resolve_adapter_returns_builtin` (line 7082) - ✅ Uses `make_worker()`

**Status:** ✅ ALL PROPERLY ISOLATED - NO FIXES NEEDED

### Pattern C: Direct Worker::new() with Manual Pinning (7 tests) ✅

**Tests using this pattern:**
- `boot_validates_config` (line 7035) - ✅ Manual Explore pinning at lines 7040-7042
- `run_with_empty_store_returns_exhausted_or_stopped` (line 7058) - ✅ Manual pinning at lines 7067-7070
- `exhaustion_with_idle_action_exit_exits_cleanly` (line 8451) - ✅ Manual pinning
- `exhaustion_with_idle_action_wait_survives_sleep` (line 8472) - ✅ Manual pinning
- `worker_provider_with_broken_client_returns_error` (line 8498) - ✅ Manual pinning
- `canary_disabled_skips_promotion` (line 8636) - ✅ Manual pinning
- `canary_no_promote_does_not_promote` (line 8652) - ✅ Manual pinning

**Status:** ✅ ALL PROPERLY ISOLATED - NO FIXES NEEDED

---

## Part 5: Implementation Recommendations

### Immediate Actions (Priority 1)

1. **Fix `valid_test_config()` helper** (src/worker/mod.rs:7003-7025)
   - Add Explore isolation to the helper itself
   - This fixes 10 tests at once

2. **Add Explore pinning to 5 direct Worker::new() tests**
   - `resolve_adapter_fails_when_routed_yaml_is_missing` (line 7096)
   - `beads_processed_starts_at_zero` (line 7128)
   - `beads_processed_increments_on_claim` (line 7154)
   - `beads_processed_does_not_overflow` (line 7181)
   - `no_bead_workspace_mend_does_not_panic` (line 8373)

3. **Add 2 missing tilde expansion tests**
   - `test_config_expand_tildes_post_push_ci_state_dir()`
   - `test_config_expand_tildes_prompt_variants_content_file()`

### Short-term Actions (Priority 2)

4. **Verify routing tests with manual Explore fixes**
   - `strict_routing_rejects_unknown_models` (line 8689)
   - `non_strict_routing_falls_back_to_default` (line 8739)
   - `routing_matches_first_applicable_rule` (line 8782)
   - `routing_default_adapter_used_when_no_match` (line 8826)

5. **Add CI verification**
   - Check for `Worker::new()` calls without Explore isolation
   - Verify all tests use approved patterns

### Long-term Actions (Priority 3)

6. **Standardize on approved patterns**
   - Integration tests: Use `test_config()` helper
   - Unit tests: Use `make_worker()` helper or add Explore pinning
   - Subprocess tests: Override `HOME` environment

7. **Add lint rule**
   - Detect direct `Worker::new()` without Explore isolation
   - Fail CI on violation

8. **Documentation**
   - Update CLAUDE.md Test Isolation Policy with these patterns
   - Add ADR-019 documenting this consolidation

---

## Part 6: Verification Checklist

### For Test Authors

**Before writing a new test that constructs a Worker:**

- [ ] Decide: In-process (Worker::new()) or subprocess (Command::new())
- [ ] In-process: Use `test_config()` helper OR manually pin Explore
- [ ] Subprocess: Set `cmd.env("HOME", tempdir.path())`
- [ ] Ensure TempDir outlives the Worker or subprocess
- [ ] Add comment referencing ADR-006/Test Isolation Policy

**Before committing code changes:**

- [ ] Run tests with `RUST_LOG=needle_explore=debug` to check for real home access
- [ ] Verify no `$HOME` or `~/` paths in Explore logs (only tempdir paths should appear)
- [ ] Check that `.beads/` directories only appear in tempdir locations

### For Reviewers

- [ ] Check that all Worker-constructing tests isolate Explore
- [ ] Verify subprocess tests override HOME environment
- [ ] Ensure new config fields have tilde expansion tests
- [ ] Reject PRs that bypass isolation requirements

---

## Part 7: Summary Statistics

### Test Isolation Coverage

| Category | Total | Isolated | Need Fixes | % |
|----------|-------|----------|------------|---|
| Integration (test_config helper) | 15 | 15 | 0 | 100% ✅ |
| Integration (manual Explore) | 8 | 8 | 0 | 100% ✅ |
| Integration (subprocess HOME) | 6 | 6 | 0 | 100% ✅ |
| Unit (make_worker helper) | 3 | 3 | 0 | 100% ✅ |
| Unit (valid_test_config helper) | 12 | 2 | 10 | 17% ❌ |
| Unit (direct Worker::new) | 5 | 0 | 5 | 0% ❌ |
| Worker-lifecycle processes | 19 | 19 | 0 | 100% ✅ |
| Generic processes | 65 | 65 | 0 | 100% ✅ |
| **TOTAL** | **133** | **118** | **15** | **89%** |

### Config Field Coverage

| Category | Total | Covered | Missing | % |
|----------|-------|---------|---------|---|
| PathBuf fields | 10 | 9 | 1 | 90% |
| Option<PathBuf> fields | 6 | 4 | 2 | 67% |
| Vec<PathBuf> fields | 2 | 2 | 0 | 100% |
| **TOTAL** | **18** | **16** | **2** | **89%** |

### Risk Assessment

| Risk Level | Description | Count | Action Required |
|------------|-------------|-------|-----------------|
| **HIGH** | No Explore isolation, can mutate real bead stores | 5 | Immediate fixes |
| **MEDIUM** | Partial isolation, fragile to config changes | 10 | Helper fixes |
| **LOW** | Missing tilde expansion tests (coverage gap) | 2 | Add tests |

---

## References

### Documentation
- `docs/testing-isolation-patterns.md` - Comprehensive isolation patterns
- `docs/tilde-expansion-config-fields.md` - Config field analysis (source)
- `docs/worker-construction-subprocess-analysis.md` - Worker analysis (source)
- `CLAUDE.md` (lines 50-87) - Original Test Isolation Policy

### Historical Incidents
- **ADR-006:** 2026-07-20 contamination incident (~284 phantom beads)
- **2026-08-05 incident:** `test_config()` isolation gap (2302 bead mutations)

### Code Locations
- `src/config/mod.rs:7003-7025` - `valid_test_config()` helper (needs fix)
- `src/config/mod.rs:6985` - `make_worker()` helper (already safe)
- `tests/integration_tests.rs:376-397` - `test_config()` helper (already safe)
- `src/util/test_env.rs` - `isolated_home()` and `isolate_env()` helpers

### Test Files Requiring Changes
1. `src/worker/mod.rs` - Fix helper + 5 direct tests
2. `src/config/mod.rs` - Add 2 tilde expansion tests

---

## Appendix A: Complete Fix Listing

### File: src/worker/mod.rs

**Fix 1: Update `valid_test_config()` helper (line 7003)**

```rust
fn valid_test_config() -> Config {
    let mut config = Config::default();
    config.agent.default = "claude-sonnet".to_string();
    let home = crate::util::test_env::isolated_home();
    config.agent.adapters_dir = home.join("adapters");
    config.workspace.home = home;

    // ADD: Explore strand isolation
    config.strands.explore.enabled = false;
    config.strands.explore.workspace_root = home.clone();
    config.strands.explore.workspaces = Vec::new();

    config.agent.routing = None;
    config
}
```

**Fix 2-6: Add Explore pinning before Worker::new() calls**

For each of these 5 tests, add before the `Worker::new()` call:

```rust
let temp_dir = tempfile::tempdir().unwrap();
config.strands.explore.workspace_root = temp_dir.path().to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

Tests affected:
- Line 7096: `resolve_adapter_fails_when_routed_yaml_is_missing`
- Line 7128: `beads_processed_starts_at_zero`
- Line 7154: `beads_processed_increments_on_claim`
- Line 7181: `beads_processed_does_not_overflow`
- Line 8373: `no_bead_workspace_mend_does_not_panic`

### File: src/config/mod.rs

**Fix 7: Add `test_config_expand_tildes_post_push_ci_state_dir()`**

```rust
#[test]
fn test_config_expand_tildes_post_push_ci_state_dir() {
    let home = isolate_env();
    let mut config = Config::default();
    config.post_push_ci.state_dir = Some(PathBuf::from("~/state"));
    config.expand_tildes(&home);

    assert_eq!(
        config.post_push_ci.state_dir,
        Some(home.join("state"))
    );
}
```

**Fix 8: Add `test_config_expand_tildes_prompt_variants_content_file()`**

```rust
#[test]
fn test_config_expand_tildes_prompt_variants_content_file() {
    let home = isolate_env();
    let mut config = Config::default();

    let variant_name = "test_variant".to_string();
    let variant = PromptVariant {
        content_file: Some(PathBuf::from("~/prompts/test.md")),
        content: None,
    };
    config.prompt.variants.insert(variant_name, variant);

    config.expand_tildes(&home);

    let expanded = config.prompt.variants.get("test_variant").unwrap();
    assert_eq!(
        expanded.content_file,
        Some(home.join("prompts/test.md"))
    );
}
```

---

## Appendix B: Scope for Follow-up Implementation Bead

### Bead Title
"Implement test isolation fixes for Explore strand and config fields"

### Acceptance Criteria

1. Update `valid_test_config()` helper to isolate Explore
2. Add Explore pinning to 5 direct `Worker::new()` tests
3. Add 2 missing tilde expansion tests
4. All tests pass with `cargo test`
5. CI passes on push to main
6. No beads created or modified during test runs (verify with bead store inspection)

### Verification Steps

1. Run tests with Explore logging: `RUST_LOG=needle_explore=debug cargo test`
2. Verify no real home directory paths in Explore logs
3. Run full test suite: `cargo test --all-targets`
4. Check that all 15 previously-unsafe tests now pass
5. Verify new config field tests pass

### Estimated Effort

- Helper update: 15 minutes
- 5 direct test fixes: 30 minutes
- 2 new config tests: 20 minutes
- Verification: 30 minutes
- **Total: ~1.5 hours**

---

**END OF REPORT**
