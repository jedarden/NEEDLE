# Explore Strand Access Patterns

## Overview

This document maps exactly how the Explore strand activates and scans bead stores. It traces through code paths, configurations, and environment variables to identify the minimum required configuration for Explore to run and when it can and cannot reach bead stores.

**Last Updated:** 2026-08-17  
**Strand:** Explore (multi-workspace bead discovery)  
**Source:** `src/strand/explore.rs`, `src/config/mod.rs`

---

## Quick Reference: Minimum Required Configuration

The **absolute minimum** for Explore to scan anything:

```yaml
strands:
  explore:
    enabled: true  # defaults to true
    # workspaces: []  # defaults to [] (auto-discovery)
    # workspace_root: ~/  # defaults to $HOME
```

With defaults applied, Explore will:
1. Scan `$HOME` for directories containing `.beads/` subdirectories
2. Create bead stores for each discovered workspace
3. Query for unassigned, unexcluded beads (no `deferred`, `human`, `blocked` labels)

**Environment variable:** `$HOME` must be set (defaults to `/tmp` if unset)

---

## Access Decision Tree

```
ExploreStrand::evaluate() called
│
├─ Is self.enabled == false?
│  └─ YES → Return NoWork (telemetry: StrandSkipped { reason: "disabled" })
│
├─ Should skip this cycle (adaptive backoff)?
│  └─ YES → Return NoWork (telemetry: StrandSkipped { reason: "adaptive_scan_backoff" })
│
├─ Run rediscover_workspaces()
│  └─ Auto-discovery mode only: re-scan workspace_root for new workspaces
│
├─ Is workspaces list empty after discovery?
│  └─ YES → Return NoWork (telemetry: StrandSkipped { reason: "no_workspaces_discovered" })
│
├─ For each workspace in shuffled order:
│  │
│  ├─ Is workspace == home_workspace?
│  │  └─ YES → Skip (Pluck already checked it)
│  │
│  ├─ Does workspace have .beads/ subdirectory?
│  │  └─ NO → Skip (no bead store)
│  │
│  ├─ Create bead store via discover_default()
│  │  └─ FAIL → Skip workspace, continue
│  │
│  ├─ Query store.ready(filters: exclude assignee, exclude labels)
│  │  └─ FAIL → Skip workspace, continue
│  │
│  ├─ Are candidates empty?
│  │  └─ YES → Run cross-workspace mend → re-query → continue
│  │
│  └─ Found candidates → Aggregate, continue scanning
│
├─ After all workspaces: Any candidates found?
│  └─ NO → Return NoWork (telemetry: StrandSkipped { reason: "no_candidates_in_any_workspace" })
│
└─ YES → Rank globally (priority ASC, created_at ASC, id ASC)
        → Return StrandResult::BeadFound(all_candidates)
```

---

## Configuration Structure

### ExploreConfig Fields

Located in `src/config/mod.rs:3389-3455`:

```rust
pub struct ExploreConfig {
    /// Master enable/disable switch
    #[serde(default = "ExploreConfig::default_enabled")]
    pub enabled: bool,  // default: true

    /// Explicit workspace list (PIN mode) or empty (AUTO mode)
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,  // default: [] (AUTO mode)

    /// Root for auto-discovery (only used when workspaces is empty)
    #[serde(default = "ExploreConfig::default_workspace_root")]
    pub workspace_root: PathBuf,  // default: $HOME

    /// Re-discovery interval (deprecated - now runs every cycle)
    #[serde(default = "ExploreConfig::default_rediscovery_cycles")]
    pub rediscovery_cycles: u32,  // default: 60

    /// Starvation alarm threshold
    #[serde(default = "ExploreConfig::default_starvation_threshold_minutes")]
    pub starvation_threshold_minutes: u64,  // default: 15

    /// Base scan interval (adaptive backoff)
    #[serde(default = "ExploreConfig::default_scan_interval_cycles")]
    pub scan_interval_cycles: u32,  // default: 1

    /// Max scan interval (adaptive backoff ceiling)
    #[serde(default = "ExploreConfig::default_max_scan_interval_cycles")]
    pub max_scan_interval_cycles: u32,  // default: 8
}
```

### Default Value Functions

Located in `src/config/mod.rs:3469-3493`:

