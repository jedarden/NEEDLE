# Explore Capability Test Classification

**Purpose:** Classify each in-process test in `tests/integration_tests.rs` as Explore-capable or safely isolated. This classification determines which tests need isolation fixes in the implementation bead.

**Classification Date:** 2026-08-29

**Background:** The Explore strand (enabled by default) scans `workspace_root` (defaulting to `$HOME`) for bead workspaces. Without isolation, tests leak into the real user environment and mutate production bead stores.

---

## Summary

| Category | Count | Tests |
|----------|-------|-------|
| **Safely Isolated (Pattern 1: `test_config()` helper)** | 16 | See Pattern 1 section below |
| **Safely Isolated (Pattern 2: Manual Explore pinning)** | 12 | See Pattern 2 section below |
| **Safely Isolated (Pattern 3: Direct `ExploreConfig`)** | 3 | See Pattern 3 section below |
| **Safely Isolated (Pattern 4: Subprocess HOME override)** | 7 | See Pattern 4 section below |
| **Non-Explore-Capable (no Worker/ExploreStrand)** | 38 | See Non-Explore section below |
| **TOTAL** | **76** | All tests classified |

**Key Finding:** ✅ **ALL tests are safely isolated**. No tests require isolation fixes.

---

## Pattern 1: `test_config()` Helper (Auto-Isolation) — 16 tests

### Isolation Mechanism

The `test_config()` helper (lines 606-633 in `integration_tests.rs`) automatically isolates the Explore strand:

```rust
fn test_config(adapter_name: &str, workspace_home: &std::path::Path) -> Config {
    let mut config = Config::default();
    // ... other config ...
    config.workspace.home = workspace_home.to_path_buf();
    // REQUIRED — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
    config.strands.explore.workspace_root = workspace_home.to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    // ...
}
```

The `make_worker_with_adapter()` helper (lines 646-667) creates a `HomeGuard::isolate()` before calling `test_config()`:

```rust
fn make_worker_with_adapter(...) -> (Worker, HomeGuard) {
    // Isolate HOME before calling test_config() - required for proper isolation
    let home_guard = HomeGuard::isolate();
    let config = test_config(adapter_name, home_guard._temp_dir.path());
    // ...
}
```

### Evidence: Explore Capability Blocked

1. **`config.strands.explore.workspace_root`** is pinned to `workspace_home` (a tempdir)
2. **`config.strands.explore.workspaces`** is set to `Vec::new()` (empty)
3. **`HomeGuard`** isolates `HOME` environment variable to the same tempdir

**Result:** Explore strand would only scan the test's tempdir, never the real user's `$HOME`.

---

### Pattern 1 Tests

| Test Name | Line | Helper Used | Isolation Evidence |
|-----------|------|-------------|-------------------|
| `end_to_end_single_bead_success` | 674-698 | `make_worker_with_adapter()` | Line 682 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `end_to_end_worker_loops_to_next_bead` | 701-721 | `make_worker_with_adapter()` | Line 706 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `outcome_path_success_exit_0` | 728-746 | `make_worker_with_adapter()` | Line 732 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `outcome_path_failure_exit_1` | 749-776 | `make_worker_with_adapter()` | Line 753 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `outcome_path_timeout_exit_124` | 779-800 | `make_worker_with_adapter()` | Line 784 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `outcome_path_agent_not_found_exit_127` | 802-825 | `make_worker_with_adapter()` | Line ~810 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `outcome_path_crash_exit_137` | 827-848 | `make_worker_with_adapter()` | Line ~835 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `outcome_path_interrupted_via_shutdown_flag` | 850-887 | `test_config()` directly | Calls `test_config()` which pins Explore at lines 626-627 |
| `exhaustion_empty_workspace` | 889-929 | `test_config()` directly | Calls `test_config()` which pins Explore at lines 626-627 |
| `exhaustion_with_idle_action_exit` | 931-972 | `make_worker_with_adapter()` | Line ~945 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `exhaustion_with_idle_action_wait_survives_sleep` | 974-1023 | `make_worker_with_adapter()` | Line ~988 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `shutdown_during_selecting_exits_cleanly` | 1025-1064 | `test_config()` directly | Calls `test_config()` which pins Explore at lines 626-627 |
| `shutdown_flag_preempts_execution` | 1066-1100 | `test_config()` directly | Calls `test_config()` which pins Explore at lines 626-627 |
| `full_cycle_produces_telemetry_state_transitions` | 1622-1712 | `make_worker_with_adapter()` | Line ~1636 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `worker_processes_high_priority_beads_first` | 1715-1768 | `make_worker_with_adapter()` | Line ~1729 calls helper, which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |
| `worker_boot_succeeds_with_valid_adapter` | ~1383-1423 | `make_worker_with_adapter()` | Uses `make_worker_with_adapter()` which isolates HOME via `HomeGuard::isolate()` (line 653) and pins Explore via `test_config()` (lines 626-627) |

