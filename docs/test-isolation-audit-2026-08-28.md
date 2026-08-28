# Test Isolation Audit - Explore Strand Access

**Date**: 2026-08-28  
**Bead**: needle-efd3eb13  
**Purpose**: Audit all Rust tests that construct Config, Worker, or worker loops in-process to determine which can reach the Explore strand without proper isolation.

---

## Executive Summary

**CRITICAL FINDING**: **11 test files are EXPLORE-CAPABLE** and can potentially leak into the real user environment through the Explore strand's workspace scanning capabilities.

The Explore strand (`ExploreConfig::default_enabled()`) scans `workspace_root` (defaulting to `$HOME`) for bead workspaces. Tests that construct `Config::default()` or Worker without explicitly setting `strands.explore.workspace_root` to a tempdir risk:

1. Scanning and mutating real user workspaces
2. Creating phantom beads in production stores
3. Contaminating real bead databases with test artifacts

---

## Audit Results by Classification

### 🔴 EXPLORE-CAPABLE (DANGER - Requires Fix)

These tests construct Config or Worker but **do not isolate the Explore strand**. They can scan and potentially mutate real user workspaces.

#### 1. tests/sigterm_heartbeat_cleanup.rs
- **Risk Level**: HIGH
- **Constructs**: `Config::default()` → HealthMonitor
- **Lines**: 51, 97, 140, 180
- **Missing Isolation**:
  ```rust
  let mut config = needle::config::Config::default();
  config.workspace.home = config_dir.to_path_buf();
  config.health.heartbeat_interval_secs = 1;
  // Missing: config.strands.explore.workspace_root = tempdir;
  // Missing: config.strands.explore.workspaces = Vec::new();
  ```
- **Required Fix**:
  ```rust
  config.strands.explore.workspace_root = config_dir.to_path_buf();
  config.strands.explore.workspaces = Vec::new();
  ```

#### 2. tests/anthropic_routing_e2e_test.rs
- **Risk Level**: HIGH
- **Constructs**: `make_test_config_with_routing()` → Dispatcher
- **Missing Isolation**: Uses `..Default::default()` which includes Explore strand
- **Required Fix**: Add Explore isolation to `make_test_config_with_routing()`

#### 3. tests/anthropic_routing_verification.rs
- **Risk Level**: HIGH
- **Constructs**: `make_test_config()` → Dispatcher
- **Missing Isolation**: Uses `..Default::default()` which includes Explore strand
- **Required Fix**: Add Explore isolation to `make_test_config()`

#### 4. tests/dispatch_model_routing_validation.rs
- **Risk Level**: HIGH
- **Constructs**: `make_test_config()` → Dispatcher
- **Missing Isolation**: Lines 27-38 show config construction without Explore isolation
- **Required Fix**: Add Explore isolation to config construction

#### 5. tests/heartbeat_state_during_dispatch.rs
- **Risk Level**: HIGH
- **Constructs**: `test_config()` → HealthMonitor
- **Lines**: 46-53
- **Missing Isolation**:
  ```rust
  fn test_config(heartbeat_dir: &Path) -> Config {
      let mut config = Config::default();
      config.workspace.home = heartbeat_dir.to_path_buf();
      config.workspace.default = heartbeat_dir.to_path_buf();
      // Missing Explore isolation
  }
  ```
- **Required Fix**:
  ```rust
  config.strands.explore.workspace_root = heartbeat_dir.to_path_buf();
  config.strands.explore.workspaces = Vec::new();
  ```

#### 6. tests/p2_integration_tests.rs
- **Risk Level**: HIGH
- **Constructs**: `Config::default()` → Strand components
- **Lines**: 1381-1388
- **Missing Isolation**: Direct `Config::default()` usage for strand runner
- **Required Fix**: Add Explore isolation before strand construction

#### 7. tests/p3_integration_tests.rs
- **Risk Level**: HIGH
- **Constructs**: `Config::default()` → Strand components
- **Missing Isolation**: Same pattern as p2_integration_tests.rs
- **Required Fix**: Same as p2_integration_tests.rs

