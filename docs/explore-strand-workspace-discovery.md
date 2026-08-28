# Explore Strand: Workspace Discovery and Scanning

## Overview

The Explore strand discovers claimable beads across multiple workspaces when the home workspace has no work (Pluck returned `NoWork`) and maintenance is clean (Mend returned `NoWork`). It provides fleet-wide bead discovery by scanning configured workspaces and aggregating candidates.

**Key Design Principles:**
- **No upward traversal** — Only scans configured paths
- **Static workspace list** — Captured at boot, refreshed periodically
- **No permanent relocation** — Workers return home after processing one bead

---

## Configuration

### ExploreConfig Structure

```rust
pub struct ExploreConfig {
    /// Whether the Explore strand is enabled (default: true)
    pub enabled: bool,

    /// Pin/exception list for restricting worker to specific workspaces
    /// When empty: enables recursive discovery under workspace_root
    /// When non-empty: disables auto-discovery, scans only these paths
    pub workspaces: Vec<PathBuf>,

    /// Root path for workspace auto-discovery (default: $HOME)
    pub workspace_root: PathBuf,

    /// Re-discover workspaces every N cycles (default: 60)
    pub rediscovery_cycles: u32,

    /// Starvation alarm threshold in minutes (default: 15, 0 = disabled)
    pub starvation_threshold_minutes: u64,

    /// Minimum selection cycles between Explore scans (default: 1)
    pub scan_interval_cycles: u32,

    /// Maximum cycles between scans after adaptive backoff (default: 8)
    pub max_scan_interval_cycles: u32,
}
```

### Minimum Required Configuration

**Explore is enabled by default** with sensible defaults:

```yaml
explore:
  enabled: true
  workspaces: []                 # Empty = auto-discovery mode
  workspace_root: "$HOME"        # Resolved from HOME env var
```

The **minimum viable config** for Explore to scan workspaces:

```yaml
# .needle.yaml
explore:
  enabled: true                  # Must be true
  workspaces: []                 # Empty triggers auto-discovery
  workspace_root: "/home/coding" # Any directory containing bead workspaces
```

**No other fields are required.** All other configuration uses defaults.

---

## Workspace Discovery Modes

### 1. Auto-Discovery Mode (DEFAULT — RECOMMENDED)

**Configuration:** `workspaces: []` (empty vector)

**Behavior:**
- Recursively scans immediate children of `workspace_root`
- Discovers all directories containing a `.beads/` subdirectory
- Re-runs discovery **every selection cycle** (as of bf-6anj4)
- Automatically picks up new workspaces without worker restart

**Example:**

```yaml
explore:
  enabled: true
  workspaces: []                 # Empty = auto-discovery
  workspace_root: "/home/coding" # Scan all repos in home directory
```

This will discover:
- `/home/coding/NEEDLE` (has `.beads/`)
- `/home/coding/SEAM` (has `.beads/`)
- `/home/coding/commitgraph` (has `.beads/`)
- But NOT `/home/coding/scratch` (no `.beads/`)

**This is the intended default for the fleet.** New workspaces are automatically discovered without configuration changes.

### 2. Pinned Mode (EXCEPTION)

**Configuration:** `workspaces: ["/path1", "/path2", ...]` (non-empty)

**Behavior:**
- **Disables auto-discovery** completely
- Scans **only** the explicitly listed paths
- Ignores `workspace_root` (never used)
- Skips workspace re-discovery (even if `rediscovery_cycles` is set)
- Emits a **WARN log at startup** naming the pinned repos

**Example:**

```yaml
explore:
  enabled: true
  workspaces:                    # Explicit list only
    - /home/coding/NEEDLE
    - /home/coding/SEAM
  # workspace_root is ignored when workspaces is non-empty
```

**This is an exception mechanism.** Use it only when:
- A specific worker must be restricted to a fixed repo set
- A dedicated worker is needed for a high-priority workspace
- Auto-discovery must be bypassed for operational reasons

The WARN log at startup ensures operators immediately recognize when a worker is running in restricted mode.

---

## Workspace Discovery Algorithm

### `discover_workspaces(root: &Path) -> Vec<PathBuf>`

**Implementation:**