```rust
impl ExploreConfig {
    fn default_enabled() -> bool {
        true  // Explore is ENABLED by default
    }

    fn default_workspace_root() -> PathBuf {
        dirs_or_home("")  // Reads $HOME env var
    }

    fn default_rediscovery_cycles() -> u32 {
        60  // Deprecated - re-discovery now runs every cycle
    }

    fn default_starvation_threshold_minutes() -> u64 {
        15
    }

    fn default_scan_interval_cycles() -> u32 {
        1  // Scan every cycle by default
    }

    fn default_max_scan_interval_cycles() -> u32 {
        8  // Cap at 8 cycles between scans after adaptive backoff
    }
}
```

### Environment Variable Dependencies

The `dirs_or_home()` function (line 6630-6636):

```rust
fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}
```

**Critical dependency:** If `$HOME` is unset, `workspace_root` defaults to `/tmp`.

---

## Initialization Path

### Worker Bootstrap

1. **Entry point:** `Worker::new()` in `src/worker/mod.rs:458`
   ```rust
   let strands = StrandRunner::from_config(&config, &qualified_id, strand_registry, telemetry.clone());
   ```

2. **StrandRunner construction:** `src/strand/mod.rs:99-254`
   ```rust
   pub fn from_config(
       config: &Config,
       worker_id: &str,
       registry: Registry,
       telemetry: Telemetry,
   ) -> Self {
       // ... other strands ...
       
       let explore = ExploreStrand::new(
           config.strands.explore.clone(),
           config.workspace.default.clone(),  // home workspace path
           explore_registry,
           telemetry.clone(),
           worker_id.to_string(),
       );
       
       StrandRunner {
           strands: vec![
               Box::new(pluck),
               Box::new(mend),
               Box::new(explore),  // 3rd in waterfall
               // ... other strands ...
           ],
           telemetry: runner_telemetry,
       }
   }
   ```

### ExploreStrand Construction

Located in `src/strand/explore.rs:197-266`:

```rust
pub fn new(
    config: ExploreConfig,
    home_workspace: PathBuf,
    registry: Registry,
    telemetry: Telemetry,
    qualified_id: String,
) -> Self {
    // 1. Determine mode: AUTO (empty workspaces) or PIN (explicit workspaces)
    let auto_discovery_mode = config.workspaces.is_empty();

    // 2. Initialize workspace list
    let workspaces = if auto_discovery_mode {
        Self::discover_workspaces(&config.workspace_root)  // AUTO mode
    } else {
        config.workspaces  // PIN mode
    };

    // 3. Warn if running in PIN mode
    if !workspaces.is_empty() && !auto_discovery_mode {
        tracing::warn!(
            worker = %qualified_id,
            mode = "pinned",
            workspaces_count = workspaces.len(),
            pinned_repos = ?repo_names,
            "Explore running in PINNED mode (non-empty workspaces list). \
             Auto-discovery is disabled; only the listed workspaces will be scanned. \
             This is an exception mechanism — the fleet default is empty workspaces \
             (recursive discovery under workspace_root). Verify this is intentional."
        );
    }

    // 4. Log auto-discovery mode
    if auto_discovery_mode {
        tracing::info!(
            worker = %qualified_id,
            configured_rediscovery_cycles = config.rediscovery_cycles,
            "Explore auto-discovery: workspaces re-discovered every cycle \
             (rediscovery_cycles throttle no longer applied)"
        );
    }

    // 5. Construct strand
    ExploreStrand {
        enabled: config.enabled,
        workspaces: std::sync::Mutex::new(workspaces),
        home_workspace,
        registry,
        telemetry,
        qualified_id,
        store_factory: Arc::new(DefaultStoreFactory),
        cycles_since_rediscovery: AtomicU32::new(0),
        rediscovery_cycles: config.rediscovery_cycles,  // deprecated, logged only
        workspace_root: config.workspace_root,
        auto_discovery_mode,
        starvation_threshold_minutes: config.starvation_threshold_minutes,
        last_successful_claim_seconds: AtomicU64::new(0),
        ready_beads_detected: AtomicU64::new(0),
        last_scan_per_workspace: Mutex::new(HashMap::new()),
        scan_backoff: Mutex::new(ExploreScanBackoff::new(
            config.scan_interval_cycles,
            config.max_scan_interval_cycles,
        )),
    }
}
```

---

## Execution Path: evaluate()

