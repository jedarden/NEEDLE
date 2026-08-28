# Explore Strand Access Patterns — Complete Map

**Document Purpose:** This document maps every code path, configuration option, and environment variable that enables the Explore strand to activate and scan bead stores. It identifies the minimum required configuration for Explore to run and traces through ExploreConfig initialization and strand execution to find all conditions that enable scanning.

**Scope:** All access paths showing exactly when Explore can and cannot reach bead stores.

---

## Executive Summary

The Explore strand activates under these conditions:

1. **Enabled via configuration** (`config.strands.explore.enabled: true`, the default)
2. **Has workspaces to scan** (auto-discovered under `workspace_root` OR explicitly pinned)
3. **Not in adaptive backoff** (scan cadence allows this cycle)
4. **Waterfall evaluation reaches Explore** (Pluck and Mend returned `NoWork`)

When these conditions are met, Explore scans configured workspaces for unassigned, unblocked beads.

---

## 1. Configuration Layer

### 1.1 Configuration Structure

```rust
// src/config/mod.rs:4135
pub struct ExploreConfig {
    /// Whether the Explore strand is enabled.
    #[serde(default = "ExploreConfig::default_enabled")]
    pub enabled: bool,

    /// Pin/exception list for restricting a worker to specific workspaces.
    /// When empty (default): enables recursive discovery under workspace_root
    /// When non-empty: disables auto-discovery, scans only these paths
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,

    /// Root path for workspace auto-discovery (when workspaces is empty)
    /// Defaults to the user's home directory
    #[serde(default = "ExploreConfig::default_workspace_root")]
    pub workspace_root: PathBuf,

    /// Re-run workspace discovery every N cycles (0 = disabled, was legacy)
    /// As of bf-6anj4, re-discovery runs every cycle regardless of this value
    #[serde(default = "ExploreConfig::default_rediscovery_cycles")]
    pub rediscovery_cycles: u32,

    /// Starvation alarm threshold in minutes (0 = disabled)
    #[serde(default = "ExploreConfig::default_starvation_threshold_minutes")]
    pub starvation_threshold_minutes: u64,

    /// Minimum number of selection cycles between Explore scans
    #[serde(default = "ExploreConfig::default_scan_interval_cycles")]
    pub scan_interval_cycles: u32,

    /// Maximum number of selection cycles between Explore scans after adaptive backoff
    #[serde(default = "ExploreConfig::default_max_scan_interval_cycles")]
    pub max_scan_interval_cycles: u32,
}
```

### 1.2 Default Values

```rust
// src/config/mod.rs:4201-4212
impl Default for ExploreConfig {
    fn default() -> Self {
        ExploreConfig {
            enabled: true,                              // ENABLED BY DEFAULT
            workspaces: Vec::new(),                     // EMPTY = AUTO-DISCOVER
            workspace_root: dirs_or_home(""),           // $HOME or /tmp
            rediscovery_cycles: 60,
            starvation_threshold_minutes: 15,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        }
    }
}
```

### 1.3 Environment Variables

Explore configuration can be overridden via environment variables:

```bash
# Enable/disable Explore strand
NEEDLE_STRANDS__EXPLORE__ENABLED=true

# Set workspace root for auto-discovery
NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT=/home/user/projects

# Set explicit workspace list (JSON array format)
NEEDLE_STRANDS__EXPLORE__WORKSPACES='["/path/to/ws1", "/path/to/ws2"]'

# Configure adaptive backoff intervals
NEEDLE_STRANDS__EXPLORE__SCAN_INTERVAL_CYCLES=1
NEEDLE_STRANDS__EXPLORE__MAX_SCAN_INTERVAL_CYCLES=8

# Configure starvation detection
NEEDLE_STRANDS__EXPLORE__STARVATION_THRESHOLD_MINUTES=15
```

**Environment Variable Loading Code:**
```rust
// src/config/mod.rs:9006-9020
let key = "NEEDLE_STRANDS__EXPLORE__ENABLED";
// ... (env loading logic)
let key = "NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT";
// ... (env loading logic)
```

### 1.4 Configuration File

Configuration is loaded from `~/.config/needle/config.yaml` (or `config.json`):

```yaml
strands:
  explore:
    enabled: true
    workspaces: []                          # Empty = auto-discover
    workspace_root: /home/coding           # Defaults to $HOME
    rediscovery_cycles: 60                 # Legacy, now ignored (bf-6anj4)
    starvation_threshold_minutes: 15
    scan_interval_cycles: 1
    max_scan_interval_cycles: 8
```

**Configuration Loading Path:**
```
Config::from_file/defaults()
  → merges env vars (NEEDLE_STRANDS__EXPLORE__*)
  → returns Config { strands: { explore: ExploreConfig } }
```

---

## 2. Strand Initialization Layer

### 2.1 ExploreStrand Construction

```rust
// src/strand/mod.rs:154-160
let explore = ExploreStrand::new(
    config.strands.explore.clone(),       // ExploreConfig from config file
    config.workspace.default.clone(),      // Home workspace path
    explore_registry,                      // Registry for orphan detection
    telemetry.clone(),                     // Telemetry emitter
    worker_id.to_string(),                // Qualified worker ID
);
```