```rust
fn discover_workspaces(root: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();

    // If root doesn't exist, return empty (not an error)
    if !root.exists() {
        tracing::debug!(root = %root.display(), "workspace root does not exist");
        return discovered;
    }

    // Read the directory; non-existent/unreadable dirs return empty
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(root = %root.display(), error = %e, "failed to read workspace root");
            return discovered;
        }
    };

    // Filter for entries containing a `.beads/` subdirectory
    for entry in entries {
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        let path = entry.path();

        if path.is_dir() && Self::has_beads_dir(&path) {
            tracing::debug!(workspace = %path.display(), "discovered workspace");
            discovered.push(path);
        }
    }

    discovered
}
```

**Key Properties:**
- **Shallow scan** — Only immediate children of `workspace_root`
- **`.beads/` detection** — A workspace is any directory with `.beads/` subdirectory
- **Graceful degradation** — Non-existent roots return empty (not an error)
- **Permission-safe** — Unreadable directories are skipped (not failed)

### Workspace Root Resolution

**Default:** `$HOME` (from `HOME` environment variable)

**Fallback:** `/tmp` if `HOME` is not set

**Configuration override:** Set `workspace_root` explicitly in `.needle.yaml`

```rust
fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}
```

---

## Scanning Behavior

### Selection Cycle Flow

Every selection cycle, the Explore strand:

1. **Check if enabled** — If disabled, return `NoWork`
2. **Check adaptive backoff** — Skip scan if backing off from empty scans
3. **Re-discover workspaces** — Refresh the workspace list (every cycle in auto-discovery mode)
4. **Check empty workspaces** — If no workspaces discovered, return `NoWork`
5. **Shuffle workspace order** — Randomize scan order to de-herd workers
6. **Scan all workspaces** — Visit each workspace, accumulate candidates
7. **Rank globally** — Sort by priority, created_at, id
8. **Return aggregated results** — Single `BeadFound` with all candidates

### Workspace Scan Order

**Historical behavior (superseded):**
- Static rotation based on `hash(qualified_id) % workspace_count`
- Each worker had a fixed starting position
- Problem: A worker whose fixed index landed near an always-non-empty workspace could permanently starve later workspaces

**Current behavior (bf-6anj4):**
- **Fresh shuffle every cycle**
- Random order using thread RNG
- De-herds workers without pinning coverage to static identity-derived values

```rust
// Shuffle this worker's workspace scan order fresh every cycle
let mut workspaces = { workspaces.lock().unwrap().clone() };
workspaces.shuffle(&mut rand::thread_rng());
```

### Candidate Aggregation

**Critical design (bf-4df1e / bf-47bfm):**

Explore **aggregates candidates across ALL workspaces** rather than returning on the first non-empty workspace. This prevents silent starvation where:
- Workspace 1 has only excluded beads (blocked, deferred, human labels)
- Workspace 2 has valid claimable beads
- Old behavior: Return on first non-empty workspace (Workspace 1) → filters exclude all → `NoWork` → Workspace 2 never scanned
- New behavior: Scan all workspaces → aggregate all candidates → return globally ranked list

```rust
// Aggregate candidates across ALL workspaces
let mut all_candidates: Vec<Bead> = Vec::new();

for workspace in &workspaces {
    // ... query workspace ...
    all_candidates.append(&mut candidates);
}

// Rank globally: priority ASC, created_at ASC, id ASC
all_candidates.sort_by(|a, b| {
    a.priority.cmp(&b.priority)
        .then_with(|| a.created_at.cmp(&b.created_at))
        .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
});

StrandResult::BeadFound(all_candidates)
```

### Filtering Logic

**Filters applied to every candidate:**

```rust
let filters = Filters {
    assignee: None,                           // Must be unassigned
    exclude_labels: vec![
        "deferred".to_string(),
        "human".to_string(),
        "blocked".to_string(),
    ],
    exclude_ids: HashSet::new(),
};
```

**Defensive double-filtering:**

Even though `store.ready()` receives `exclude_labels`, some backend implementations may not filter correctly. Explore applies belt-and-suspenders filtering:

```rust
candidates.retain(|b| {
    let assignee_ok = b.assignee.is_none();
    let labels_ok = !b.labels.iter().any(|l| filters.exclude_labels.contains(l));
    assignee_ok && labels_ok
});
```

### Workspace Exclusions

Explore skips workspaces under these conditions:

1. **Home workspace** — Already checked by Pluck
2. **Missing `.beads/` directory** — Not a valid bead workspace
3. **Store creation failure** — Backend binding failed
4. **Query failure** — `store.ready()` returned an error

All exclusions are tracked in `exclusion_reasons` for telemetry.

### Cross-Workspace Orphan Cleanup

When a workspace has no ready candidates:

1. List all beads in the workspace
2. Check for any `InProgress` beads
3. Run `cleanup_orphaned_in_progress` to release beads from dead workers
4. Re-query `ready()` to pick up newly-available beads
5. Continue scanning remaining workspaces

This prevents starvation from stuck beads assigned to workers that died without releasing them.

---

## Adaptive Scan Backoff

### Problem

Without backoff, Explore scans every selection cycle, causing:
- Excessive filesystem I/O
- Unnecessary store creation/teardown
- Wasted CPU scanning consistently-empty workspaces

### Solution

ExploreScanBackoff adapts the scan interval based on results:

**Empty scan → Increase interval geometrically (1, 2, 4, 8... capped at max)**
**Candidate found → Reset interval to base (1)**

**Configuration:**
- `scan_interval_cycles`: Base interval (default: 1)
- `max_scan_interval_cycles`: Maximum interval (default: 8)

**Example timeline:**

| Cycle | Found Candidate? | Interval to Next Scan |
|-------|------------------|------------------------|
| 1     | No               | 1                      |
| 2     | No               | 2                      |
| 3     | No               | 4                      |
| 4     | No               | 8 (capped)             |
| 5     | No               | 8 (capped)             |
| 6     | **Yes**          | 1 (reset)              |
| 7     | No               | 1                      |
| 8     | No               | 2                      |

**Implementation:**

```rust
fn effective_interval_cycles(&self) -> u32 {
    let multiplier = 1u32
        .checked_shl(self.consecutive_empty_scans.min(31))
        .unwrap_or(u32::MAX);
    self.base_interval_cycles
        .saturating_mul(multiplier)
        .min(self.max_interval_cycles)
}

fn record_scan(&mut self, found_candidate: bool) {
    if found_candidate {
        self.consecutive_empty_scans = 0;
        self.cycles_until_scan = self.base_interval_cycles.saturating_sub(1);
    } else {
        self.consecutive_empty_scans = self.consecutive_empty_scans.saturating_add(1);
        self.cycles_until_scan = self.effective_interval_cycles().saturating_sub(1);
    }
}
```

### Backoff vs. Strand Skipping

**Important:** A backoff skip returns `StrandResult::NoWork`, allowing the waterfall to continue evaluating Weave and later strands. Only Explore's remote scan is deferred — the worker doesn't sleep.

---

## Workspace Re-Discovery

### Historical Behavior

**Before bf-6anj4:** Workspace re-discovery ran on a throttle (`rediscovery_cycles`).

**Problem:** A newly created bead store needed a worker restart to be discovered — the workspace list was captured at boot and only refreshed periodically.

### Current Behavior

**As of bf-6anj4:** Workspace re-discovery runs **every selection cycle** (unconditionally in auto-discovery mode).

**Rationale:**
- A plain `read_dir` over ~40 entries is cheap (~1-2ms)
- Workers pick up new stores immediately without restart
- `rediscovery_cycles` is still parsed from config for backward compatibility but no longer applied
- Only pinned mode (`workspaces` non-empty) skips re-discovery

**Implementation:**

```rust
// Re-discover workspaces every cycle (bf-3peh4 / bf-6anj4)
let _cycle = self.cycles_since_rediscovery.fetch_add(1, Ordering::Relaxed) + 1;
let added = self.rediscover_workspaces();

if added > 0 {
    tracing::info!(worker = %self.qualified_id, added, "workspace re-discovery found new workspaces");
}
```

### Re-Discovery Constraints

**Re-discovery preserves:**
- **No upward traversal** — Only scans `workspace_root`'s immediate children
- **Explicit workspaces override** — Skipped when `workspaces` is non-empty (pinned mode)

```rust
fn rediscover_workspaces(&self) -> usize {
    // Skip re-discovery if we're in pinned mode
    if !self.auto_discovery_mode {
        tracing::debug!("skipping workspace re-discovery: running in pinned mode");
        return 0;
    }

    let new_workspaces = Self::discover_workspaces(&self.workspace_root);
    let added_count = new_workspaces.len().saturating_sub(previous_count);

    // Update the workspace list
    *self.workspaces.lock().unwrap() = new_workspaces;

    added_count
}
```