### Entry Point

Called by `StrandRunner::select()` in `src/strand/mod.rs:268-562`:

```rust
pub async fn select(
    &self,
    store: &dyn BeadStore,  // Pluck's store (ignored by Explore)
    exclusions: &HashSet<BeadId>,  // race-lost beads
) -> Result<SelectOutcome> {
    // ... waterfall loop ...
    for strand in &self.strands {
        let result = strand.evaluate(store, exclusions).await;
        // ... handle result ...
    }
}
```

### evaluate() Implementation

Located in `src/strand/explore.rs:563-924`:

#### Phase 1: Early Exit Checks

```rust
async fn evaluate(
    &self,
    _store: &dyn BeadStore,  // Pluck's store (ignored by Explore)
    _exclusions: &HashSet<BeadId>,
) -> StrandResult {
    // 1. Master disable switch
    if !self.enabled {
        let _ = self.telemetry.emit(EventKind::StrandSkipped {
            strand_name: "explore".to_string(),
            reason: "disabled".to_string(),
        });
        return StrandResult::NoWork;
    }

    // 2. Adaptive backoff skip
    if !self.should_scan_this_cycle() {
        let _ = self.telemetry.emit(EventKind::StrandSkipped {
            strand_name: "explore".to_string(),
            reason: "adaptive_scan_backoff".to_string(),
        });
        tracing::debug!(
            worker = %self.qualified_id,
            "Explore scan deferred by adaptive empty-scan backoff"
        );
        return StrandResult::NoWork;
    }

    // 3. Re-discover workspaces (every cycle as of bf-6anj4)
    let _cycle = self.cycles_since_rediscovery.fetch_add(1, Ordering::Relaxed) + 1;
    let added = self.rediscover_workspaces();
    if added > 0 {
        tracing::info!(
            worker = %self.qualified_id,
            added,
            "workspace re-discovery found new workspaces"
        );
    }

    // 4. Empty workspaces check
    {
        let workspaces = self.workspaces.lock().unwrap();
        if workspaces.is_empty() {
            let _ = self.telemetry.emit(EventKind::StrandSkipped {
                strand_name: "explore".to_string(),
                reason: "no_workspaces_discovered".to_string(),
            });
            self.record_scan_result(false);
            return StrandResult::NoWork;
        }
    }
```

#### Phase 2: Workspace Scanning