#### 8. tests/timeout_config_integration.rs
- **Risk Level**: MEDIUM
- **Constructs**: Config parsing tests (no worker execution)
- **Missing Isolation**: Parsed configs may include default Explore settings
- **Required Fix**: Ensure test YAML configs either disable Explore or isolate to tempdir

#### 9. tests/timeout_config_integration_tests.rs
- **Risk Level**: MEDIUM
- **Constructs**: Config validation tests (no worker execution)
- **Missing Isolation**: Direct `Config::default()` usage
- **Required Fix**: Add Explore isolation to config validation tests

#### 10. tests/upgrade_check_integration.rs
- **Risk Level**: HIGH
- **Constructs**: `Config::default()` → Supervisor tests
- **Lines**: 23-33
- **Missing Isolation**: Direct `Config::default()` usage
- **Required Fix**: Add Explore isolation before supervisor construction

#### 11. tests/test_mend_stale_assignee.rs
- **Risk Level**: LOW
- **Constructs**: `MendConfig::default()` → MendStrand (not full Config)
- **Line**: 514
- **Missing Isolation**: MendConfig doesn't control Explore strand
- **Required Fix**: Ensure test uses full Config with Explore isolation if it runs a full worker

---

### ✅ PROPERLY ISOLATED (Safe - No Action Needed)

These tests correctly isolate the Explore strand to a temporary directory.

#### 1. tests/integration_tests.rs
- **Status**: ✅ SAFE
- **Pattern**: Uses `test_config()` helper with explicit isolation
- **Evidence**:
  ```rust
  fn test_config(adapter_name: &str, workspace_home: &std::path::Path) -> Config {
      let mut config = Config::default();
      // ... other config ...
      // Confine the Explore strand to the test's temp home.
      // REQUIRED — see "Test Isolation Policy" in CLAUDE.md and ADR-006.
      config.strands.explore.workspace_root = workspace_home.to_path_buf();
      config.strands.explore.workspaces = Vec::new();
      config
  }
  ```

#### 2. tests/otlp_integration.rs
- **Status**: ✅ SAFE
- **Pattern**: Uses `make_config()` helper with documented isolation
- **Evidence**:
  ```rust
  fn make_config(workspace_home: &Path) -> Config {
      let mut config = Config::default();
      // ... other settings ...
      // Isolate Explore strand to prevent scanning real home directory
      // REQUIRED — see ADR-006 and Test Isolation Policy in CLAUDE.md
      config.strands.explore.workspace_root = workspace_home.to_path_buf();
      config.strands.explore.workspaces = Vec::new();
      config
  }
  ```

#### 3. tests/otlp_transport_seam_tests.rs
- **Status**: ✅ SAFE
- **Pattern**: Uses `IsolatedTest::new()` with verification assertions
- **Evidence**:
  ```rust
  impl IsolatedTest {
      fn new() -> Self {
          let home = tempfile::tempdir().expect("failed to create isolated HOME");
          let mut config = Config::default();
          config.workspace.home = home.path().to_path_buf();
          config.strands.explore.workspace_root = home.path().to_path_buf();
          config.strands.explore.workspaces.clear();
      }
      
      fn assert_explore_isolated(&self) {
          assert_eq!(self.config.strands.explore.workspace_root, self.config.workspace.home);
          assert!(self.config.strands.explore.workspaces.is_empty());
      }
  }
  ```

#### 4. tests/real_br_integration_tests.rs
- **Status**: ✅ SAFE
- **Pattern**: Explicitly sets Explore strand workspace_root
- **Evidence**:
  ```rust
  config.strands.explore.workspace_root = workspace.path().to_path_buf();
  ```

#### 5. tests/routing_integration.rs
- **Status**: ✅ SAFE
- **Pattern**: Explicit tempdir workspace_root configuration for ExploreConfig
- **Evidence**:
  ```rust
  let config = ExploreConfig {
      enabled: true,
      workspaces: vec![remote_workspace.clone()],
      workspace_root: scan_root.path().to_path_buf(),
      // ...
  };
  ```

