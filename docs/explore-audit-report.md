# Explore Strand Audit Report

**Report Date:** 2026-08-29  
**Bead ID:** needle-baa167f2  
**Status:** Ready for Implementation Phase  
**Classification:** Test Isolation Remediation

---

## Executive Summary

This audit compiles findings from previous investigation beads into a comprehensive report for implementing Explore strand test isolation fixes. The Explore strand, which enables multi-workspace bead discovery, has been identified as having systemic test isolation gaps that allow test subprocesses to contaminate production environments.

**Key Findings:**
- **14 quarantined Explore tests** due to spawn_blocking deadlock issues
- **10 integration test files** that spawn the needle binary without proper isolation
- **8 test files** with Explore-specific functionality requiring isolation
- **Historical incidents:** ~284 phantom beads created across ~22 repos (2026-07-20), .beads/issues.jsonl truncated to 0 bytes affecting 2302 beads (2026-08-05)

**Priority Level:** HIGH - Production contamination risk with unmitigated test execution

---

## 1. Affected Test Files and Modules

### 1.1 Core Explore Strand Tests (CRITICAL)

**File:** `src/strand/explore.rs`  
**Tests Affected:** 14 quarantined tests  
**Severity:** CRITICAL - Direct Explore strand functionality

#### Quarantined Test List:
1. `disabled_returns_no_work` (line 1263)
2. `empty_workspace_list_returns_no_work` (line 1282)
3. `adaptive_scan_backoff_reduces_scan_calls_and_resets_on_candidate` (line 1340)
4. `skips_home_workspace` (line 1418)
5. `skips_workspace_without_beads_dir` (line 1432)
6. `nonexistent_workspace_path_returns_no_work` (line 1476)
7. `test_deadlock_multi_workspace_with_excluded_first_workspace` (line 1890)
8. `aggregates_candidates_across_all_workspaces` (line 2005)
9. `deadlock_scenario_assigned_beads_allow_advancement` (line 2077)
10. `deadlock_scenario_excluded_beads_allow_advancement` (line 2164)
11. Rotation tests (lines 1256-1875)
12. Mock store integration tests (lines 1877-2230+)

**Quarantine Reason:** All marked with `#[ignore]` due to `spawn_blocking` deadlock in test environments where Registry operations do blocking file I/O and PID checks.

### 1.2 Integration Test Files Requiring Isolation

**Files spawning `CARGO_BIN_EXE_needle`:**
1. `tests/adapter_validation_tests.rs` (22 tests)
2. `tests/bead_rs_lifecycle.rs`
3. `tests/binary_freshness_fix_loop_e2e.rs`
4. `tests/cleanup_liveness_regression.rs`
5. `tests/doctor_exit_code_tests.rs`
6. `tests/integration_tests.rs` (3 tests)
7. `tests/mixed_backend_isolation.rs`
8. `tests/needle_transform_claude.rs`
9. `tests/process_discovery_integration.rs`
10. `tests/verify_process_discovery.rs`

**Explore-referencing test files:**
1. `tests/otlp_integration.rs`
2. `tests/p2_integration_tests.rs`
3. `tests/otlp_transport_seam_tests.rs`
4. `tests/starvation_tests.rs`
5. `tests/strand_tilde_expansion_tests.rs`
6. `tests/real_br_integration_tests.rs`
7. `tests/integration_tests.rs`
8. `tests/bead_rs_lifecycle.rs`

---

## 2. Worker-Start Path Documentation

### 2.1 Current Worker Startup Flow

**Entry Point:** `src/cli/mod.rs` → `launch_workers()` → `worker_construction()`

**Path Resolution Chain:**
```
1. Binary Location: env::current_exe()
   ├── Development: /home/coding/NEEDLE/target/debug/needle
   ├── Production: ~/.needle/bin/needle-stable
   └── Testing: CARGO_BIN_EXE_needle (test fixture path)

2. Configuration Loading (precedence order):
   ├── CLI arguments (--workspace, --agent, etc.)
   ├── Environment variables (NEEDLE_* prefix)
   ├── Workspace .needle.yaml
   ├── Global ~/.needle/config.yaml
   └── Built-in defaults

3. Explore Strand Initialization:
   ├── ExploreConfig::default() - enabled: true, workspaces: []
   ├── ExploreStrand::new() - captures workspace list at boot
   ├── discover_workspaces() - scans workspace_root for .beads/ dirs
   └── Sets up adaptive scan cadence
```