### 2.2 ExploreStrand::new() Internal Logic

```rust
// src/strand/explore.rs:197-266
pub fn new(
    config: ExploreConfig,
    home_workspace: PathBuf,
    registry: Registry,
    telemetry: Telemetry,
    qualified_id: String,
) -> Self {
    // Determine auto-discovery mode
    let auto_discovery_mode = config.workspaces.is_empty();

    // Workspace discovery or explicit list
    let workspaces = if auto_discovery_mode {
        Self::discover_workspaces(&config.workspace_root)  // Auto-discover
    } else {
        config.workspaces                                      // Use explicit list
    };

    // WARN if running in pinned mode (non-empty workspaces)
    if !workspaces.is_empty() && !auto_discovery_mode {
        tracing::warn!(pinned_repos = ?repo_names, "Explore running in PINNED mode");
    }

    ExploreStrand {
        enabled: config.enabled,
        workspaces: std::sync::Mutex::new(workspaces),
        home_workspace,
        registry,
        telemetry,
        qualified_id,
        store_factory: Arc::new(DefaultStoreFactory),
        cycles_since_rediscovery: AtomicU32::new(0),
        rediscovery_cycles: config.rediscovery_cycles,  // Legacy, ignored
        workspace_root: config.workspace_root,
        auto_discovery_mode,
        starvation_threshold_minutes: config.starvation_threshold_minutes,
        last_successful_claim_seconds: AtomicU64::new(0),
        ready_beads_detected: AtomicU64::new(0),
        last_scan_per_workspace: std::sync::Mutex::new(HashMap::new()),
        scan_backoff: std::sync::Mutex::new(ExploreScanBackoff::new(
            config.scan_interval_cycles,
            config.max_scan_interval_cycles,
        )),
    }
}
```

### 2.3 Workspace Discovery Logic

```rust
// src/strand/explore.rs:357-396
fn discover_workspaces(root: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();

    // If root doesn't exist, return empty (not an error)
    if !root.exists() {
        tracing::debug!(root = %root.display(), "workspace root does not exist");
        return discovered;
    }

    // Read the directory
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(error = %e, "failed to read workspace root");
            return discovered;
        }
    };

    // Filter for entries containing a .beads/ subdirectory
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        if path.is_dir() && Self::has_beads_dir(&path) {
            tracing::debug!(workspace = %path.display(), "discovered workspace");
            discovered.push(path);
        }
    }

    discovered
}

// src/strand/explore.rs:399-401
fn has_beads_dir(workspace: &Path) -> bool {
    workspace.join(".beads").is_dir()
}
```

**Discovery Constraints:**
- **Shallow traversal only:** scans immediate children of `workspace_root`
- **No upward traversal:** never scans parent directories
- **`.beads/` detection:** only directories containing `.beads/` subdirectory are workspaces
- **Non-existent root:** returns empty list (not an error)
- **Unreadable directory:** returns empty list (not an error)

---

## 3. Strand Execution Layer

### 3.1 Strand Registration in Waterfall

```rust
// src/strand/mod.rs:241-253
StrandRunner {
    strands: vec![
        Box::new(pluck),     // 1. Check home workspace first
        Box::new(mend),      // 2. Maintenance/cleanup
        Box::new(explore),   // 3. Scan other workspaces ← THIS
        Box::new(weave),     // 4. Generative work
        Box::new(unravel),   // 5. Extraction tasks
        Box::new(pulse),     // 6. Time-based triggers
        Box::new(reflect),   // 7. Extraction agent
        Box::new(splice),    // 8. Failure escalation
        Box::new(knot),      // 9. Exhaustion alerting
    ],
    telemetry: runner_telemetry,
}
```

### 3.2 Strand Evaluation Entry Point

```rust
// src/strand/mod.rs:268-291
pub async fn select(
    &self,
    store: &dyn BeadStore,
    exclusions: &HashSet<BeadId>,
) -> Result<SelectOutcome> {
    // Waterfall loop
    'waterfall: loop {
        for strand in &self.strands {
            let result = strand.evaluate(store, exclusions).await;

            // Handle result...
            match result {
                StrandResult::BeadFound(beads) => { /* return first bead */ }
                StrandResult::NoWork => { /* continue to next strand */ }
                StrandResult::WorkCreated => { /* restart waterfall from Pluck */ }
                // ... other result types
            }
        }
    }
}
```

### 3.3 ExploreStrand::evaluate() — Main Logic