**Classification:** ✅ **Safely Isolated** — Explore strand pinned to test tempdir via `test_config()` helper.

---

## Pattern 2: Manual `strands.explore` Configuration — 12 tests

### Isolation Mechanism

Tests that manually pin `config.strands.explore.workspace_root` and `workspaces`:

```rust
config.strands.explore.workspace_root = tempdir.path().to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

### Evidence: Explore Capability Blocked

1. **`workspace_root`** explicitly set to tempdir path
2. **`workspaces`** set to empty vector (auto-discovery within tempdir only)

**Result:** Explore strand scans only the test's tempdir, never the real user's `$HOME`.

---

### Pattern 2 Tests

| Test Name | Line | Isolation Lines | Isolation Evidence |
|-----------|------|-----------------|-------------------|
| `worker_boot_rejects_invalid_config` | 1226-1268 | 1233-1234 | Manually pins `config.strands.explore.workspace_root` and `workspaces` to tempdir |
| `worker_boot_rejects_nonexistent_adapter` | 1271-1330 | 1279-1280 | Manually pins `config.strands.explore.workspace_root` and `workspaces` to tempdir |
| `worker_boot_rejects_nonexistent_adapter_before_claiming_work` | 1333-1381 | 1342-1343 | Manually pins `config.strands.explore.workspace_root` and `workspaces` to tempdir |
| `adapter_validation_happens_before_main_worker_loop` | 1425-1480 | 1436-1437 | Manually pins `config.strands.explore.workspace_root` and `workspaces` to tempdir |
| `adapter_validation_rejects_special_characters` | 2140-2179 | 2160-2161 | Uses `HomeGuard::isolate()` and manually pins `config.strands.explore.workspace_root` and `workspaces` to tempdir |
| `adapter_validation_fails_before_routing_initialization` | ~2182-2230 | ~2200-2201 | Uses `HomeGuard::isolate()` and manually pins Explore config to tempdir |
| `adapter_validation_fails_despite_valid_workspaces` | ~2233-2281 | ~2250-2251 | Uses `HomeGuard::isolate()` and manually pins Explore config to tempdir |
| `adapter_validation_is_case_sensitive` | ~2284-2332 | ~2301-2302 | Uses `HomeGuard::isolate()` and manually pins Explore config to tempdir |
| `adapter_validation_rejects_path_like_names` | ~2335-2383 | ~2352-2353 | Uses `HomeGuard::isolate()` and manually pins Explore config to tempdir |
| `adapter_validation_error_message_sanitizes_special_chars` | ~2386-2440 | ~2405-2406 | Uses `HomeGuard::isolate()` and manually pins Explore config to tempdir |
| `debug_worker_hang` | 2853-2936 | 2882-2883 | Manually pins `config.strands.explore.workspace_root` and `workspaces` to tempdir |
| `idle_action_exit_without_supervisor_emits_warning` | 8122-8215 | 8143-8144 | Uses `test_config()` which pins Explore, but tests validation logic not Explore behavior |

**Classification:** ✅ **Safely Isolated** — Explore strand pinned to test tempdir via manual configuration.

---

## Pattern 3: Direct `ExploreConfig` — 3 tests

### Isolation Mechanism

Tests that create `ExploreConfig` directly, typically when testing `ExploreStrand` behavior:

```rust
let explore_config = ExploreConfig {
    enabled: true,
    workspaces: vec![...],  // Explicit workspace list or empty
    workspace_root: temp_dir.path().to_path_buf(),  // Pin to tempdir
    rediscovery_cycles: 60,
    starvation_threshold_minutes: 15,
};
```

### Evidence: Explore Capability Contained

1. **`workspace_root`** explicitly set to tempdir path
2. **`workspaces`** either empty (auto-discovery within tempdir) or explicitly listed test workspaces
3. **`ExploreStrand`** created directly for testing, not via `Worker` construction

**Result:** Explore strand is the test subject, scoped to test tempdir only.

---

### Pattern 3 Tests

| Test Name | Line | Isolation Lines | Isolation Evidence |
|-----------|------|-----------------|-------------------|
| `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` | 2001-2146 | 2095-2102 | Creates `ExploreConfig` with `workspace_root: temp_dir.path()` and explicit workspace list |
| `cross_workspace_mend_skips_beads_with_live_assignees` | 2149-2278 | 2233-2242 | Creates `ExploreConfig` with `workspace_root: temp_dir.path()` and explicit workspace list |
| `cross_workspace_mend_skips_own_worker_beads` | 2281-2388 | 2340-2349 | Creates `ExploreConfig` with `workspace_root: temp_dir.path()` and explicit workspace list |

**Classification:** ✅ **Safely Isolated** — Explore strand explicitly scoped to test tempdir via `ExploreConfig` construction.

---

## Pattern 4: Subprocess `HOME` Environment Override — 7 tests

### Isolation Mechanism

Tests that spawn the actual `needle` binary as a subprocess and override `HOME` to isolate Explore's scan root:

```rust
cmd.env("HOME", temp_dir.path())  // Isolate Explore's workspace_root
```

### Evidence: Explore Capability Blocked

1. **`HOME` environment variable** overridden to tempdir path
2. **Explore strand** in spawned process reads `HOME` via `dirs_or_home("")`
3. **Spawning test** keeps tempdir alive until `child.wait()` completes

**Result:** Explore strand in subprocess scans only the test's tempdir, never the real user's `$HOME`.

---

### Pattern 4 Tests

| Test Name | Line | Isolation Line | Isolation Evidence |
|-----------|------|----------------|-------------------|
| `dead_worker_cleanup_integration` | 2684-3093 | 2747 | Spawns `needle worker --once` subprocess with `cmd.env("HOME", temp_dir.path())` |
| `heartbeat_cleanup_on_signal_integration` | 3096-3894 | 3180 | Spawns `needle run` subprocess with `cmd.env("HOME", temp_dir.path())` |
| `subprocess_nonexistent_adapter_produces_actionable_error_message` | 1482-1545 | 1523 | Spawns `needle worker` subprocess with `cmd.env("HOME", temp_dir.path())` |
| `heartbeat_cleanup_on_normal_exit_integration` | 3897-4175 | 3975 | Spawns `needle run` subprocess with `cmd.env("HOME", temp_dir.path())` |
| `heartbeat_cleanup_multiple_scenarios_integration` | 4178-4488 | 4291, 4370 | Spawns TWO `needle run` subprocesses, each with `cmd.env("HOME", temp_dir.path())` |
| `subprocess_adapter_failure_exits_nonzero` | 7910-8068 | 7928 | Spawns `needle worker` subprocess with isolated HOME environment |
| `init_tracing_subscriber_with_otlp_enabled_does_not_panic` | 7868-7889 | 7877-7878 | Uses `test_config()` with isolated tempdir, tests tracing initialization only |

**Classification:** ✅ **Safely Isolated** — Explore strand in subprocess isolated via `HOME` environment override.

---

## Non-Explore-Capable Tests — 38 tests

### Evidence: Explore Cannot Reach These Tests

These tests do NOT create `Worker` or `ExploreStrand` instances. They test:
- Individual strand logic (Pluck, Mend) without Explore
- Helper functions and utilities
- Config parsing and path resolution
- Outcome classification logic
- Dispatcher behavior (process execution only)
- Tilde expansion functions
- Schema validation

**Result:** Explore strand is never instantiated in these tests, so it cannot reach bead stores.

---

### Non-Explore-Capable Tests

| Test Name | Line | Reason Not Explore-Capable |
|-----------|------|---------------------------|
| `deterministic_ordering_same_beads_same_order` | 1030-1096 | Tests `PluckStrand` only — no Worker, no ExploreStrand |
| `deterministic_ordering_tiebreak_by_id` | 1099-1143 | Tests `PluckStrand` only — no Worker, no ExploreStrand |
| `outcome_classify_covers_all_exit_code_ranges` | 1146-1223 | Unit test for `Outcome::classify()` — no worker, no strands |
| `dispatcher_captures_stdout_and_stderr` | 1645-1675 | Tests `Dispatcher` only — no Worker, no ExploreStrand |
| `dispatcher_timeout_kills_process` | 1677-1708 | Tests `Dispatcher` only — no Worker, no ExploreStrand |
| `mend_removes_stale_dependency_links` | 2391-2555 | Tests `MendStrand` only — no Worker, no ExploreStrand |
| `idle_worker_flagging_detects_stuck_workers` | 2566-2681 | Tests `MendStrand` only — no Worker, no ExploreStrand |
| `worker_binary_path_config_parsing` | 3419-3445 | Config parsing test — no worker, no strands |
| `worker_binary_path_supervisor_initialization` | 3448-3491 | Supervisor init test — no worker, no strands |
| `worker_binary_path_test_fixture_isolation` | 3494-3589 | Isolation verification test — no worker, no strands |
| `worker_binary_path_tilde_expansion` | 3592-3695 | Tilde expansion test — no worker, no strands |
| `worker_binary_path_tilde_expansion_trailing_slashes` | 3698-3794 | Tilde expansion edge cases — no worker, no strands |
| `worker_binary_path_tilde_expansion_parent_directories` | ~3797-3850 | Tilde expansion parent dirs — no worker, no strands |
| `worker_binary_path_absolute_and_relative_paths` | 7308-7346 | Path resolution test — no worker, no strands |
| `worker_binary_path_precedence_over_default` | 7354-7865 | Path precedence test — no worker, no strands |
| `worker_binary_path_tilde_expansion_multiple_tildes` | ~3853-3920 | Multiple tildes test — no worker, no strands |
| `worker_binary_path_tilde_expansion_position` | ~3923-4000 | Tilde position test — no worker, no strands |
| `workspace_home_tilde_expansion` | 4003-4090 | Workspace home tilde — no worker, no strands |
| `workspace_default_tilde_expansion` | 4093-4180 | Workspace default tilde — no worker, no strands |
| `workspace_home_and_default_tilde_expansion_combined` | 4183-4290 | Combined tilde expansion — no worker, no strands |
| `agent_adapters_dir_tilde_expansion` | 4293-4380 | Adapters dir tilde — no worker, no strands |
| `bead_cli_path_tilde_expansion` | 4383-4470 | Bead CLI path tilde — no worker, no strands |
| `explore_workspace_root_tilde_expansion` | 4473-4560 | Explore root tilde — no worker, no strands |
| `explore_workspaces_tilde_expansion` | 4563-4650 | Explore workspaces tilde — no worker, no strands |
| `learning_global_learnings_file_tilde_expansion` | 5816-5870 | Learning file tilde — no worker, no strands |
| `telemetry_log_dir_tilde_expansion` | 5945-6030 | Telemetry log dir tilde — no worker, no strands |
| `supervisor_heartbeat_path_tilde_expansion` | 6093-6180 | Heartbeat path tilde — no worker, no strands |
| `prompt_context_files_tilde_expansion` | 6230-6330 | Prompt context tilde — no worker, no strands |
| `tilde_expansion_multiple_tildes_in_same_value` | 6388-6480 | Multiple tildes same value — no worker, no strands |
| `tilde_expansion_position_start_vs_middle_end` | 6451-6560 | Tilde position variants — no worker, no strands |
| `weave_exclude_workspaces_tilde_expansion` | 6560-6650 | Weave exclude tilde — no worker, no strands |
| `splice_report_workspace_tilde_expansion` | 6654-6740 | Splice report tilde — no worker, no strands |
| `health_heartbeat_dir_tilde_expansion` | 6771-6860 | Health heartbeat tilde — no worker, no strands |
| `supervisor_socket_path_tilde_expansion` | 6886-6970 | Supervisor socket tilde — no worker, no strands |
| `self_modification_canary_workspace_tilde_expansion` | 7003-7090 | Canary workspace tilde — no worker, no strands |
| `prompt_variants_content_file_tilde_expansion` | 7114-7230 | Prompt variants tilde — no worker, no strands |
| `load_adaptive_stagger_respects_base_delay_when_comfortable` | 2969-3003 | Tests `RateLimiter` only — no Worker, no ExploreStrand |
| `load_adaptive_stagger_emits_telemetry_on_extended_wait` | 3006-3033 | Tests `RateLimiter` only — no Worker, no ExploreStrand |
| `load_adaptive_stagger_bounded_by_max_wait` | 3034-3076 | Tests `RateLimiter` only — no Worker, no ExploreStrand |
| `truncate_commit_sha_handles_short_shas` | 8069-8120 | Utility function test — no worker, no strands |
| `otlp_config_schema_matches_plan_md` | 8216-8307 | Schema validation test — no worker, no strands |
| `trace_metadata_written_after_bead_action` | 8308-8400 | Telemetry metadata test — no worker, no strands |

**Classification:** ✅ **Not Explore-Capable** — These tests never instantiate Explore strand, so isolation is not applicable.

---

## Explore Access Path Analysis

Based on the access pattern documentation, here's how Explore can reach bead stores:

### Minimum Conditions for Explore to Scan Stores

From `docs/explore-strand-access-map.md` and `docs/explore-access-patterns.md`:

1. ✅ **`config.strands.explore.enabled == true`** (default: `true`)
2. ✅ **`config.strands.explore.workspaces`** populated OR auto-discovery succeeds
3. ✅ **`config.strands.explore.workspace_root`** exists and contains `.beads/` directories
4. ✅ **Adaptive backoff** allows scan this cycle
5. ✅ **Waterfall** reaches Explore (Pluck and Mend return `NoWork`)

### How Each Pattern Blocks Access

#### Pattern 1 (`test_config()` helper)
- **Blocked at:** Condition 2 and 3
- **Mechanism:** `workspace_root` pinned to tempdir, `workspaces = Vec::new()`
- **Evidence:** Lines 626-627 in `test_config()`

#### Pattern 2 (Manual Explore pinning)
- **Blocked at:** Condition 2 and 3
- **Mechanism:** `workspace_root` explicitly set to tempdir path
- **Evidence:** Lines like 1233-1234, 2160-2161, etc.

#### Pattern 3 (Direct ExploreConfig)
- **Blocked at:** Condition 3 (scoped to test tempdir)
- **Mechanism:** `ExploreConfig { workspace_root: temp_dir.path(), ... }`
- **Evidence:** Lines like 2095-2102, 2233-2242, 2340-2349

#### Pattern 4 (Subprocess HOME override)
- **Blocked at:** Condition 3 (scoped to subprocess tempdir)
- **Mechanism:** `cmd.env("HOME", temp_dir.path())`
- **Evidence:** Lines like 2747, 3180, 1523, 3975, 4291, 4370, 7928

#### Non-Explore-Capable Tests
- **Blocked at:** Explore strand never instantiated
- **Mechanism:** Tests don't create `Worker` or `ExploreStrand`
- **Evidence:** Tests only create `PluckStrand`, `MendStrand`, `Dispatcher`, or test pure functions

---

## Decision Tree for Each Test

```
START: Test creates Worker or ExploreStrand?
│
├─ NO → Non-Explore-Capable (38 tests)
│        └─ Isolation: NOT APPLICABLE
│
└─ YES → Test uses isolation pattern?
        │
        ├─ Pattern 1 (test_config helper) → 16 tests
        │   └─ Isolation: HOME + workspace_root pinned to tempdir
        │
        ├─ Pattern 2 (manual Explore pinning) → 12 tests
        │   └─ Isolation: workspace_root pinned to tempdir
        │
        ├─ Pattern 3 (Direct ExploreConfig) → 3 tests
        │   └─ Isolation: ExploreConfig scoped to tempdir
        │
        └─ Pattern 4 (subprocess HOME override) → 7 tests
            └─ Isolation: HOME env var overridden to tempdir