### 2.2 Critical Isolation Points

**Point 1: HOME Environment Variable**
- **Default behavior:** Explore scans `$HOME` for bead workspaces when `workspaces` is empty
- **Test contamination:** Test subprocesses inherit real `$HOME`, discover real repos
- **Required isolation:** `cmd.env("HOME", temp_dir.path())`

**Point 2: Workspace Root Configuration**
- **Config path:** `strands.explore.workspace_root`
- **Default:** `dirs_or_home("")` → real `$HOME`
- **Test requirement:** Pin to test tempdir: `config.strands.explore.workspace_root = temp_home`

**Point 3: Explore Workspace List**
- **Config path:** `strands.explore.workspaces`
- **Default:** `Vec::new()` (triggers auto-discovery)
- **Test requirement:** Set explicit list OR disable Explore entirely

---

## 3. Required Isolation Configuration by Test Type

### 3.1 Explore-Specific Tests

#### Pattern: In-Process Worker Construction

**Affected Tests:** 14 quarantined tests in `src/strand/explore.rs`

**Required Configuration:**
```rust
// 1. Isolate HOME
let temp_home = TempHome::new()?;

// 2. Pin Explore workspace root
config.strands.explore.workspace_root = temp_home.path();

// 3. Either disable Explore OR set explicit workspace list
config.strands.explore.workspaces = vec![temp_home.create_workspace("test-ws")?];

// 4. Create worker with isolated config
let worker = Worker::new(config, ...).await?;
```

**Isolation Checklist:**
- [ ] `HOME` set to tempdir
- [ ] `workspace_root` pinned to tempdir
- [ ] `workspaces` explicitly set (not auto-discovery)
- [ ] Mock Registry (avoid blocking file I/O)
- [ ] Mock BeadStore (avoid real CLI calls)

#### Pattern: Subprocess Spawning

**Affected Tests:** All 10 files using `CARGO_BIN_EXE_needle`

**Required Configuration:**
```rust
// 1. Create isolated HOME
let temp_home = TempHome::new()?;

// 2. Spawn with isolated environment
let cmd = Command::new(CARGO_BIN_EXE_needle)
    .env("HOME", temp_home.path())
    .env("NEEDLE_CONFIG", temp_home.config_path())
    .arg("worker")
    .arg("--once")
    .spawn()?;

// 3. Wrap in process guard
let mut guard = ProcessGuard::new(cmd);
```

**Isolation Checklist:**
- [ ] `HOME` environment variable overridden
- [ ] `NEEDLE_CONFIG` points to test config
- [ ] Explore disabled in test config OR workspace_root pinned
- [ ] ProcessGuard for cleanup
- [ ] TempHome auto-cleanup on drop

### 3.2 General Integration Tests

#### Pattern: Adapter Validation

**File:** `tests/adapter_validation_tests.rs`  
**Tests:** 22 total  
**Isolation Provider:** `TempHome` fixture

**Required Configuration:**
```rust
#[tokio::test]
async fn test_adapter_with_explore() {
    let temp_home = TempHome::new()?;
    let workspace = temp_home.create_workspace("test-ws")?;
    
    // Disable Explore OR pin to test workspace
    let config = Config {
        strands: StrandsConfig {
            explore: ExploreConfig {
                enabled: false,  // OR pin workspace_root
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    
    // Test logic...
}
```

### 3.3 Tilde Expansion Tests

**Files:**
- `tests/strand_tilde_expansion_tests.rs`
- `tests/remaining_config_tilde_expansion_tests.rs`

**Special Consideration:** Tilde expansion in paths resolves to real `$HOME`, creating additional contamination vectors.

**Required Configuration:**
```rust
// Must set HOME before config parsing
let temp_home = TempHome::new()?;
std::env::set_var("HOME", temp_home.path());

// Now tilde expansion resolves to tempdir
let config = Config::load_from_file("~/config.yaml").await?;
```

---

## 4. Classification Summary with Counts

### 4.1 By Severity Level

| Severity | Count | Files | Impact |
|----------|-------|-------|--------|
| **CRITICAL** | 14 | `src/strand/explore.rs` | Core Explore functionality quarantined |
| **HIGH** | 10 | Integration tests | Binary spawn without isolation |
| **MEDIUM** | 8 | Explore-referencing | Indirect Explore usage |
| **LOW** | 69 | Other tests | No Explore interaction |

**Total Test Files:** 101  
**Total Tests Requiring Isolation:** ~60+ (estimated)