```rust
    // 5. Initialize filters
    let filters = Filters {
        assignee: None,  // unassigned only
        exclude_labels: vec![
            "deferred".to_string(),
            "human".to_string(),
            "blocked".to_string(),
        ],
        exclude_ids: HashSet::new(),
    };

    // 6. Shuffle workspace order (bf-6anj4)
    let mut workspaces = {
        let workspaces = self.workspaces.lock().unwrap();
        workspaces.clone()
    };
    {
        use rand::seq::SliceRandom;
        workspaces.shuffle(&mut rand::thread_rng());
    }
    let total_workspaces = workspaces.len();

    // 7. Scan all workspaces
    let mut all_candidates: Vec<Bead> = Vec::new();
    let mut workspaces_visited: Vec<String> = Vec::new();
    let mut workspaces_with_candidates: Vec<String> = Vec::new();
    let mut total_candidates = 0usize;
    let mut exclusion_reasons: HashSet<String> = HashSet::new();

    for workspace in &workspaces {
        // Track visit
        let workspace_str = workspace.display().to_string();
        workspaces_visited.push(workspace_str.clone());

        // 7a. Skip home workspace
        if workspace == &self.home_workspace {
            tracing::debug!(workspace = %workspace.display(), "skipping home workspace");
            exclusion_reasons.insert("home_workspace".to_string());
            continue;
        }

        // 7b. Check .beads/ directory exists
        if !Self::has_beads_dir(workspace) {
            tracing::debug!(workspace = %workspace.display(), "no .beads/ directory, skipping");
            exclusion_reasons.insert("no_beads_dir".to_string());
            continue;
        }

        // 7c. Create bead store
        let remote_store = match self.store_factory.create_store(workspace).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    workspace = %workspace.display(),
                    error = %e,
                    "failed to create bead store for workspace, skipping"
                );
                exclusion_reasons.insert(format!("store_error: {}", e));
                continue;
            }
        };

        // 7d. Query for ready beads
        match remote_store.ready(&filters).await {
            Ok(mut candidates) => {
                // 7e. Defensive filtering (assignee + labels)
                let before_count = candidates.len();
                candidates.retain(|b| {
                    let assignee_ok = b.assignee.is_none();
                    let labels_ok = !b.labels.iter()
                        .any(|l| filters.exclude_labels.contains(l));
                    assignee_ok && labels_ok
                });
                let filtered_count = before_count - candidates.len();

                if filtered_count > 0 {
                    exclusion_reasons.insert(format!("filtered_{}", filtered_count));
                }

                // 7f. Handle empty candidates
                if candidates.is_empty() {
                    tracing::debug!(
                        workspace = %workspace.display(),
                        "no ready candidates, running cross-workspace mend"
                    );
                    exclusion_reasons.insert("no_ready_candidates".to_string());

                    // Run cross-workspace mend to release orphaned in-progress beads
                    match cleanup_orphaned_in_progress(
                        remote_store.as_ref(),
                        &self.registry,
                        &self.telemetry,
                        &self.qualified_id,
                    ).await {
                        Ok(released) if released > 0 => {
                            tracing::info!(
                                workspace = %workspace.display(),
                                released,
                                "cross-workspace mend released orphans, re-querying"
                            );

                            // Re-query after cleanup
                            match remote_store.ready(&filters).await {
                                Ok(mut retry_candidates) => {
                                    // Apply defensive filtering again
                                    let retry_before = retry_candidates.len();
                                    retry_candidates.retain(|b| {
                                        let assignee_ok = b.assignee.is_none();
                                        let labels_ok = !b.labels.iter()
                                            .any(|l| filters.exclude_labels.contains(l));
                                        assignee_ok && labels_ok
                                    });
                                    let retry_filtered = retry_before - retry_candidates.len();

                                    if retry_filtered > 0 {
                                        exclusion_reasons.insert(format!("retry_filtered_{}", retry_filtered));
                                    }

                                    if !retry_candidates.is_empty() {
                                        // Tag with workspace
                                        for bead in &mut retry_candidates {
                                            bead.workspace = workspace.clone();
                                        }

                                        workspaces_with_candidates.push(workspace_str.clone());
                                        total_candidates += retry_candidates.len();

                                        tracing::info!(
                                            workspace = %workspace.display(),
                                            candidates = retry_candidates.len(),
                                            "explore found candidates in remote workspace after cross-workspace mend"
                                        );

                                        // Accumulate and continue (bf-4df1e)
                                        all_candidates.append(&mut retry_candidates);
                                    } else {
                                        tracing::info!(
                                            workspace = %workspace.display(),
                                            released,
                                            "cross-workspace mend released orphans but re-query found no candidates (beads may not pass filters), continuing to next workspace"
                                        );
                                        exclusion_reasons.insert("orphans_released_no_candidates".to_string());
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        workspace = %workspace.display(),
                                        error = %e,
                                        "failed to re-query workspace after cross-workspace mend, skipping"
                                    );
                                    exclusion_reasons.insert(format!("requery_error: {}", e));
                                }
                            }
                        }
                        Ok(_) => {
                            // No orphans released, workspace is truly empty
                            tracing::debug!(
                                workspace = %workspace.display(),
                                "cross-workspace mend found no orphans"
                            );
                            exclusion_reasons.insert("no_orphans".to_string());
                        }
                        Err(e) => {
                            tracing::warn!(
                                workspace = %workspace.display(),
                                error = %e,
                                "cross-workspace mend failed, skipping workspace"
                            );
                            exclusion_reasons.insert(format!("mend_error: {}", e));
                        }
                    }

                    continue;  // Next workspace
                }

                // 7g. Found candidates - tag and accumulate
                for bead in &mut candidates {
                    bead.workspace = workspace.clone();
                }

                workspaces_with_candidates.push(workspace_str.clone());
                total_candidates += candidates.len();

                tracing::info!(
                    workspace = %workspace.display(),
                    candidates = candidates.len(),
                    "explore found candidates in remote workspace"
                );

                // Accumulate and continue (bf-4df1e / bf-47bfm)
                all_candidates.append(&mut candidates);
            }
            Err(e) => {
                tracing::warn!(
                    workspace = %workspace.display(),
                    error = %e,
                    "failed to query workspace, skipping"
                );
                exclusion_reasons.insert(format!("query_error: {}", e));
                continue;
            }
        }
    }
```