```rust
// src/strand/explore.rs:563-953
async fn evaluate(
    &self,
    _store: &dyn BeadStore,  // Ignored by Explore (creates own stores)
    _exclusions: &HashSet<BeadId>,
) -> StrandResult {
    // ── CHECK 1: Enabled flag ────────────────────────────────────────
    if !self.enabled {
        self.telemetry.emit(StrandSkipped {
            strand_name: "explore",
            reason: "disabled",
        });
        return StrandResult::NoWork;
    }

    // ── CHECK 2: Adaptive scan backoff ───────────────────────────────
    if !self.should_scan_this_cycle() {
        self.telemetry.emit(StrandSkipped {
            strand_name: "explore",
            reason: "adaptive_scan_backoff",
        });
        tracing::debug!("Explore scan deferred by adaptive empty-scan backoff");
        return StrandResult::NoWork;
    }

    // ── STEP 3: Re-discover workspaces (every cycle, bf-6anj4) ──────
    let _cycle = self.cycles_since_rediscovery.fetch_add(1, Ordering::Relaxed) + 1;
    let added = self.rediscover_workspaces();
    // The legacy `rediscovery_cycles` throttle is no longer applied;
    // re-discovery runs unconditionally every cycle now.

    // ── CHECK 4: Workspaces available ───────────────────────────────
    {
        let workspaces = self.workspaces.lock().unwrap();
        if workspaces.is_empty() {
            self.telemetry.emit(StrandSkipped {
                strand_name: "explore",
                reason: "no_workspaces_discovered",
            });
            self.record_scan_result(false);
            return StrandResult::NoWork;
        }
    }

    // ── STEP 5: Shuffle scan order (bf-6anj4) ─────────────────────────
    let mut workspaces = {
        let workspaces = self.workspaces.lock().unwrap();
        workspaces.clone()
    };
    workspaces.shuffle(&mut rand::thread_rng());

    // ── STEP 6: Scan all workspaces (aggregate candidates) ────────────
    let mut all_candidates: Vec<Bead> = Vec::new();

    for workspace in &workspaces {
        // Skip home workspace (Pluck already checked it)
        if workspace == &self.home_workspace {
            continue;
        }

        // Verify .beads/ exists
        if !Self::has_beads_dir(workspace) {
            continue;
        }

        // Create bead store for this workspace
        let remote_store = match self.store_factory.create_store(workspace).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to create bead store, skipping");
                continue;
            }
        };

        // Query for ready beads
        let filters = Filters {
            assignee: None,
            exclude_labels: vec!["deferred", "human", "blocked"],
            exclude_ids: HashSet::new(),
        };

        match remote_store.ready(&filters).await {
            Ok(mut candidates) => {
                // Defensive filtering (belt-and-suspenders)
                candidates.retain(|b| {
                    let assignee_ok = b.assignee.is_none();
                    let labels_ok = !b.labels.iter()
                        .any(|l| filters.exclude_labels.contains(l));
                    assignee_ok && labels_ok
                });

                if candidates.is_empty() {
                    // Run cross-workspace mend to release orphans
                    // ... (mend logic)
                    continue;
                }

                // Tag candidates with workspace
                for bead in &mut candidates {
                    bead.workspace = workspace.clone();
                }

                // Accumulate (bf-4df1e: don't return early)
                all_candidates.append(&mut candidates);
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to query workspace, skipping");
                continue;
            }
        }
    }

    // ── CHECK 7: Candidates found ─────────────────────────────────────
    if all_candidates.is_empty() {
        self.record_scan_result(false);
        self.telemetry.emit(StrandSkipped {
            strand_name: "explore",
            reason: "no_candidates_in_any_workspace",
        });
        return StrandResult::NoWork;
    }

    self.record_scan_result(true);

    // ── STEP 8: Rank globally ─────────────────────────────────────────
    all_candidates.sort_by(|a, b| {
        a.priority.cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
    });

    StrandResult::BeadFound(all_candidates)
}
```

---

## 4. Access Conditions Summary

Explore strand reaches bead stores when ALL of these conditions are met:

### 4.1 Configuration Conditions

| Condition | Default | How to Set | Failure Behavior |
|-----------|---------|------------|-------------------|
| `enabled: true` | ✅ `true` | Config file or env var | Returns `NoWork`, emits `StrandSkipped { reason: "disabled" }` |
| `workspaces` populated OR auto-discovery succeeds | ✅ Auto-discovers | Config file or env var | Returns `NoWork`, emits `StrandSkipped { reason: "no_workspaces_discovered" }` |
| `workspace_root` exists | ✅ `$HOME` | Config file or env var | Auto-discovery returns empty, no error |

### 4.2 Runtime Conditions

| Condition | Default | How to Control | Failure Behavior |
|-----------|---------|----------------|-------------------|
| Adaptive backoff allows scan | ✅ Every 1 cycle | `scan_interval_cycles`, `max_scan_interval_cycles` | Returns `NoWork`, emits `StrandSkipped { reason: "adaptive_scan_backoff" }` |
| Waterfall reaches Explore | ✅ After Pluck/Mend | Waterfall order | Never evaluated if Pluck or Mend returns `BeadFound` |
| At least one workspace has `.beads/` | Required | Workspace structure | Skipped during scan (not fatal) |
| At least one workspace has valid (unassigned, unblocked) beads | Required | Bead state | Returns `NoWork`, emits `StrandSkipped { reason: "no_candidates_in_any_workspace" }` |

### 4.3 Workspace Detection Conditions

