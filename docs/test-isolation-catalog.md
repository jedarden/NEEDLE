# NEEDLE Test Isolation Catalog

This document catalogs the isolation configuration for every Explore-capable test in `tests/integration_tests.rs`. Each test listing shows the minimal safe configuration needed to prevent bead store contamination.

## Background

The Explore strand (enabled by default) scans `workspace_root` (defaulting to `$HOME`) for bead workspaces. Without isolation, tests leak into the real user environment and mutate production bead stores.

**2026-08-05 contamination incident:** An in-process Worker test without Explore isolation let an orphaned `integration_tests` binary mutate 2302 beads to `in_progress` under assignee `echo-test-test-worker` and truncate `.beads/issues.jsonl` to 0 bytes (recovered from git).

**2026-07-20 contamination incident:** A non-isolated test created ~284 phantom beads across ~22 repos under fixture worker identifiers.

See `docs/testing-isolation-patterns.md` for detailed patterns and anti-patterns.

---

## Pattern 1: `test_config()` Helper (Auto-Isolation)

Tests using `make_worker_with_adapter()` or `test_config()` automatically get Explore isolation via the helper function at lines 376-397.

### Isolation Mechanism
```rust
fn test_config(adapter_name: &str, workspace_home: &std::path::Path) -> Config {
    // ... other config ...
    config.strands.explore.workspace_root = workspace_home.to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    // ...
}
```

### Tests Using This Pattern

| Test Name | Line | Usage |
|-----------|------|-------|
| `end_to_end_single_bead_success` | 438 | `make_worker_with_adapter()` |
| `end_to_end_worker_loops_to_next_bead` | 465 | `make_worker_with_adapter()` |
| `outcome_path_success_exit_0` | 492 | `make_worker_with_adapter()` |
| `outcome_path_failure_exit_1` | 509 | `make_worker_with_adapter()` |
| `outcome_path_timeout_exit_124` | 539 | `make_worker_with_adapter()` |
| `outcome_path_agent_not_found_exit_127` | 570 | `make_worker_with_adapter()` |
| `outcome_path_crash_exit_137` | 597 | `make_worker_with_adapter()` |
| `outcome_path_interrupted_via_shutdown_flag` | 625 | `test_config()` |
| `exhaustion_empty_workspace` | 659 | `test_config()` |
| `full_cycle_produces_telemetry_state_transitions` | 1622 | `make_worker_with_adapter()` |
| `worker_processes_high_priority_beads_first` | 1715 | `make_worker_with_adapter()` |
| `shutdown_during_selecting_exits_cleanly` | 974 | `test_config()` |
| `shutdown_flag_preempts_execution` | 997 | `test_config()` |
| `worker_boot_succeeds_with_valid_adapter` | 1385 | `make_worker_with_adapter()` |
| `init_tracing_subscriber_with_otlp_enabled_does_not_panic` | 4450 | `test_config()` |

### Minimal Safe Configuration

```rust
let home_dir = tempfile::tempdir().expect("failed to create temp dir for test workspace home");
let config = test_config("echo-test", home_dir.path());
let worker = Worker::new(config, "test-worker".to_string(), store);
// Keep home_dir alive for test duration
```

---

## Pattern 2: Manual `strands.explore` Configuration

Tests that manually pin `config.strands.explore.workspace_root` and `workspaces`.

### Isolation Mechanism
```rust
config.strands.explore.workspace_root = tempdir.path().to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

### Tests Using This Pattern

| Test Name | Line | Isolation Lines |
|-----------|------|-----------------|
| `exhaustion_with_idle_action_exit` | 707 | 719-720 |
| `exhaustion_with_idle_action_wait_survives_sleep` | 742 | 942-943 |
| `worker_boot_rejects_invalid_config` | 1226 | 1233-1234 |
| `worker_boot_rejects_nonexistent_adapter` | 1271 | 1279-1280 |
| `worker_boot_rejects_nonexistent_adapter_before_claiming_work` | 1333 | 1342-1343 |
| `adapter_validation_happens_before_main_worker_loop` | 1425 | 1436-1437 |
| `debug_worker_hang` | 2853 | 2882-2883 |

### Minimal Safe Configuration

```rust
let _home_dir = tempfile::tempdir().unwrap();
let mut config = Config::default();
// ... other config fields ...
config.workspace.home = _home_dir.path().to_path_buf();
// REQUIRED: Explore strand isolation
config.strands.explore.workspace_root = _home_dir.path().to_path_buf();
config.strands.explore.workspaces = Vec::new();