#### Phase 3: Result Compilation

```rust
    // 8. Emit scan summary telemetry
    let duration_ms = scan_start.elapsed().as_millis() as u64;
    let _ = self.telemetry.emit(EventKind::ExploreScanSummary {
        workspaces_visited,
        workspaces_with_candidates,
        total_candidates,
        exclusion_reasons: exclusion_reasons.into_iter().collect(),
        duration_ms,
    });

    // 9. No candidates found
    if all_candidates.is_empty() {
        self.record_scan_result(false);
        let _ = self.telemetry.emit(EventKind::StrandSkipped {
            strand_name: "explore".to_string(),
            reason: "no_candidates_in_any_workspace".to_string(),
        });
        return StrandResult::NoWork;
    }

    // 10. Found candidates - record and rank
    self.record_scan_result(true);

    // Global ranking: priority ASC, created_at ASC, id ASC
    all_candidates.sort_by(|a, b| {
        a.priority.cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
    });

    tracing::info!(
        worker = %self.qualified_id,
        candidates = all_candidates.len(),
        workspaces = total_workspaces,
        "explore aggregated candidates across all workspaces this cycle"
    );

    StrandResult::BeadFound(all_candidates)
}
```

---

## Helper Functions

### Workspace Discovery

Located in `src/strand/explore.rs:353-396`:

```rust
fn discover_workspaces(root: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();

    // Root doesn't exist → return empty (not an error)
    if !root.exists() {
        tracing::debug!(root = %root.display(), "workspace root does not exist, no workspaces discovered");
        return discovered;
    }

    // Read directory
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(root = %root.display(), error = %e, "failed to read workspace root");
            return discovered;
        }
    };

    // Filter for directories containing .beads/
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

    tracing::debug!(
        root = %root.display(),
        count = discovered.len(),
        "workspace discovery complete"
    );

    discovered
}
```

### Beads Directory Check

Located in `src/strand/explore.rs:398-401`:

```rust
fn has_beads_dir(workspace: &Path) -> bool {
    workspace.join(".beads").is_dir()
}
```

**Critical check:** A workspace must have a `.beads/` subdirectory to be scanned.

### Re-discovery

Located in `src/strand/explore.rs:403-459`:

```rust
fn rediscover_workspaces(&self) -> usize {
    // Skip in PIN mode (explicit workspaces list)
    if !self.auto_discovery_mode {
        tracing::debug!(
            worker = %self.qualified_id,
            "skipping workspace re-discovery: running in pinned mode (explicit workspaces list)"
        );
        return 0;
    }

    let previous_count = {
        let workspaces = self.workspaces.lock().unwrap();
        workspaces.len()
    };

    // Re-scan workspace_root
    let new_workspaces = Self::discover_workspaces(&self.workspace_root);
    let new_count = new_workspaces.len();

    // Update workspace list
    {
        let mut workspaces = self.workspaces.lock().unwrap();
        *workspaces = new_workspaces;
    }

    let added_count = new_count.saturating_sub(previous_count);

    if added_count > 0 {
        tracing::info!(
            worker = %self.qualified_id,
            previous_count,
            new_count,
            added_count,
            "workspace re-discovery found new workspaces"
        );
    }

    added_count
}
```

**Key behavior:** Re-discovery only runs in AUTO mode (empty workspaces config).

### Adaptive Backoff

Located in `src/strand/explore.rs:86-137`:

```rust
struct ExploreScanBackoff {
    base_interval_cycles: u32,
    max_interval_cycles: u32,
    consecutive_empty_scans: u32,
    cycles_until_scan: u32,
}

impl ExploreScanBackoff {
    fn effective_interval_cycles(&self) -> u32 {
        let multiplier = 1u32
            .checked_shl(self.consecutive_empty_scans.min(31))
            .unwrap_or(u32::MAX);
        self.base_interval_cycles
            .saturating_mul(multiplier)
            .min(self.max_interval_cycles)
    }

    fn should_scan(&mut self) -> bool {
        if self.cycles_until_scan == 0 {
            true
        } else {
            self.cycles_until_scan -= 1;
            false
        }
    }

    fn record_scan(&mut self, found_candidate: bool) {
        if found_candidate {
            self.consecutive_empty_scans = 0;
            self.cycles_until_scan = self.base_interval_cycles.saturating_sub(1);
            return;
        }

        self.consecutive_empty_scans = self.consecutive_empty_scans.saturating_add(1);
        self.cycles_until_scan = self.effective_interval_cycles().saturating_sub(1);
    }
}
```