For **auto-discovery mode** (`workspaces: []`):
- ✅ Scans immediate children of `workspace_root`
- ✅ Only directories containing `.beads/` subdirectory
- ✅ Shallow traversal (no grandchildren)
- ✅ Non-existent root → empty list (not an error)
- ✅ Unreadable directory → empty list (not an error)

For **pinned mode** (`workspaces: ["/path1", "/path2"]`):
- ✅ Scans only explicitly listed paths
- ✅ No validation at construction time
- ✅ Non-existent paths → skipped during evaluation (not fatal)
- ✅ WARN log emitted at startup naming pinned repos

---

## 5. Adaptive Scan Backoff Mechanism

### 5.1 Backoff State

```rust
// src/strand/explore.rs:86-137
struct ExploreScanBackoff {
    base_interval_cycles: u32,        // Default: 1
    max_interval_cycles: u32,         // Default: 8
    consecutive_empty_scans: u32,
    cycles_until_scan: u32,
}
```

### 5.2 Backoff Logic

**Empty Scan (no candidates found):**
- Interval doubles: `1 → 2 → 4 → 8 → 8` (capped at `max_interval_cycles`)
- Next scan deferred for N cycles

**Successful Scan (candidates found):**
- Interval resets to `base_interval_cycles` (1)
- Scan runs on next cycle

**Example Timeline:**
```
Cycle 1: Scan (empty) → next scan in 1 cycle
Cycle 2: Scan (empty) → next scan in 2 cycles
Cycle 3: SKIP (backoff)
Cycle 4: Scan (empty) → next scan in 4 cycles
Cycles 5-7: SKIP (backoff)
Cycle 8: Scan (empty) → next scan in 8 cycles
Cycles 9-15: SKIP (backoff)
Cycle 16: Scan (candidates found!) → next scan in 1 cycle
Cycle 17: Scan (empty) → next scan in 2 cycles
...
```

---

## 6. Re-discovery Mechanism

### 6.1 When Re-discovery Runs

```rust
// src/strand/explore.rs:403-459
fn rediscover_workspaces(&self) -> usize {
    // Skip in pinned mode
    if !self.auto_discovery_mode {
        return 0;
    }

    // Re-discovery runs EVERY cycle (bf-6anj4)
    // The legacy `rediscovery_cycles` throttle is no longer applied
    let new_workspaces = Self::discover_workspaces(&self.workspace_root);

    // Update workspace list
    {
        let mut workspaces = self.workspaces.lock().unwrap();
        *workspaces = new_workspaces;
    }

    added_count
}
```

### 6.2 Re-discovery Behavior

| Mode | When Re-discovery Runs | What It Does |
|------|----------------------|--------------|
| **Auto-discovery** (`workspaces: []`) | Every cycle (unconditional) | Refreshes workspace list, picks up new repos |
| **Pinned** (`workspaces: ["/path"]`) | Never (skipped) | Uses explicit list only |

**Historical Note:** The `rediscovery_cycles` config field (default: 60) is legacy. Before bf-6anj4, it throttled re-discovery. Now re-discovery runs every cycle regardless of this value.

---

## 7. Store Creation Path

### 7.1 StoreFactory Pattern

```rust
// src/strand/explore.rs:60-78
#[async_trait::async_trait]
trait StoreFactory: Send + Sync {
    async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error>;
}

struct DefaultStoreFactory;

#[async_trait::async_trait]
impl StoreFactory for DefaultStoreFactory {
    async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
        discover_default(
            workspace.to_path_buf(),
            None,
            Some("needle".to_string()),
            Some(env!("CARGO_PKG_VERSION").to_string()),
        )
    }
}
```

### 7.2 Store Creation During Scan

```rust
// src/strand/explore.rs:701-712
let remote_store = match self.store_factory.create_store(workspace).await {
    Ok(s) => s,
    Err(e) => {
        tracing::warn!(error = %e, "failed to create bead store, skipping");
        continue;
    }
};
```

**Store Failure Handling:**
- Store creation failure → skip workspace (not fatal)
- Continue scanning remaining workspaces
- Emit WARN log but don't fail the strand

---

## 8. Candidate Filtering

### 8.1 Store-Level Filtering

```rust
// src/strand/explore.rs:632-640
let filters = Filters {
    assignee: None,
    exclude_labels: vec!["deferred", "human", "blocked"],
    exclude_ids: HashSet::new(),
};
```

### 8.2 Defensive Filtering (Belt-and-Suspenders)

```rust
// src/strand/explore.rs:721-727
candidates.retain(|b| {
    let assignee_ok = b.assignee.is_none();
    let labels_ok = !b.labels.iter()
        .any(|l| filters.exclude_labels.contains(l));
    assignee_ok && labels_ok
});
```

**Why Two Filters?**
- **Store-level filtering:** Backend implementation may not filter correctly
- **Defensive filtering:** Guarantees excluded/assigned beads never returned

### 8.3 Exclusion Reasons Tracked