---

### 🟡 NO EXPLORE ACCESS (Safe - Different Concern)

These tests don't use the Explore strand or spawn subprocesses (covered by existing policy).

#### tests/workspace_equality_tests.rs
- **Status**: ✅ SAFE (subprocess-only)
- **Pattern**: Spawns bf CLI via `Command::new("bf")`
- **Coverage**: Already protected by subprocess isolation policy (HOME env var)

---

## Fix Pattern Template

For all EXPLORE-CAPABLE tests, apply this pattern:

```rust
fn test_config(temp_dir: &Path) -> Config {
    let mut config = Config::default();
    
    // Existing workspace config
    config.workspace.home = temp_dir.to_path_buf();
    config.workspace.default = temp_dir.to_path_buf();
    
    // 🔒 CRITICAL: Isolate Explore strand to prevent scanning real home directory
    // REQUIRED — see ADR-006 and Test Isolation Policy in CLAUDE.md
    config.strands.explore.workspace_root = temp_dir.to_path_buf();
    config.strands.explore.workspaces = Vec::new();
    
    // ... other config ...
    
    config
}
```

**Key requirements**:
1. Set `config.strands.explore.workspace_root` to the test's temp directory
2. Clear `config.strands.explore.workspaces` to `Vec::new()` or known test workspaces
3. Add comment referencing ADR-006 and the Test Isolation Policy

---

## Unit Test Coverage

### src/strand/mod.rs
- **Status**: ✅ SAFE
- **Pattern**: Uses mock strands (`StubStrand`), no real Config or Worker construction
- **Coverage**: StrandRunner waterfall logic only, no Explore strand access

### src/worker/mod.rs
- **Status**: ⚠️ NEEDS REVIEW
- **Pattern**: Some unit tests construct `Worker::new()` with `valid_test_config()`
- **Finding**: Tests that set `config.strands.explore.enabled = false` are safe
- **Evidence**:
  ```rust
  config.strands.explore.enabled = false;
  config.strands.explore.workspace_root = workspace.path().to_path_buf();
  config.strands.explore.workspaces = Vec::new();
  ```
- **Status**: Already properly isolated for tests that use it

---

## Follow-up Implementation Bead Scope

The implementation bead should:

1. **Fix all 11 EXPLORE-CAPABLE test files** using the documented pattern
2. **Add a shared test helper** (e.g., `isolated_test_config()`) to centralize the pattern
3. **Update CLAUDE.md** to emphasize Explore strand isolation for in-process tests
4. **Add a CI check** (if not already present) to verify Explore isolation in test configs
5. **Document the fix pattern** in `docs/testing-isolation-patterns.md`

**Test files requiring fixes**:
- sigterm_heartbeat_cleanup.rs
- anthropic_routing_e2e_test.rs  
- anthropic_routing_verification.rs
- dispatch_model_routing_validation.rs
- heartbeat_state_during_dispatch.rs
- p2_integration_tests.rs
- p3_integration_tests.rs
- timeout_config_integration.rs
- timeout_config_integration_tests.rs
- upgrade_check_integration.rs
- test_mend_stale_assignee.rs

---

## References

- **ADR-006**: Test isolation policy and contamination incident postmortem
- **CLAUDE.md**: Test Isolation Policy section (in-process clause)
- **docs/testing-isolation-patterns.md**: Comprehensive isolation patterns (if exists, else create it)
- **Bead**: needle-efd3eb13 (this audit)

---

## Verification Checklist

For each fixed test file, verify:

- [ ] `config.strands.explore.workspace_root` is set to a tempdir
- [ ] `config.strands.explore.workspaces` is `Vec::new()` or known test values
- [ ] Comment references ADR-006 and Test Isolation Policy
- [ ] Test runs in isolation without scanning `$HOME`
- [ ] Test passes in CI (iad-ci workflow)

---

**Audit completed**: 2026-08-28  
**Next action**: Update bead needle-efd3eb13 with findings and create implementation bead