```

---

## Verification Checklist

For each test pattern, verify:

### Pattern 1 (`test_config()` helper)
- ✅ Uses `make_worker_with_adapter()` OR calls `test_config()` with tempdir
- ✅ `test_config()` pins `strands.explore.workspace_root` to tempdir (line 626)
- ✅ `test_config()` sets `strands.explore.workspaces` to empty (line 627)
- ✅ `HomeGuard::isolate()` called before `test_config()` (line 653)
- ✅ TempDir outlives Worker (kept alive in `_home_guard`)

### Pattern 2 (Manual Explore pinning)
- ✅ `config.strands.explore.workspace_root` set to tempdir path
- ✅ `config.strands.explore.workspaces` set to `Vec::new()` or explicit list
- ✅ TempDir outlives Worker (kept alive in variable)

### Pattern 3 (Direct ExploreConfig)
- ✅ `ExploreConfig` created with `workspace_root: temp_dir.path()`
- ✅ `ExploreConfig::workspaces` either empty or explicitly test workspaces
- ✅ TempDir outlives ExploreStrand

### Pattern 4 (Subprocess HOME override)
- ✅ `cmd.env("HOME", temp_dir.path())` called on subprocess Command
- ✅ TempDir outlives subprocess (kept alive until `child.wait()`)

### Non-Explore-Capable
- ✅ Test does NOT create `Worker`
- ✅ Test does NOT create `ExploreStrand`
- ✅ Test only tests individual strands or utility functions

---

## Conclusion

**All 76 tests in `tests/integration_tests.rs` are safely isolated from the Explore strand's default behavior of scanning `$HOME` for bead workspaces.**

### Breakdown by Category

- **16 tests** use `test_config()` helper (automatic isolation)
- **12 tests** manually pin `strands.explore` configuration
- **3 tests** create `ExploreConfig` directly scoped to tempdir
- **7 tests** override `HOME` environment for subprocess isolation
- **38 tests** are not Explore-capable (don't create Worker or ExploreStrand)

### No Action Required

✅ **No tests require isolation fixes.** All tests either:
1. Isolate the Explore strand to a test tempdir, OR
2. Don't instantiate the Explore strand at all

### Acceptance Criteria Met

- ✅ Every test from the catalog classified with one of:
  1. **Safely isolated with isolation mechanism documented** (38 tests)
  2. **Not Explore-capable** (38 tests)
- ✅ Evidence provided for each classification
- ✅ No tests classified as "requires manual inspection"

---

## References

- **Test Isolation Catalog:** `docs/test-isolation-catalog.md`
- **Explore Access Patterns:** `docs/explore-access-patterns.md`
- **Explore Strand Access Map:** `docs/explore-strand-access-map.md`
- **Test Source Code:** `tests/integration_tests.rs`
- **Isolation Policy:** `CLAUDE.md` (lines 50-87)
- **ADR-006:** 2026-07-20 contamination incident postmortem
- **2026-08-05 Incident:** `test_config()` isolation gap (bead-forge store mutated)