---

## Minimum Configuration for Explore to Scan

Explore scans workspaces when **all** of these conditions are met:

1. **Explore is enabled**
   ```yaml
   explore:
     enabled: true
   ```

2. **Workspaces are discoverable**
   - **Auto-discovery mode:** `workspaces: []` + `workspace_root` exists and contains directories with `.beads/`
   - **Pinned mode:** `workspaces: ["/path1", "/path2"]` paths exist and have `.beads/`

3. **Workspace root is readable** (auto-discovery mode only)
   - `workspace_root` directory exists
   - Process has read permission on `workspace_root`

### Example: Minimal Working Config

```yaml
# .needle.yaml
explore:
  enabled: true
  workspaces: []                 # Triggers auto-discovery
  workspace_root: "/home/coding" # Contains bead repos
```

**Requirements:**
- `/home/coding` directory exists
- At least one subdirectory has `.beads/` (e.g., `/home/coding/NEEDLE/.beads/`)
- Process has read access to `/home/coding`

### Example: Pinned Mode Config

```yaml
# .needle.yaml
explore:
  enabled: true
  workspaces:
    - /home/coding/NEEDLE
    - /home/coding/SEAM
```

**Requirements:**
- `/home/coding/NEEDLE/.beads/` exists
- `/home/coding/SEAM/.beads/` exists
- Process has read access to both paths

---

## Error Conditions That Prevent Scanning

Explore returns `StrandResult::NoWork` (emitting `StrandSkipped` telemetry) when:

### 1. Strand Disabled
```yaml
explore:
  enabled: false
```
**Reason:** `"disabled"`

### 2. Adaptive Backoff
Consecutive empty scans have increased the interval beyond this cycle.
**Reason:** `"adaptive_scan_backoff"`

### 3. No Workspaces Discovered
`workspace_root` doesn't exist, is unreadable, or contains no `.beads/` directories.
**Reason:** `"no_workspaces_discovered"`

### 4. No Candidates in Any Workspace
All workspaces scanned but none had claimable beads (all excluded, assigned, or empty).
**Reason:** `"no_candidates_in_any_workspace"`

### Telemetry Emitted

All skip conditions emit `EventKind::StrandSkipped` with `strand_name: "explore"` and the reason.

Successful scans emit `EventKind::ExploreScanSummary` with:
- `workspaces_visited`: List of workspace paths scanned
- `workspaces_with_candidates`: Workspaces that had claimable beads
- `total_candidates`: Total candidates found across all workspaces
- `exclusion_reasons`: Set of reasons workspaces/candidates were excluded
- `duration_ms`: Scan duration in milliseconds
- `scan_start_at`: Timestamp when scan started

---

## Bead Store Creation

### `discover_default` Factory

Explore uses a factory pattern to create bead stores for each workspace:

```rust
async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>> {
    discover_default(
        workspace.to_path_buf(),
        None,                              // model (optional)
        Some("needle".to_string()),        // harness
        Some(env!("CARGO_PKG_VERSION")),   // harness_version
    )
}
```

### Store Binding Resolution

`discover_default` loads the workspace's `.needle.yaml` to determine the bead backend:

```rust
pub fn discover_default(
    workspace: PathBuf,
    model: Option<String>,
    harness: Option<String>,
    harness_version: Option<String>,
) -> Result<Arc<dyn BeadStore>> {
    let (config, _) = ConfigLoader::load_resolved(
        &workspace,
        CliOverrides {
            workspace: Some(workspace.clone()),
            ..Default::default()
        },
    )?;

    open_configured(&config.bead_cli, workspace, model, harness, harness_version)
}
```

**Key point:** Each workspace's `.needle.yaml` declares its own bead backend (`bead_cli.backend`), allowing mixed fleets (some workspaces on bead-rs, others on bead-forge).

---

## Design Constraints (from v1 lessons)

### 1. No Upward Traversal

Explore only scans configured paths. It never walks parent directories or searches beyond `workspace_root`'s immediate children.

**Rationale:** Prevents surprising behavior and permission issues.

### 2. Static Workspace List