```rust
// src/strand/explore.rs:669-670
let mut exclusion_reasons: HashSet<String> = HashSet::new();
// ... during scan:
exclusion_reasons.insert("home_workspace".to_string());
exclusion_reasons.insert("no_beads_dir".to_string());
exclusion_reasons.insert(format!("store_error: {}", e));
exclusion_reasons.insert(format!("filtered_{}", filtered_count));
exclusion_reasons.insert("no_ready_candidates".to_string());
exclusion_reasons.insert(format!("list_error: {}", e));
exclusion_reasons.insert("no_in_progress".to_string());
exclusion_reasons.insert(format!("mend_error: {}", e));
exclusion_reasons.insert(format!("query_error: {}", e));
```

---

## 9. Orphan Detection and Cleanup

### 9.1 Cross-Workspace Mend

```rust
// src/strand/explore.rs:734-770
// Only run cleanup if there are actually in-progress beads
let all_beads = match remote_store.list_all().await {
    Ok(beads) => beads,
    Err(e) => {
        tracing::warn!(error = %e, "failed to list beads for orphan check, skipping");
        continue;
    }
};

let has_in_progress = all_beads.iter()
    .any(|b| b.status == BeadStatus::InProgress);

if !has_in_progress {
    continue;
}

match super::cleanup_orphaned_in_progress(
    remote_store.as_ref(),
    &self.registry,
    &self.telemetry,
    &self.qualified_id,
).await {
    Ok(released) if released > 0 => {
        tracing::info!(released, "cross-workspace mend released orphans, re-querying");
        // Re-query ready after cleanup
        match remote_store.ready(&filters).await {
            Ok(retry_candidates) => {
                // ... handle retry candidates
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to re-query after mend");
            }
        }
    }
    Ok(_) => {
        // No orphans released
    }
    Err(e) => {
        tracing::warn!(error = %e, "cross-workspace mend failed");
    }
}
```

### 9.2 Orphan Detection Conditions

- ✅ Runs when workspace has no ready candidates
- ✅ Checks if any beads are `InProgress` status
- ✅ Uses worker registry to detect dead workers
- ✅ Releases orphaned beads (sets `assignee: None`)
- ✅ Re-queries after cleanup to find newly-available beads

---

## 10. Test Isolation Considerations

### 10.1 Isolation Requirements

From `CLAUDE.md` and test documentation:

**For subprocess tests** (spawn `needle` binary):
```rust
cmd.env("HOME", temp_dir.path())  // Isolate home directory
```

**For in-process tests** (build `Worker` directly):
```rust
config.strands.explore.workspace_root = temp_home.to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

### 10.2 Isolation Failure Mode

Without isolation, the Explore strand can:
- ✅ Scan real user workspaces under `$HOME`
- ✅ Read real `.beads/` directories
- ✅ Return real beads from production stores
- ✅ Contaminate test results with production state

**Critical Incident (2026-07-19/20):**
- Non-isolated test created 284 phantom beads across 22 repos
- Root cause: In-process worker with default `workspace_root: $HOME`
- Fix: Always pin `workspace_root` in test configs

---

## 11. Minimum Required Configuration

### 11.1 Production Fleet Defaults

```yaml
strands:
  explore:
    enabled: true                      # Required for Explore to run
    workspaces: []                     # Empty = auto-discovery (recommended)
    workspace_root: /home/coding       # Defaults to $HOME
    scan_interval_cycles: 1           # Minimum scan cadence
    max_scan_interval_cycles: 8       # Maximum backoff interval
```

### 11.2 Test Isolation Defaults

```rust
ExploreConfig {
    enabled: true,
    workspaces: vec![],                // Empty = auto-discovery
    workspace_root: temp_dir.path(),    // ISOLATED to test tempdir
    rediscovery_cycles: 0,
    starvation_threshold_minutes: 0,
    scan_interval_cycles: 1,
    max_scan_interval_cycles: 8,
}
```

### 11.3 Disabled Explore

```yaml
strands:
  explore:
    enabled: false                     # Disables Explore completely