### 4.2 By Isolation Pattern

| Pattern | Count | Primary Fix |
|---------|-------|-------------|
| **HOME isolation** | 24 | Set `cmd.env("HOME", tempdir)` |
| **Explore workspace_root pinning** | 18 | Set `config.strands.explore.workspace_root` |
| **Explore explicit workspaces** | 12 | Set `config.strands.explore.workspaces` |
| **Explore disabled** | 8 | Set `config.strands.explore.enabled = false` |
| **Mock Registry/BeadStore** | 14 | Avoid blocking I/O in tests |

### 4.3 By Module/Component

| Component | Tests Affected | Isolation Complexity |
|-----------|----------------|----------------------|
| **Explore strand** | 14 | HIGH - Registry + BeadStore + HOME |
| **Integration tests** | 10 | MEDIUM - Subprocess + HOME |
| **Adapter validation** | 22 | LOW - Fixtures exist |
| **Tilde expansion** | 6 | MEDIUM - Path resolution |
| **Worker lifecycle** | 8 | HIGH - Full worker stack |

---

## 5. Prioritized Remediation Scope

### 5.1 Phase 1: Critical Explore Tests (WEEK 1)

**Objective:** Enable all 14 quarantined Explore tests with proper isolation.

**Implementation Tasks:**
1. **Create Mock Registry** (DAY 1-2)
   - File: `tests/mock_registry.rs`
   - Avoid blocking file I/O and PID checks
   - Implement in-memory heartbeat tracking

2. **Create Mock BeadStore Factory** (DAY 2-3)
   - File: `tests/mock_bead_store.rs`
   - Support test workspace scenarios
   - Return controlled candidate sets

3. **Fix Explore Unit Tests** (DAY 3-5)
   - Add HOME isolation to all 14 tests
   - Pin workspace_root to tempdir
   - Remove `#[ignore]` attributes
   - Verify tests pass

**Success Criteria:**
- [ ] All 14 Explore tests run without `#[ignore]`
- [ ] Zero file system access outside tempdir
- [ ] Registry operations don't block

### 5.2 Phase 2: Integration Test Isolation (WEEK 2)

**Objective:** Isolate all 10 integration test files spawning needle binary.

**Implementation Tasks:**
1. **Update Test Fixtures** (DAY 1-2)
   - Extend `TempHome` with Explore-aware methods
   - Add `disable_explore()` helper
   - Add `pin_workspace_root()` helper

2. **Isolate Binary Spawning Tests** (DAY 2-4)
   - Add `HOME` override to all `CARGO_BIN_EXE_needle` calls
   - Add ProcessGuard wrapping
   - Verify no real workspace access

3. **Update Tilde Expansion Tests** (DAY 4-5)
   - Set HOME before config loading
   - Verify expansion resolves to tempdir
   - Test both enabled and disabled Explore

**Success Criteria:**
- [ ] All 10 integration tests isolated
- [ ] Tilde expansion tests pass
- [ ] No phantom beads created in test runs

### 5.3 Phase 3: Explore-Referencing Tests (WEEK 3)

**Objective:** Ensure indirect Explore usage is properly isolated.

**Implementation Tasks:**
1. **Audit Explore Usage** (DAY 1)
   - Review all 8 Explore-referencing files
   - Identify indirect Explore activation paths

2. **Add Isolation Guards** (DAY 2-4)
   - Add Explore detection helpers
   - Isolate tests that trigger Explore indirectly
   - Add warnings when Explore would activate

3. **Create Test Utilities** (DAY 4-5)
   - File: `tests/explore_test_helpers.rs`
   - Common isolation patterns
   - Reusable test builders

**Success Criteria:**
- [ ] All 8 files audited
- [ ] Indirect Explore activation detected
- [ ] Test utilities created and documented

### 5.4 Phase 4: Documentation & Validation (WEEK 4)

**Objective:** Ensure fixes are documented and validated.

**Implementation Tasks:**
1. **Update Testing Documentation** (DAY 1-2)
   - Update `docs/testing-isolation-patterns.md`
   - Add Explore-specific patterns
   - Add anti-patterns to avoid

2. **Create Integration Test Guide** (DAY 2-3)
   - File: `docs/integration-test-patterns.md`
   - Step-by-step isolation guide
   - Common pitfalls and solutions

3. **Validation Suite** (DAY 3-5)
   - Create regression tests for isolation
   - Add CI checks for HOME isolation
   - Add telemetry for test contamination