**Behavior:** Empty scans exponentially increase the interval (1→2→4→8→8...) until capped at `max_scan_interval_cycles`. Finding a candidate resets to base interval.

---

## Modes of Operation

### AUTO Mode (Recommended Default)

**Configuration:**
```yaml
strands:
  explore:
    enabled: true
    workspaces: []  # Empty → auto-discovery
    workspace_root: ~/  # or any path
```

**Behavior:**
1. Scans `workspace_root` for directories containing `.beads/` subdirectories
2. Re-discovers workspaces every cycle (picks up new stores without restart)
3. Shuffles workspace order each cycle (de-herds workers)
4. Emits WARN if a worker switches to PIN mode (non-empty workspaces)

**Use case:** Fleet-wide default - automatically picks up new workspaces.

### PIN Mode (Exception Mechanism)

**Configuration:**
```yaml
strands:
  explore:
    enabled: true
    workspaces:
      - /home/coding/repo1
      - /home/coding/repo2
    workspace_root: ~/  # Ignored in PIN mode
```

**Behavior:**
1. Scans **only** the explicitly listed workspaces
2. Never runs re-discovery (workspace list is static)
3. Emits WARN at startup listing the pinned repos
4. Shuffles the explicit list each cycle

**Use case:** Restrict a specific worker to a fixed repo set (e.g., dedicated worker for high-priority workspace).

---

## When Explore Cannot Reach Bead Stores

Explore will **fail to scan** if **any** of these conditions are true:

### 1. Master Disable Switch
```yaml
strands:
  explore:
    enabled: false
```
**Result:** `StrandSkipped { reason: "disabled" }`

### 2. Adaptive Backoff Skip
After consecutive empty scans, Explore skips cycles to reduce load.
**Result:** `StrandSkipped { reason: "adaptive_scan_backoff" }`

**Reset:** Finding a candidate resets the interval to base.

### 3. No Workspaces Discovered (AUTO Mode)
- `workspace_root` doesn't exist
- `workspace_root` is unreadable (permissions)
- No directories under `workspace_root` contain `.beads/` subdirectories

**Result:** `StrandSkipped { reason: "no_workspaces_discovered" }`

### 4. Empty Workspace List (PIN Mode)
If `workspaces` is explicitly set to empty (not defaulted):
```yaml
strands:
  explore:
    workspaces: []  # Explicit empty vector
```
**Result:** `StrandSkipped { reason: "no_workspaces_discovered" }`

### 5. No Candidates in Any Workspace
All workspaces scanned but:
- All beads are assigned (`assignee != None`)
- All beads have excluded labels (`deferred`, `human`, `blocked`)
- All workspaces have empty ready queues

**Result:** `StrandSkipped { reason: "no_candidates_in_any_workspace" }`

### 6. Per-Workspace Failures
Each workspace is skipped if:
- **No `.beads/` directory:** `has_beads_dir() == false`
- **Store creation fails:** `discover_default()` returns error
- **Query fails:** `store.ready()` returns error

**Result:** Workspace skipped with warning, continues to next workspace.

---

## Bead Store Creation

### Factory Pattern

Located in `src/strand/explore.rs:56-78`:

```rust
#[async_trait]
trait StoreFactory: Send + Sync {
    async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error>;
}

struct DefaultStoreFactory;

#[async_trait]
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

### discover_default()

This is the **only gateway** to bead stores for Explore. It:
1. Detects the backend type (bead-rs vs bf) from `.beads/config.json` or `.beads/config.yaml`
2. Creates the appropriate store implementation
3. Returns a trait object (`Arc<dyn BeadStore>`)

**Backend detection:** Located in `src/bead_store/mod.rs` via `detect_bead_backend()`.

---

## Telemetry Events

Explore emits the following telemetry events:

### StrandSkipped
Emitted when Explore is blocked from scanning:
- `"disabled"` - Master disable switch
- `"adaptive_scan_backoff"` - Deferred by empty-scan backoff
- `"no_workspaces_discovered"` - No workspaces to scan
- `"no_candidates_in_any_workspace"` - All workspaces empty or filtered

### ExploreScanSummary
Emitted after each complete scan (even if empty):
```rust
EventKind::ExploreScanSummary {
    workspaces_visited: Vec<String>,        // All workspaces checked
    workspaces_with_candidates: Vec<String>, // Workspaces with unassigned beads
    total_candidates: usize,                  // Total beads found (before exclusion)
    exclusion_reasons: Vec<String>,           // Reasons for skipping workspaces
    duration_ms: u64,                        // Scan duration
}
```

### StrandEvaluated
Emitted when Explore returns a result:
- `"bead_found"` - Candidates returned
- `"skipped({reason})"` - Skipped (see StrandSkipped reasons)

---

## Testing Isolation

### Critical Test Requirements

**Subprocess tests** (spawn `needle` binary via `Command::new(CARGO_BIN_EXE_needle)`):
```rust
cmd.env("HOME", temp_dir.path())  // Required - prevents leaking into real user environment
```

**In-process tests** (build `Worker` directly):
```rust
config.strands.explore.workspace_root = temp_home.to_path_buf();  // Required - prevents scanning real $HOME
config.strands.explore.workspaces = Vec::new();  // Pin to test directories
```

### Why Isolation Matters

The 2026-08-05 contamination incident:
- Test built `Worker` in-process
- Test isolated `workspace.default` and `workspace.home`
- **Test did NOT isolate** `strands.explore.workspace_root`
- Orphaned binary scanned real `$HOME` (bead-forge store)
- Created 284 phantom beads under `echo-test-test-worker` across 22 repos
- Truncated `.beads/issues.jsonl` to 0 bytes (2302 beads, recovered from git)

**Lesson:** Explore defaults to scanning `$HOME` - always isolate in tests.

---

## Summary: Complete Access Map

### Required for Explore to Scan Anything

1. **Configuration:**
   - `strands.explore.enabled: true` (default)
   - `strands.explore.workspaces: []` (default - AUTO mode)
   - `strands.explore.workspace_root: <valid_path>` (default: `$HOME`)

2. **Environment:**
   - `$HOME` set (defaults to `/tmp` if unset)

3. **Filesystem:**
   - At least one directory under `workspace_root` containing `.beads/` subdirectory
   - `.beads/` directory is readable

4. **Bead Store:**
   - Valid backend detected (`.beads/config.json` or `.beads/config.yaml`)
   - `discover_default()` succeeds

5. **Runtime:**
   - Not blocked by adaptive backoff (or backoff cycle has elapsed)
   - Worker hasn't disabled the strand

### When Explore Reaches Bead Stores

Explore will **successfully query** a bead store if:

1. Workspace has `.beads/` subdirectory
2. `discover_default()` creates a store successfully
3. `store.ready(filters)` executes without error
4. At least one bead passes filters:
   - `assignee == None` (unassigned)
   - No labels in `["deferred", "human", "blocked"]`

### When Explore Cannot Reach Bead Stores

Explore will **fail to query** if **any** of:

1. `enabled == false` (master disable)
2. Adaptive backoff defers the scan
3. No workspaces discovered (empty list after discovery)
4. Per-workspace failures:
   - No `.beads/` directory
   - Store creation fails
   - Query fails

### Configuration Paths

**Fleet default (AUTO mode):**
```yaml
strands:
  explore:
    # enabled: true (default)
    # workspaces: [] (default)
    # workspace_root: ~/ (default via $HOME)
```

**Exception (PIN mode):**
```yaml
strands:
  explore:
    enabled: true
    workspaces:
      - /path/to/repo1
      - /path/to/repo2
    # workspace_root is ignored in PIN mode
```

**Disabled:**
```yaml
strands:
  explore:
    enabled: false
```

---

## References

- **Explore strand:** `src/strand/explore.rs:1-2197`
- **Configuration:** `src/config/mod.rs:3389-3493`
- **Worker bootstrap:** `src/worker/mod.rs:458`
- **Strand runner:** `src/strand/mod.rs:99-254`
- **Bead store factory:** `src/strand/explore.rs:56-78`
- **Isolation incident:** 2026-08-05 contamination (284 phantom beads)

---

**Document Status:** Complete - maps all access paths, configurations, and environment variables enabling Explore strand activation and bead store scanning.
