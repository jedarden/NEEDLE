# NEEDLE Test Isolation Patterns

This document catalogs the isolation patterns used across NEEDLE's test suite to prevent bead store contamination. All patterns serve the same goal: **prevent the Explore strand from scanning the real user's home directory and mutating production bead stores.**

## Background

The Explore strand (enabled by default via `ExploreConfig::default_enabled()`) scans `workspace_root` (defaulting to `$HOME`) for bead workspaces. Without isolation, a test's worker—whether spawned as a subprocess or built in-process—will leak into the real user environment and scan real repos, contaminating both the test and production bead stores.

### Why This Matters

**The 2026-08-05 contamination incident:** A test using `test_config()` in `tests/integration_tests.rs` isolated `workspace.default` and `workspace.home` but NOT `strands.explore`. This let an orphaned local `integration_tests` binary roam into bead-forge's live store, mutate beads to `in_progress` under assignee `echo-test-test-worker`, and truncate `.beads/issues.jsonl` to 0 bytes (2302 beads, recovered from git). See ADR-006 for full postmortem.

**The 2026-07-20 contamination incident:** A non-isolated test created ~284 phantom beads across ~22 repos under fixture worker identifiers. This led to the initial Test Isolation Policy in CLAUDE.md.

---

## Pattern 1: `test_config()` Helper (Auto-Isolation)

**When to use:** In-process tests that build a `Worker` directly and want automatic isolation via a helper function.

**How it works:** The `test_config()` helper (defined in `tests/integration_tests.rs:251-286`) automatically configures isolation for you. Pass a tempdir path as `workspace_home`, and the helper pins the Explore strand to that directory.

### Code Example

```rust
fn test_config(adapter_name: &str, workspace_home: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    config.agent.default = adapter_name.to_string();
    config.agent.timeout = 10;
    config.agent.routing = None; // Disable routing in tests - use adapter directly
    config.self_modification.hot_reload = false;
    // Match the test bead workspace so the remote-store-switch logic
    // doesn't fire (it would try to create a real CLI store).
    config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
    // Isolate workspace home so the registry doesn't leak between tests.
    config.workspace.home = workspace_home.to_path_buf();

    // ═════════════════════════════════════════════════════════════════════════════
    // REQUIRED: Explore strand isolation
    // ═════════════════════════════════════════════════════════════════════════════
    // Confine the Explore strand to the test's temp home.
    //
    // REQUIRED — see "Test Isolation Policy" in CLAUDE.md and ADR-006. That
    // policy is written for tests that spawn the `needle` *binary* and isolate
    // it with `cmd.env("HOME", ...)`. Workers built in-process here never go
    // through a child process, so no `HOME` override applies and they inherit
    // `ExploreConfig::default()`: `workspaces: []` (= auto-discover) with
    // `workspace_root` defaulting to the real home directory
    // (`ExploreConfig::default_workspace_root()` -> `dirs_or_home("")`).
    //
    // Left unset, Explore scans every directory under $HOME containing a
    // `.beads/` and claims REAL beads from REAL repos under the fixture
    // adapter/worker identity. On 2026-08-05 that emptied bead-forge's live
    // store — beads were mutated to `in_progress` under assignee
    // `echo-test-test-worker` (adapter `echo-test` + worker `test-worker`)
    // and `.beads/issues.jsonl` was truncated to 0 bytes.
    config.strands.explore.workspace_root = workspace_home.to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    // ═════════════════════════════════════════════════════════════════════════════

    // Enable OTLP sink to trigger the runtime guard (bf-4nwm7).
    // This ensures that init_tracing_subscriber's tokio::spawn calls
    // work correctly with the runtime context guard from bf-3s2b0.
    config.telemetry.otlp_sink.enabled = true;
    config
}

// Usage in test
#[tokio::test]
async fn example_test() {
    let home_dir = tempfile::tempdir().expect("failed to create temp dir for test workspace home");
    let config = test_config("echo-test", home_dir.path());
    let worker = Worker::new(config, "test-worker".to_string(), store);
    // ... test logic
}
```

### Why This Pattern Works

- **Reusability:** Centralizes isolation logic in one helper function
- **Explicit:** Makes the tempdir dependency visible in the function signature
- **Self-documenting:** The helper includes a detailed comment explaining WHY isolation is needed

### Common Mistakes

1. **Using `Config::default()` directly** without pinning `strands.explore.workspace_root`
2. **Isolating only `workspace.home` but not `strands.explore`** (the exact bug from 2026-08-05)
3. **Letting the tempdir drop before the worker finishes** (keep `TempDir` alive for the test duration)

---

## Pattern 2: Manual `strands.explore` Configuration

**When to use:** When you can't use `test_config()` (e.g., different config requirements) and need to manually configure isolation.