The workspace list is read from config at boot and captured in `ExploreStrand::new()`. It is refreshed periodically (every cycle in auto-discovery mode) but not re-evaluated from config files after startup.

**Rationale:** Predictable behavior; config changes require worker restart.

### 3. No Permanent Relocation

Workers process one bead from a remote workspace, then return to their home workspace. They never "move" to another workspace permanently.

**Rationale:** Workers remain manageable; home workspace is always the base.

---

## Configuration Examples

### Fleet Default (Auto-Discovery)

```yaml
# .needle.yaml
explore:
  enabled: true
  workspaces: []                 # Empty = auto-discovery
  workspace_root: "/home/coding" # Scan all repos in home
  rediscovery_cycles: 60         # Legacy field, no longer applied
  starvation_threshold_minutes: 15
  scan_interval_cycles: 1
  max_scan_interval_cycles: 8
```

**Behavior:** Discovers all repos under `/home/coding` with `.beads/` directories. Re-scans every cycle. Emits starvation WARN after 15 minutes of no successful claims while ready beads exist.

### Dedicated Worker (Pinned Mode)

```yaml
# .needle.yaml
explore:
  enabled: true
  workspaces:
    - /home/coding/NEEDLE       # Only scan NEEDLE
  # All other fields ignored when workspaces is non-empty
```

**Behavior:** Only scans `/home/coding/NEEDLE`. Ignores all other repos. Emits WARN at startup: `"Explore running in PINNED mode (non-empty workspaces list)"`.

### Disabled Explore

```yaml
# .needle.yaml
explore:
  enabled: false
```

**Behavior:** Explore strand never runs. Worker only checks home workspace (Pluck) and runs maintenance (Mend).

---

## Testing Considerations

### Test Isolation

Explore tests must isolate `$HOME` to prevent contamination:

```rust
// Set HOME to test's tempdir
cmd.env("HOME", temp_dir.path())

// Or disable Explore entirely in test config
config.strands.explore.enabled = false;
```

**Rationale:** The 2026-07-20 contamination incident where a non-isolated test created ~284 phantom beads across ~22 repos.

### Store Factory Injection

For testing complex scenarios (e.g., deadlock cases), inject a custom `StoreFactory`:

```rust
struct MockStoreFactory { /* ... */ }

impl StoreFactory for MockStoreFactory {
    async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>> {
        // Return controlled test stores
    }
}

let strand = ExploreStrand::new_with_store_factory(
    workspaces,
    home_workspace,
    registry,
    telemetry,
    qualified_id,
    Arc::new(mock_factory),
);
```

---

## Related Documentation

- **ADR-015:** Concurrent same-repo worker isolation (why no worktrees)
- **ADR-006:** Test isolation policy (2026-07-20 contamination incident)
- **bead_store:** Backend binding and store creation
- **docs/testing-isolation-patterns.md:** Comprehensive isolation patterns

---

## Summary

### Workspace Discovery

1. **Auto-discovery mode (default):** Empty `workspaces` → scan `workspace_root` children for `.beads/` directories
2. **Pinned mode (exception):** Non-empty `workspaces` → scan only explicit paths
3. **Re-discovery:** Runs every cycle in auto-discovery mode; skipped in pinned mode

### Scanning Behavior

1. **Adaptive backoff:** Empty scans increase interval (1, 2, 4, 8...); candidate found resets to 1
2. **Shuffle order:** Randomize workspace order each cycle to de-herd workers
3. **Aggregate globally:** Collect candidates from ALL workspaces, rank once
4. **Filter defensively:** Exclude assigned beads and deferred/human/blocked labels (belt-and-suspenders)
5. **Cross-workspace mend:** Release orphaned in-progress beads when no ready candidates

### Minimum Required Config

```yaml
explore:
  enabled: true
  workspaces: []                 # Empty = auto-discovery
  workspace_root: "$HOME"        # Any dir with bead repos
```

Explore scans when enabled + workspaces are discoverable + workspace root is readable.

### Error Conditions

- `disabled`: Strand is disabled in config
- `adaptive_scan_backoff`: Backing off from consecutive empty scans
- `no_workspaces_discovered`: No `.beads/` directories found
- `no_candidates_in_any_workspace`: All workspaces empty or excluded

All emit `StrandSkipped` telemetry with the reason.