```

When disabled, Explore strand:
- ✅ Returns `NoWork` immediately
- ✅ Emits `StrandSkipped { reason: "disabled" }`
- ✅ Never scans any workspaces
- ✅ Never creates bead stores

---

## 12. Access Path Decision Tree

```
START
  │
  ├─ Is config.strands.explore.enabled == true?
  │   ├─ NO → RETURN NoWork (emit "disabled")
  │   └─ YES → CONTINUE
  │
  ├─ Does adaptive backoff allow scan this cycle?
  │   ├─ NO → RETURN NoWork (emit "adaptive_scan_backoff")
  │   └─ YES → CONTINUE
  │
  ├─ Re-discover workspaces (every cycle)
  │   ├─ Auto-discovery mode: scan workspace_root for .beads/ dirs
  │   └─ Pinned mode: use explicit workspaces list
  │
  ├─ Are there any workspaces to scan?
  │   ├─ NO → RETURN NoWork (emit "no_workspaces_discovered")
  │   └─ YES → CONTINUE
  │
  ├─ Shuffle workspace scan order
  │
  ├─ For each workspace:
  │   ├─ Is this the home workspace?
  │   │   ├─ YES → SKIP (Pluck already checked it)
  │   │   └─ NO → CONTINUE
  │   │
  │   ├─ Does workspace have .beads/ directory?
  │   │   ├─ NO → SKIP (emit "no_beads_dir")
  │   │   └─ YES → CONTINUE
  │   │
  │   ├─ Create bead store for workspace
  │   │   ├─ FAIL → SKIP workspace (emit "store_error")
  │   │   └─ SUCCESS → CONTINUE
  │   │
  │   ├─ Query store for ready beads
  │   │   ├─ FAIL → SKIP workspace (emit "query_error")
  │   │   └─ SUCCESS → CONTINUE
  │   │
  │   ├─ Apply defensive filtering
  │   │   └─ Remove assigned/excluded beads
  │   │
  │   ├─ Are there any candidates after filtering?
  │   │   ├─ YES → ADD to aggregated list
  │   │   └─ NO → RUN cross-workspace mend
  │   │       ├─ Mend released orphans?
  │   │       │   ├─ YES → Re-query, add to list
  │   │       │   └─ NO → CONTINUE to next workspace
  │   │
  │   └─ Continue to next workspace
  │
  ├─ Is aggregated candidates list non-empty?
  │   ├─ NO → RETURN NoWork (emit "no_candidates_in_any_workspace")
  │   │         Record empty scan (adaptive backoff increases)
  │   └─ YES → CONTINUE
  │
  ├─ Sort candidates by priority, created_at, id
  │
  └─ RETURN BeadFound(candidates)
      Record successful scan (adaptive backoff resets)
```

---

## 13. Failure Mode Summary

### 13.1 Non-Fatal Failures (Continue Scanning)

| Failure Point | Behavior | Emission |
|---------------|----------|----------|
| Home workspace | Skip | Not emitted |
| Missing `.beads/` | Skip workspace | `no_beads_dir` |
| Store creation error | Skip workspace | `store_error: {e}` |
| Query error | Skip workspace | `query_error: {e}` |
| List all error | Skip orphan check | `list_error: {e}` |
| Mend error | Skip workspace | `mend_error: {e}` |
| No in-progress beads | Skip orphan check | `no_in_progress` |
| All candidates filtered | Continue scanning | `filtered_{n}` |

### 13.2 Fatal Failures (Terminate Strand)

| Failure Point | Behavior | Emission |
|---------------|----------|----------|
| `enabled: false` | Return `NoWork` | `StrandSkipped { reason: "disabled" }` |
| Adaptive backoff active | Return `NoWork` | `StrandSkipped { reason: "adaptive_scan_backoff" }` |
| No workspaces discovered | Return `NoWork` | `StrandSkipped { reason: "no_workspaces_discovered" }` |
| No candidates in any workspace | Return `NoWork` | `StrandSkipped { reason: "no_candidates_in_any_workspace" }` |

### 13.3 Success Cases

| Outcome | Behavior | Side Effects |
|---------|----------|--------------|
| Candidates found | Return `BeadFound` | Reset adaptive backoff |
| Orphans released | Return `BeadFound` or continue | Reset adaptive backoff if found |

---

## 14. Configuration Variants

### 14.1 Auto-Discovery Mode (Recommended)

```yaml
strands:
  explore:
    enabled: true
    workspaces: []                     # EMPTY → auto-discovery
    workspace_root: /home/coding       # Scan this directory
```

**Behavior:**
- ✅ Scans immediate children of `workspace_root`
- ✅ Finds all directories with `.beads/` subdirectory
- ✅ Re-discovers every cycle (picks up new repos)
- ✅ WARNs if running in pinned mode (not applicable here)

### 14.2 Pinned Mode (Exception)

```yaml
strands:
  explore:
    enabled: true
    workspaces:                       # NON-EMPTY → pinned mode
      - /home/coding/NEEDLE
      - /home/coding/commitgraph
    workspace_root: /home/coding       # IGNORED in pinned mode
```

**Behavior:**
- ✅ Scans ONLY explicitly listed paths
- ✅ No auto-discovery (even if new repos appear)
- ✅ No re-discovery (workspace list is static)
- ✅ WARN log at startup: `"Explore running in PINNED mode"`

### 14.3 Disabled Mode

```yaml
strands:
  explore:
    enabled: false