**How it works:** Directly mutate `config.strands.explore` after creating your `Config`, pinning both `workspace_root` and `workspaces` to test-specific paths.

### Code Example

```rust
#[tokio::test]
async fn exhaustion_with_idle_action_exit() {
    let store: Arc<dyn BeadStore> = Arc::new(IntegrationMockStore::empty());
    let _home_dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    config.agent.default = "echo-test".to_string();
    config.agent.routing = None; // Disable routing in tests - use adapter directly
    config.self_modification.hot_reload = false;
    config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
    config.workspace.home = _home_dir.path().to_path_buf();

    // ═════════════════════════════════════════════════════════════════════════════
    // REQUIRED: Explore strand isolation
    // ═════════════════════════════════════════════════════════════════════════════
    // Isolate Explore strand to prevent scanning real home directory
    // REQUIRED — see ADR-006 and Test Isolation Policy in CLAUDE.md
    config.strands.explore.workspace_root = _home_dir.path().to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    // ═════════════════════════════════════════════════════════════════════════════

    let mut worker = Worker::new(config, "test-worker".to_string(), store);
    // ... test logic
}
```

### Why This Pattern Works

- **Flexibility:** Allows custom config that doesn't match `test_config()` defaults
- **Explicit:** Makes every config field visible and controllable
- **Same guarantees:** Provides identical isolation to Pattern 1 when both fields are pinned

### Common Mistakes

1. **Forgetting to set `workspaces` to `Vec::new()`** (leaving it empty enables auto-discovery from `workspace_root`, which must be isolated)
2. **Setting only `workspaces` but not `workspace_root`** (auto-discovery still scans from real home)
3. **Using the same tempdir for multiple tests** (share no state between tests)

---

## Pattern 3: Direct `ExploreConfig` with Isolated Tempdir

**When to use:** When testing `ExploreStrand` directly (bypassing `Worker`), typically in strand-specific integration tests.

**How it works:** Create an `ExploreConfig` struct directly, passing an isolated tempdir as `workspace_root` and controlling `workspaces` explicitly.

### Code Example

```rust
#[tokio::test]
async fn cross_workspace_mend_releases_zombie_beads_and_returns_tagged_bead() {
    use needle::config::ExploreConfig;
    use needle::strand::{ExploreStrand, Strand};
    use std::fs;

    // Create real temporary directories for home and remote workspaces.
    let home_dir = tempfile::tempdir().unwrap();
    let home_workspace = home_dir.path().to_path_buf();
    let home_store = Arc::new(IntegrationMockStore::empty());

    let remote_dir = tempfile::tempdir().unwrap();
    let remote_workspace = remote_dir.path().to_path_buf();
    let remote_beads_dir = remote_workspace.join(".beads");
    fs::create_dir_all(&remote_beads_dir).unwrap();

    // Initialize the br workspace first.
    let init_output = std::process::Command::new("/home/coding/.local/bin/br")
        .arg("init")
        .current_dir(&remote_workspace)
        .output()
        .expect("br init command failed to execute");

    // Create ExploreStrand with the remote workspace configured.
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = needle::registry::Registry::new(temp_dir.path());
    let telemetry = Telemetry::new("test-worker".to_string());

    // ═════════════════════════════════════════════════════════════════════════════
    // REQUIRED: Explore strand isolation
    // ═════════════════════════════════════════════════════════════════════════════
    let explore_config = ExploreConfig {
        enabled: true,
        workspaces: vec![remote_workspace.clone()], // Explicit workspace list
        // Isolate Explore strand to prevent scanning real home directory
        // REQUIRED — see ADR-006 and Test Isolation Policy in CLAUDE.md
        workspace_root: temp_dir.path().to_path_buf(), // Pin to tempdir
        rediscovery_cycles: 60,
        starvation_threshold_minutes: 15,
    };
    // ═════════════════════════════════════════════════════════════════════════════

    let explore = ExploreStrand::new(
        explore_config,
        home_workspace,
        registry,
        telemetry,
        "test-worker".to_string(),
    );

    // Evaluate ExploreStrand — it should run cross-workspace mend.
    let result = explore.evaluate(home_store.as_ref(), &HashSet::new()).await;
    // ... assertions
}
```

### Why This Pattern Works

- **Direct control:** Tests `ExploreStrand` behavior without `Worker` overhead
- **Explicit workspaces:** Can test specific workspace configurations (auto-discovery vs pinned list)
- **Same isolation model:** Uses tempdir-based `workspace_root` pinning like other patterns

### Common Mistakes

1. **Using the real home directory as `workspace_root`** (defeats the entire purpose of isolation)
2. **Using the same tempdir for multiple `ExploreStrand` instances** (state leakage)
3. **Forgetting that `workspaces: vec![]` means auto-discovery** (empty ≠ disabled, it means discover all under `workspace_root`)

---