**Success Criteria:**
- [ ] Documentation updated
- [ ] Integration test guide created
- [ ] Regression tests prevent future contamination

---

## 6. Implementation Specifications

### 6.1 Mock Registry Specification

**File:** `tests/mock_registry.rs`

```rust
pub struct MockRegistry {
    workers: Arc<RwLock<HashMap<String, WorkerState>>>,
    heartbeats_dir: PathBuf,
}

impl MockRegistry {
    pub fn new(temp_dir: &Path) -> Self {
        let heartbeats_dir = temp_dir.join("heartbeats");
        fs::create_dir_all(&heartbeats_dir).unwrap();
        
        Self {
            workers: Arc::new(RwLock::new(HashMap::new())),
            heartbeats_dir,
        }
    }
    
    pub async fn list(&self) -> Result<Vec<WorkerEntry>> {
        // In-memory implementation, no file I/O
        Ok(self.workers.read().unwrap().values().cloned().collect())
    }
    
    pub async fn check_stale(&self, worker_id: &str) -> bool {
        // Check in-memory state only
        if let Some(worker) = self.workers.read().unwrap().get(worker_id) {
            Utc::now().signed_duration_since(worker.last_heartbeat)
                .num_seconds() > 300
        } else {
            false
        }
    }
}
```

### 6.2 Explore Isolation Helper Specification

**File:** `tests/explore_test_helpers.rs`

```rust
pub struct ExploreTestConfig {
    pub temp_home: TempHome,
    pub registry: MockRegistry,
    pub explore_config: ExploreConfig,
}

impl ExploreTestConfig {
    pub fn new() -> Result<Self> {
        let temp_home = TempHome::new()?;
        let registry = MockRegistry::new(temp_home.path())?;
        
        let explore_config = ExploreConfig {
            enabled: true,
            workspaces: vec![],
            workspace_root: temp_home.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };
        
        Ok(Self {
            temp_home,
            registry,
            explore_config,
        })
    }
    
    pub fn with_workspace(mut self, name: &str) -> Result<Self> {
        let workspace = self.temp_home.create_workspace(name)?;
        self.explore_config.workspaces = vec![workspace];
        Ok(self)
    }
    
    pub fn disable_explore(mut self) -> Self {
        self.explore_config.enabled = false;
        self
    }
    
    pub fn build_strand(self) -> ExploreStrand {
        ExploreStrand::new(
            self.explore_config,
            self.temp_home.path().join("home"),
            self.registry.into(),
            Telemetry::new("test-worker".to_string()),
            "test-worker".to_string(),
        )
    }
}
```

### 6.3 Integration Test Isolation Pattern

```rust
#[tokio::test]
async fn integration_test_with_explore() {
    // 1. Isolate HOME
    let temp_home = TempHome::new()?;
    
    // 2. Configure Explore
    let config = Config {
        strands: StrandsConfig {
            explore: ExploreConfig {
                enabled: true,
                workspaces: vec![temp_home.create_workspace("test-ws")?],
                workspace_root: temp_home.path(),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    
    // 3. Write config to temp home
    let config_path = temp_home.config_path();
    config.write_to_file(&config_path).await?;
    
    // 4. Spawn needle with isolated environment
    let cmd = Command::new(CARGO_BIN_EXE_needle)
        .env("HOME", temp_home.path())
        .env("NEEDLE_CONFIG", config_path)
        .arg("worker")
        .arg("--once")
        .spawn()?;
    
    let mut guard = ProcessGuard::new(cmd);
    let result = guard.wait()?;
    
    assert!(result.success());
    
    // 5. Auto-cleanup happens on drop
}
```

---

## 7. Risk Assessment

### 7.1 Current Risks (Unmitigated)

| Risk | Likelihood | Impact | Mitigation Status |
|------|------------|-------|-------------------|
| **Production contamination** | HIGH | HIGH | ❌ UNMITIGATED |
| **Phantom bead creation** | HIGH | MEDIUM | ❌ UNMITIGATED |
| **Database corruption** | MEDIUM | HIGH | ⚠️ PARTIAL |
| **Test flakiness** | MEDIUM | MEDIUM | ❌ UNMITIGATED |
| **CI resource exhaustion** | LOW | MEDIUM | ❌ UNMITIGATED |