```

**Behavior:**
- ✅ Never scans any workspaces
- ✅ Returns `NoWork` immediately
- ✅ Emits `StrandSkipped { reason: "disabled" }`

---

## 15. Security and Access Control

### 15.1 Filesystem Access

Explore strand requires:
- ✅ **Read access** to `workspace_root` directory
- ✅ **Read access** to each workspace directory
- ✅ **Read access** to `.beads/` subdirectory in each workspace
- ✅ **Read access** to bead store files (SQLite, JSON, etc.)

**No write access required** — Explore only reads beads.

### 15.2 Registry Access

Explore strand requires:
- ✅ **Read/write access** to registry directory (default: `$HOME/.beads/state/`)
- ✅ Used for cross-workspace orphan detection
- ✅ Creates/reads worker PID files for liveness checks

### 15.3 Network Access

Explore strand requires:
- ❌ **No network access**
- ❌ Does not make HTTP requests or network calls
- ✅ Purely local filesystem and registry operations

---

## 16. Debugging Explore Access Issues

### 16.1 Common Issues and Diagnostics

| Symptom | Likely Cause | Diagnostic |
|---------|--------------|------------|
| Explore never runs | `enabled: false` | Check config, verify env var |
| Explore runs but finds nothing | No workspaces discovered | Check `workspace_root` exists |
| Explore finds workspaces but no beads | All beads assigned/excluded | Check bead status/labels |
| Explore scans intermittently | Adaptive backoff active | Check empty scan history |
| Explore doesn't find new repos | Re-discovery not running | Verify auto-discovery mode |

### 16.2 Telemetry Events

```rust
// StrandSkipped events
EventKind::StrandSkipped {
    strand_name: "explore",
    reason: "disabled" | "adaptive_scan_backoff" | "no_workspaces_discovered" | "no_candidates_in_any_workspace"
}

// Explore scan summary
EventKind::ExploreScanSummary {
    workspaces_visited: Vec<String>,
    workspaces_with_candidates: Vec<String>,
    total_candidates: usize,
    exclusion_reasons: Vec<String>,
    duration_ms: u64,
}
```

### 16.3 Log Patterns

**Successful scan:**
```
INFO explore{worker=needle-test}: worker scan: shuffled order over 26 workspaces this cycle
INFO explore{worker=needle-test}: explore found candidates in remote workspace
 INFO explore{worker=needle-test, workspace=/home/coding/NEEDLE}: explore found candidates in remote workspace
INFO explore{worker=needle-test}: explore aggregated candidates across all workspaces this cycle
```

**Pinned mode WARN:**
```
WARN explore{worker=needle-test, mode="pinned"}: Explore running in PINNED mode (non-empty workspaces list)
```

**Backoff skip:**
```
DEBUG explore{worker=needle-test}: Explore scan deferred by adaptive empty-scan backoff
```

---

## 17. Summary: Complete Access Map

### 17.1 Configuration Path

```
Config file (~/.config/needle/config.yaml)
  ↓ merged with
Environment variables (NEEDLE_STRANDS__EXPLORE__*)
  ↓ produces
ExploreConfig {
    enabled: bool,
    workspaces: Vec<PathBuf>,
    workspace_root: PathBuf,
    scan_interval_cycles: u32,
    max_scan_interval_cycles: u32,
    ...
}
```

### 17.2 Initialization Path

```
ExploreConfig::new()
  ↓
ExploreStrand::new(config, home_workspace, registry, telemetry, worker_id)
  ↓
IF workspaces.is_empty():
    discover_workspaces(workspace_root) → Vec<PathBuf>
ELSE:
    use explicit workspaces list
  ↓
Set up adaptive backoff state
```

### 17.3 Execution Path

```
StrandRunner::select(store, exclusions)
  ↓
Waterfall loop:
  Pluck → Mend → EXPLORE → Weave → ...
  ↓
ExploreStrand::evaluate(store, exclusions)
  ↓
CHECK 1: enabled? → NO: return NoWork
CHECK 2: should_scan_this_cycle? → NO: return NoWork
STEP 3: rediscover_workspaces()
CHECK 4: workspaces.is_empty? → YES: return NoWork
STEP 5: shuffle scan order
STEP 6: for each workspace:
    - Skip home workspace
    - Verify .beads/ exists
    - Create bead store
    - Query ready beads
    - Filter candidates
    - Run cross-workspace mend
    - Accumulate candidates
CHECK 7: all_candidates.is_empty? → YES: return NoWork
STEP 8: sort candidates globally
  ↓