let mut worker = Worker::new(config, "test-worker".to_string(), store);
// Keep _home_dir alive for test duration
```

---

## Pattern 3: Direct `ExploreConfig`

Tests that create `ExploreConfig` directly, typically when testing `ExploreStrand` behavior.

### Isolation Mechanism
```rust
let explore_config = ExploreConfig {
    enabled: true,
    workspaces: vec![...],  // Explicit workspace list or empty
    workspace_root: temp_dir.path().to_path_buf(),  // Pin to tempdir
    rediscovery_cycles: 60,
    starvation_threshold_minutes: 15,
};
```

### Tests Using This Pattern

| Test Name | Line | Isolation Lines |
|-----------|------|-----------------|
| `cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead` | 2001 | 2095-2102 |
| `cross_workspace_mend_skips_beads_with_live_assignees` | 2149 | 2233-2242 |
| `cross_workspace_mend_skips_own_worker_beads` | 2281 | 2340-2349 |

### Minimal Safe Configuration

```rust
let temp_dir = tempfile::tempdir().unwrap();
let explore_config = ExploreConfig {
    enabled: true,
    workspaces: vec![remote_workspace.clone()],  // or Vec::new() for auto-discovery
    workspace_root: temp_dir.path().to_path_buf(),  // Pin to tempdir
    rediscovery_cycles: 60,
    starvation_threshold_minutes: 15,
};

let explore = ExploreStrand::new(
    explore_config,
    home_workspace,
    registry,
    telemetry,
    "test-worker".to_string(),
);
// Keep temp_dir alive for test duration
```

---

## Pattern 4: Subprocess `HOME` Environment Override

Tests that spawn the actual `needle` binary as a subprocess and override `HOME` to isolate Explore's scan root.

### Isolation Mechanism
```rust
cmd.env("HOME", temp_dir.path())  // Isolate Explore's workspace_root
```

### Tests Using This Pattern

| Test Name | Line | Isolation Line |
|-----------|------|----------------|
| `dead_worker_cleanup_integration` | 2684 | 2747 |
| `heartbeat_cleanup_on_signal_integration` | 3096 | 3180 |
| `subprocess_nonexistent_adapter_produces_actionable_error_message` | 1482 | 1523 |
| `heartbeat_cleanup_on_normal_exit_integration` | 3897 | 3975 |
| `heartbeat_cleanup_multiple_scenarios_integration` | 4178 | 4291, 4370 |
| `subprocess_adapter_failure_exits_nonzero` | 4492 | 4543 |

### Minimal Safe Configuration

```rust
let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
let mut cmd = Command::new(&needle_binary);
cmd.arg("worker")
    .arg("--once")
    .arg("--workspace")
    .arg(&workspace)
    .env("HOME", temp_dir.path())  // ISOLATION: Prevent scanning real user directories
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