## Pattern 4: Subprocess Tests with `HOME` Environment Override

**When to use:** When spawning a real `needle` binary as a subprocess via `Command::new(CARGO_BIN_EXE_needle)`.

**How it works:** Override the `HOME` environment variable in the subprocess, causing the spawned binary's `ExploreConfig::default_workspace_root()` (which calls `dirs_or_home("")`) to resolve to the test's tempdir instead of the real home.

### Code Example

```rust
#[tokio::test]
async fn dead_worker_cleanup_integration() {
    // Integration test: verify that dead workers are proactively cleaned up
    // from the registry file during the mend strand cycle.
    let temp_dir = tempfile::tempdir().unwrap();
    let workspace = temp_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let reg_dir = temp_dir.path().join("registry");
    std::fs::create_dir_all(&reg_dir).unwrap();

    let registry = needle::registry::Registry::new(&reg_dir);

    // Register a live worker.
    let live_entry = needle::registry::WorkerEntry {
        id: "claude-live-worker".to_string(),
        pid: std::process::id(),
        workspace: workspace.clone(),
        agent: "claude".to_string(),
        model: Some("sonnet".to_string()),
        provider: Some("anthropic".to_string()),
        started_at: Utc::now() - chrono::Duration::seconds(300),
        beads_processed: 10,
    };
    registry.register(live_entry.clone()).unwrap();

    // Run the needle worker with a single mend cycle.
    // IMPORTANT: Isolate HOME to prevent Explore strand from scanning the real user workspace.
    // Without this, the spawned needle binary would leak into the real $HOME and scan real repos,
    // contaminating the test environment (see ADR-006 and the 2026-07-20 contamination incident).
    let bin_path = std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());
    let mut cmd = Command::new(&bin_path);
    cmd.arg("worker")
        .arg("--once")
        .arg("--adapter=claude")
        .arg("--model=test")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--registry")
        .arg(&reg_dir)
        .env("HOME", temp_dir.path()) // ════════════════════════════════════════════════════
                                       // Isolate Explore's workspace_root to test tempdir.
                                       // The spawned needle binary will call
                                       // ExploreConfig::default_workspace_root() ->
                                       // dirs_or_home(""), which resolves to $HOME.
                                       // Without this override, it scans the real user's home.
                                       // ════════════════════════════════════════════════════
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Spawn the process and wait with timeout to prevent hangs
    let child = cmd.spawn().expect("Failed to spawn worker");

    // ... test logic for waiting on child and asserting behavior
}
```

### Why This Pattern Works

- **Process-level isolation:** The spawned binary is a separate OS process with its own environment
- **No code changes needed:** `ExploreConfig::default_workspace_root()` automatically respects `$HOME`
- **Identical to production:** Tests the exact code path that runs in production (no test-specific config mutations)

### Common Mistakes

1. **Forgetting `.env("HOME", ...)` entirely** (most common mistake)
2. **Using `std::env::set_var("HOME", ...)` instead of `.env()`** (modifies the test process's env, not the subprocess's)
3. **Letting the tempdir drop before the subprocess exits** (keep `TempDir` alive until `child.wait()` completes)
4. **Assuming `PATH` or other env vars are isolated** (only `HOME` is overridden; other env vars leak from parent)

---

## Decision Tree: Which Pattern to Use

```
┌─────────────────────────────────────────────────────────────────┐
│ Are you spawning a real needle binary as a subprocess?          │
└─────────────────────────────────────────────────────────────────┘
                           │ YES
                           ▼
           ┌───────────────────────────────┐
           │ Pattern 4: HOME env override  │
           │ cmd.env("HOME", tempdir.path())│
           └───────────────────────────────┘

                           │ NO
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│ Are you testing ExploreStrand directly (not via Worker)?      │
└─────────────────────────────────────────────────────────────────┘
                           │ YES
                           ▼
     ┌──────────────────────────────────────────┐
     │ Pattern 3: Direct ExploreConfig          │
     │ ExploreConfig { workspace_root: tempdir } │
     └──────────────────────────────────────────┘

                           │ NO
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│ Can you use the test_config() helper?                         │
│ (Does your test accept standard config defaults?)             │
└─────────────────────────────────────────────────────────────────┘
                           │ YES
                           ▼
      ┌─────────────────────────────────────────┐
      │ Pattern 1: test_config() helper         │
      │ let config = test_config(name, home);   │
      └─────────────────────────────────────────┘

                           │ NO
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│ Do you need custom config but still use Worker?                │
└─────────────────────────────────────────────────────────────────┘
                           │ YES
                           ▼
   ┌────────────────────────────────────────────────────┐
   │ Pattern 2: Manual strands.explore configuration    │
   │ config.strands.explore.workspace_root = tempdir;   │
   │ config.strands.explore.workspaces = vec![];       │
   └────────────────────────────────────────────────────┘
```