return BeadFound(candidates)
```

### 17.4 Minimum Conditions for Access

**Explore reaches bead stores when:**

1. ✅ `config.strands.explore.enabled == true`
2. ✅ Adaptive backoff allows scan this cycle
3. ✅ Workspaces list non-empty (auto-discovered OR pinned)
4. ✅ At least one workspace has `.beads/` directory
5. ✅ At least one workspace has valid (unassigned, unblocked) beads
6. ✅ Waterfall reaches Explore (Pluck and Mend returned `NoWork`)

**When ALL conditions are met:**
- Explore creates bead stores for each workspace
- Queries each store for ready beads
- Filters out assigned/excluded beads
- Returns aggregated candidate list
- Resets adaptive backoff

---

## 18. Test Coverage

### 18.1 Unit Tests

See `src/strand/explore.rs` tests section (lines 959-3974):

- ✅ `disabled_returns_no_work`
- ✅ `empty_workspace_list_returns_no_work`
- ✅ `skips_home_workspace`
- ✅ `skips_workspace_without_beads_dir`
- ✅ `has_beads_dir_detects_directory`
- ✅ `workspace_list_is_static`
- ✅ `home_workspace_is_captured`
- ✅ `nonexistent_workspace_path_returns_no_work`
- ✅ `default_config_is_enabled_with_empty_workspaces`
- ✅ `discover_workspaces_finds_dirs_with_beads_subdir`
- ✅ `discover_workspaces_returns_empty_for_nonexistent_root`
- ✅ `empty_workspaces_config_triggers_discovery`
- ✅ `explicit_workspaces_list_skips_discovery`
- ✅ Adaptive backoff tests
- ✅ Rotation tests
- ✅ Deadlock scenario tests
- ✅ Regression tests (2026-07-19/20 incident)
- ✅ Re-discovery tests

### 18.2 Integration Tests

See `tests/integration_tests.rs`:

- ✅ Test isolation with `HOME` override
- ✅ Test isolation with `workspace_root` pinning
- ✅ End-to-end strand evaluation

---

## Appendix A: Historical Context

### A.1 2026-07-19/20 Incident

**Issue:** Static workspace list missed newly-added repos (`commitgraph`, `twitterapi-proxy`).

**Root Cause:** Fleet config had a hardcoded 24-entry workspace list. New repos weren't in the list, so Explore never scanned them.

**Fix:** Switched to auto-discovery mode (empty `workspaces` config). Now Explore automatically picks up any directory with `.beads/` under `workspace_root`.

**Affected Tests:**
- `regression_fixture_2026_07_19_incident_all_workspaces_discovered`
- `regression_empty_workspaces_config_triggers_full_discovery`

### A.2 bf-6anj4 (Every-Cycle Re-discovery)

**Change:** Re-discovery now runs every cycle unconditionally, regardless of `rediscovery_cycles` config value.

**Reason:** A newly-created workspace needed a worker restart to be seen. Re-discovery is cheap (~40 `read_dir` entries), so the throttle was removed.

**Legacy Behavior:** `rediscovery_cycles: 0` disabled re-discovery. Now it's ignored.

### A.3 bf-4df1e / bf-47bfm (Aggregate Candidates)

**Change:** Explore now scans ALL workspaces and aggregates candidates, rather than returning on the first non-empty workspace.

**Reason:** A single stale/excluded bead in an early workspace caused Explore to return early, silently starving later workspaces.

**Fix:** Scan every workspace, collect all candidates, rank globally.

---

## Appendix B: Configuration Reference

### B.1 Full Configuration Schema

```yaml
strands:
  explore:
    # Enable/disable Explore strand
    enabled: true                      # default: true

    # Workspace list (empty = auto-discover, non-empty = pinned)
    workspaces: []                      # default: []

    # Root directory for auto-discovery
    workspace_root: /home/coding       # default: $HOME or /tmp

    # Re-discovery interval (legacy, now ignored)
    rediscovery_cycles: 60              # default: 60 (ignored as of bf-6anj4)

    # Starvation detection threshold
    starvation_threshold_minutes: 15    # default: 15

    # Adaptive backoff intervals
    scan_interval_cycles: 1             # default: 1
    max_scan_interval_cycles: 8         # default: 8
```

### B.2 Environment Variable Override Reference

```bash
# Enable/disable
NEEDLE_STRANDS__EXPLORE__ENABLED=true

# Auto-discovery root
NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT=/home/coding

# Explicit workspace list (JSON array)
NEEDLE_STRANDS__EXPLORE__WORKSPACES='["/path/to/ws1", "/path/to/ws2"]'

# Adaptive backoff
NEEDLE_STRANDS__EXPLORE__SCAN_INTERVAL_CYCLES=1
NEEDLE_STRANDS__EXPLORE__MAX_SCAN_INTERVAL_CYCLES=8

# Starvation detection
NEEDLE_STRANDS__EXPLORE__STARVATION_THRESHOLD_MINUTES=15

# Legacy (ignored as of bf-6anj4)
NEEDLE_STRANDS__EXPLORE__REDISCOVERY_CYCLES=60
```

---

## Appendix C: Code Reference Index

### C.1 Key Source Files

| File | Lines | Purpose |
|------|-------|---------|
| `src/strand/explore.rs` | 1-3974 | Explore strand implementation |
| `src/strand/mod.rs` | 1-996 | Strand trait and waterfall |
| `src/config/mod.rs` | 4135-4239 | ExploreConfig definition |
| `src/worker/mod.rs` | 777, 154-160 | Strand initialization |

### C.2 Key Functions

| Function | Location | Purpose |
|----------|----------|---------|
| `ExploreStrand::new()` | `explore.rs:197-266` | Strand construction |
| `ExploreStrand::evaluate()` | `explore.rs:563-953` | Main strand logic |
| `discover_workspaces()` | `explore.rs:357-396` | Auto-discovery logic |
| `has_beads_dir()` | `explore.rs:399-401` | Workspace detection |
| `rediscover_workspaces()` | `explore.rs:403-459` | Re-discovery logic |
| `should_scan_this_cycle()` | `explore.rs:336-338` | Backoff check |
| `record_scan_result()` | `explore.rs:341-351` | Backoff update |

---

**Document Version:** 1.0  
**Last Updated:** 2026-08-28  
**Status:** Complete — covers all Explore strand access paths