let child = cmd.spawn().expect("Failed to spawn worker");
// Keep temp_dir alive until child.wait() completes
```

---

## Non-Explore-Capable Tests

These tests do NOT create Workers or ExploreStrands, so they do NOT require Explore isolation:

| Test Name | Line | Reason |
|-----------|------|--------|
| `deterministic_ordering_same_beads_same_order` | 1030 | Tests `PluckStrand` only (no Explore) |
| `deterministic_ordering_tiebreak_by_id` | 1099 | Tests `PluckStrand` only (no Explore) |
| `outcome_classify_covers_all_exit_code_ranges` | 1146 | Unit test for `Outcome::classify()` (no worker) |
| `dispatcher_captures_stdout_and_stderr` | 1645 | Tests `Dispatcher` only (no worker) |
| `dispatcher_timeout_kills_process` | 1677 | Tests `Dispatcher` only (no worker) |
| `mend_removes_stale_dependency_links` | 2391 | Tests `MendStrand` only (no Explore) |
| `idle_worker_flagging_detects_stuck_workers` | 2566 | Tests `MendStrand` only (no Explore) |
| `worker_binary_path_config_parsing` | 3419 | Config parsing test (no worker) |
| `worker_binary_path_supervisor_initialization` | 3448 | Supervisor init test (no worker) |
| `worker_binary_path_test_fixture_isolation` | 3494 | Isolation verification test (no worker) |
| `worker_binary_path_tilde_expansion` | 3592 | Tilde expansion test (no worker) |
| `worker_binary_path_tilde_expansion_trailing_slashes` | 3698 | Tilde expansion edge cases (no worker) |
| `worker_binary_path_absolute_and_relative_paths` | 3797 | Path resolution test (no worker) |
| `worker_binary_path_precedence_over_default` | 3843 | Path precedence test (no worker) |
| `load_adaptive_stagger_respects_base_delay_when_comfortable` | 2969 | Tests `RateLimiter` only (no worker) |
| `load_adaptive_stagger_emits_telemetry_on_extended_wait` | 3006 | Tests `RateLimiter` only (no worker) |
| `load_adaptive_stagger_bounded_by_max_wait` | 3034 | Tests `RateLimiter` only (no worker) |

---

## Verification Checklist

When adding a new test, verify it follows isolation correctly:

### In-Process Tests (build `Worker` directly)
- [ ] Used `test_config()` helper OR manually pinned `config.strands.explore.workspace_root`
- [ ] Set `config.strands.explore.workspaces` (either `Vec::new()` or explicit list)
- [ ] TempDir outlives the Worker (no early drops)
- [ ] Added comment referencing ADR-006/Test Isolation Policy if pattern isn't obvious

### Subprocess Tests (spawn `needle` binary)
- [ ] Used `.env("HOME", tempdir.path())` on the `Command`
- [ ] TempDir outlives the subprocess (kept alive until `child.wait()`)
- [ ] Used ProcessGuard or similar cleanup mechanism
- [ ] Added comment explaining isolation requirement

### Non-Explore Tests
- [ ] Verified test does NOT create Worker or ExploreStrand
- [ ] If test creates a Worker, it MUST follow in-process isolation rules above

---

## Quick Reference

| Pattern | When to Use | Key Lines | Example Location |
|---------|--------------|-----------|------------------|
| Pattern 1: `test_config()` helper | In-process tests with standard config needs | `let config = test_config(name, home_dir.path());` | `tests/integration_tests.rs:376-397` |
| Pattern 2: Manual `strands.explore` | In-process tests with custom config | `config.strands.explore.workspace_root = tempdir;`<br>`config.strands.explore.workspaces = vec![];` | `tests/integration_tests.rs:719-720` |
| Pattern 3: Direct `ExploreConfig` | Testing `ExploreStrand` directly | `ExploreConfig { workspace_root: tempdir, workspaces: vec![...] }` | `tests/integration_tests.rs:2095-2102` |
| Pattern 4: Subprocess `HOME` override | Spawning real `needle` binary | `cmd.env("HOME", temp_dir.path())` | `tests/integration_tests.rs:2747` |

---

## References

- **ADR-006:** Full postmortem of the 2026-07-20 contamination incident (~284 phantom beads across ~22 repos)
- **CLAUDE.md Test Isolation Policy:** Original policy document (lines 50-87 in `/home/coding/NEEDLE/CLAUDE.md`)
- **2026-08-05 incident:** `test_config()` isolation gap (bead-forge store mutated, 2302 beads truncated to 0 bytes)
- **Explore strand code:** `src/strand/explore.rs` (workspace discovery logic)
- **Config defaults:** `src/config/mod.rs` (ExploreConfig::default(), workspace_root resolution)
- **Detailed patterns:** `docs/testing-isolation-patterns.md`