---

## Verification Checklist

After writing a new test, verify it follows isolation correctly:

- [ ] **Subprocess tests?** Used `.env("HOME", tempdir.path())` on the `Command`
- [ ] **In-process tests?** Pinned `config.strands.explore.workspace_root` to a tempdir
- [ ] **In-process tests?** Set `config.strands.explore.workspaces` (either `Vec::new()` for auto-discovery or explicit list)
- [ ] **Tempdir lifetime?** The `TempDir` outlives the worker/subprocess (no early drops)
- [ ] **No shared state?** Each test uses a fresh tempdir (no reuse across tests)
- [ ] **Documentation?** Added a comment referencing ADR-006/Test Isolation Policy if the pattern isn't obvious

---

## Anti-Patterns: What NOT to Do

### ❌ Using `Config::default()` Without Isolation

```rust
// WRONG: This scans the real home directory
let config = Config::default();
let worker = Worker::new(config, "test-worker".to_string(), store);
```

**Why it's wrong:** `Config::default()` inherits `ExploreConfig::default()`, which sets `workspace_root` to `dirs_or_home("")` → real `$HOME`. The worker will scan every directory under your home containing `.beads/`.

### ❌ Isolating `workspace.home` but NOT `strands.explore`

```rust
// WRONG: This was the exact bug from 2026-08-05
let mut config = Config::default();
config.workspace.home = _home_dir.path().to_path_buf(); // Isolated workspace home
config.workspace.default = std::path::PathBuf::from("/tmp/test-workspace");
// FORGOT: config.strands.explore.workspace_root = _home_dir.path().to_path_buf();
let worker = Worker::new(config, "test-worker".to_string(), store);
```

**Why it's wrong:** `workspace.home` is for NEEDLE's own registry/heartbeat files, NOT for Explore's scan root. The Explore strand still scans the real home even though `workspace.home` is isolated.

### ❌ Using `std::env::set_var` for Subprocess Isolation

```rust
// WRONG: This modifies the test process, not the subprocess
std::env::set_var("HOME", temp_dir.path());
let mut cmd = Command::new(&bin_path);
// FORGOT: cmd.env("HOME", temp_dir.path())
let child = cmd.spawn().expect("Failed to spawn worker");
```

**Why it's wrong:** `std::env::set_var` modifies the current process's environment. Subprocesses inherit their environment at spawn time via `.env()`. Without `.env("HOME", ...)`, the spawned binary gets the test's original `$HOME`, not the overridden value.

### ❌ Using the Same Tempdir Across Multiple Tests

```rust
// WRONG: Shared tempdir causes state leakage between tests
lazy_static! {
    static ref SHARED_TEMPDIR: TempDir = tempfile::tempdir().unwrap();
}

#[tokio::test]
async fn test_one() {
    let config = test_config("echo-test", SHARED_TEMPDIR.path());
    // ... test logic that might leave state
}

#[tokio::test]
async fn test_two() {
    let config = test_config("echo-test", SHARED_TEMPDIR.path());
    // ... contaminated by test_one's leftover state
}
```

**Why it's wrong:** Tests MUST be independent. A tempdir shared across tests leaks registry files, heartbeat files, and partial state from one test to another, causing flaky failures.

---

## References

- **ADR-006:** Full postmortem of the 2026-07-20 contamination incident (~284 phantom beads across ~22 repos)
- **CLAUDE.md Test Isolation Policy:** Original policy document (lines 50-87 in `/home/coding/NEEDLE/CLAUDE.md`)
- **2026-08-05 incident:** `test_config()` isolation gap (bead-forge store mutated, 2302 beads truncated to 0 bytes)
- **Explore strand code:** `src/strand/explore.rs` (workspace discovery logic)
- **Config defaults:** `src/config/mod.rs` (ExploreConfig::default(), workspace_root resolution)

---

## Quick Reference

| Pattern | When to Use | Key Lines | Example Location |
|---------|--------------|-----------|------------------|
| Pattern 1: `test_config()` helper | In-process tests with standard config needs | `let config = test_config(name, home_dir.path());` | `tests/integration_tests.rs:251-286` |
| Pattern 2: Manual `strands.explore` | In-process tests with custom config | `config.strands.explore.workspace_root = tempdir;`<br>`config.strands.explore.workspaces = vec![];` | `tests/integration_tests.rs:578-579` |
| Pattern 3: Direct `ExploreConfig` | Testing `ExploreStrand` directly | `ExploreConfig { workspace_root: tempdir, workspaces: vec![...] }` | `tests/integration_tests.rs:1555-1563` |
| Pattern 4: Subprocess `HOME` override | Spawning real `needle` binary | `cmd.env("HOME", temp_dir.path())` | `tests/integration_tests.rs:2208` |