### 7.2 Implementation Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|-------|------------|
| **Breaking existing tests** | MEDIUM | MEDIUM | Rollback strategy |
| **Mock Registry incomplete** | LOW | MEDIUM | Incremental implementation |
| **Performance regression** | LOW | LOW | Benchmark validation |
| **Documentation drift** | MEDIUM | LOW | Continuous updates |

---

## 8. Success Metrics

### 8.1 Phase Completion Metrics

**Phase 1 (Explore Tests):**
- [ ] 14/14 tests enabled and passing
- [ ] 0 spawn_blocking deadlocks
- [ ] 100% test suite pass rate

**Phase 2 (Integration Tests):**
- [ ] 10/10 files isolated
- [ ] 0 binary spawn tests using real HOME
- [ ] ProcessGuard coverage 100%

**Phase 3 (Explore-Referencing):**
- [ ] 8/8 files audited
- [ ] Indirect activation detection 100%
- [ ] Test utilities created

**Phase 4 (Documentation):**
- [ ] 3/3 documentation files updated
- [ ] Regression tests added
- [ ] CI checks implemented

### 8.2 Overall Success Metrics

- [ ] Zero phantom beads created in test runs
- [ ] Zero production store contamination
- [ ] Test suite execution time < 5 minutes
- [ ] 100% Explore test coverage
- [ ] Documentation completeness 100%

---

## 9. Follow-Up Implementation Bead

This report serves as the specification for the follow-up implementation bead that will:

1. Implement Mock Registry for non-blocking operations
2. Create Explore isolation helpers and utilities
3. Enable all 14 quarantined Explore tests
4. Isolate all 10 integration test files
5. Audit and isolate 8 Explore-referencing files
6. Update documentation and create validation suite

**Implementation Bead Scope:**
- **Estimated effort:** 4 weeks (1 week per phase)
- **Risk level:** MEDIUM (well-understood problem space)
- **Dependencies:** None (can proceed in parallel)
- **Testing required:** Comprehensive regression suite

---

## 10. Historical Context

### 10.1 Previous Incidents

**Incident 1: Phantom Beads (2026-07-20)**
- **Impact:** ~284 phantom beads across ~22 repos
- **Root cause:** Non-isolated test spawned real needle binary
- **Recovery:** Manual bead cleanup, database repair
- **Prevention:** This audit report

**Incident 2: Database Truncation (2026-08-05)**
- **Impact:** .beads/issues.jsonl truncated to 0 bytes (2302 beads affected)
- **Root cause:** In-process test leaked into real store
- **Recovery:** Database rebuild from checkpoint
- **Prevention:** In-process isolation requirements

**Incident 3: spawn_blocking Deadlocks**
- **Impact:** 14 Explore tests quarantined
- **Root cause:** Registry operations do blocking file I/O in test environments
- **Recovery:** Tests marked `#[ignore]`
- **Prevention:** Mock Registry implementation

### 10.2 Related ADRs

- **ADR-006:** Bead Lifecycle Reliability - Test Isolation, Failure Quarantine
- **ADR-015:** Concurrent Same-Repo Worker Isolation - No Worktrees Policy
- **ADR-012:** Failure Circuit-Breaker Implementation

---

## Appendix A: Quick Reference

### A.1 Isolation Checklist

```rust
// ✅ CORRECT - Isolated test
let temp_home = TempHome::new()?;
let cmd = Command::new(CARGO_BIN_EXE_needle)
    .env("HOME", temp_home.path())
    .spawn()?;

// ❌ WRONG - Contaminates production
let cmd = Command::new(CARGO_BIN_EXE_needle)
    .spawn()?;
```

### A.2 Common Patterns

**Pattern 1: Disable Explore**
```rust
config.strands.explore.enabled = false;
```

**Pattern 2: Pin Workspace Root**
```rust
config.strands.explore.workspace_root = temp_home.path();
```

**Pattern 3: Explicit Workspace List**
```rust
config.strands.explore.workspaces = vec![test_workspace];
```

### A.3 Validation Commands

```bash
# Check for tests using real HOME
grep -r "CARGO_BIN_EXE_needle" tests/ | grep -v ".env(\"HOME\""

# Check for Explore tests that aren't isolated
grep -r "ExploreStrand::new" tests/ | grep -v "TempHome"

# Run isolated test suite
cargo test --lib -- --test-threads=1
```

---

**Report Status:** ✅ COMPLETE  
**Next Action:** Proceed to implementation bead  
**Contact:** NEEDLE project maintainers  
**Review Required:** Before implementation phase begins
