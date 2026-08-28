# Explore Strand: Workspace Discovery and Scanning

**Document Version:** 1.0  
**Last Updated:** 2026-08-28  
**Bead:** needle-d5cb0954

## Overview

The Explore strand enables multi-workspace bead discovery by scanning configured repositories for claimable beads. This document traces the complete workspace discovery mechanism, scanning behavior, filtering logic, and minimum required configuration.

## Architecture Summary

```
ExploreStrand::new()
    │
    ├─ If config.workspaces is empty → AUTO-DISCOVERY MODE (default)
    │   └─ Call discover_workspaces(&config.workspace_root)
    │       └─ Scan immediate children of workspace_root
    │           └─ Filter: directories containing .beads/ subdirectory
    │
    └─ If config.workspaces is non-empty → PINNED MODE (exception)
        └─ Use explicit workspace paths directly
            └─ WARN log emitted at startup

During each evaluate() cycle:
    ├─ Re-discover workspaces (auto-discovery mode only)
    ├─ Shuffle workspace scan order (per-cycle de-herding)
    ├─ For each workspace:
    │   ├─ Skip if == home_workspace
    │   ├─ Skip if no .beads/ directory
    │   ├─ Create bead store for workspace
    │   ├─ Query ready beads with Filters
    │   ├─ Apply defensive filtering (assignee, labels)
    │   └─ If empty, run cross-workspace mend
    └─ Aggregate and rank candidates globally
```

## 1. Workspace Discovery Mechanism

### 1.1 Two Operating Modes

**DEFAULT MODE (RECOMMENDED): Auto-Discovery**

```yaml
# config.yaml
strands:
  explore:
    enabled: true
    workspaces: []              # Empty = auto-discovery (default)
    workspace_root: /home/user  # Scan this directory's children
```

When `config.workspaces` is **empty** (the default):
- Explore runs **recursive workspace discovery** under `config.workspace_root`
- All directories containing a `.beads/` subdirectory are automatically scanned
- New workspaces are picked up automatically without configuration changes
- This is the **intended default for the fleet**

**PINNED MODE (EXCEPTION): Explicit List**

```yaml
strands:
  explore:
    enabled: true
    workspaces:                 # Non-empty = pinned mode
      - /home/user/repo1
      - /home/user/repo2
    workspace_root: /home/user  # Ignored in pinned mode
```

When `config.workspaces` is **non-empty**:
- Auto-discovery is **disabled**
- **Only** the explicitly listed paths are scanned
- Used to restrict specific workers to fixed repo sets
- This is an **exception mechanism** — most workers should leave this empty
- A **WARN log is emitted at startup** naming the pinned repos

### 1.2 Discovery Implementation

**Source:** `src/strand/explore.rs:353-396`

```rust
fn discover_workspaces(root: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();

    // Non-existent root returns empty (not an error)
    if !root.exists() {
        tracing::debug!(root = %root.display(), 
                       "workspace root does not exist, no workspaces discovered");
        return discovered;
    }

    // Unreadable root returns empty (not an error)
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(root = %root.display(), error = %e, 
                           "failed to read workspace root");
            return discovered;
        }
    };

    // Filter for immediate children containing .beads/
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        if path.is_dir() && Self::has_beads_dir(&path) {
            discovered.push(path);
        }
    }

    discovered
}

fn has_beads_dir(workspace: &Path) -> bool {
    workspace.join(".beads").is_dir()
}
```

**Key Constraints:**
- ✅ Scans **only immediate children** of `workspace_root` (no upward traversal)
- ✅ Only one level deep (non-recursive)
- ✅ Requires `.beads/` subdirectory to qualify as a workspace
- ✅ Non-existent/unreadable roots return empty (fail-safe)
- ❌ Does NOT scan deeper subdirectories

### 1.3 Re-Discovery Behavior

**Historical Context (bf-6anj4):**

Previously, workspace discovery ran only on a throttle (`rediscovery_cycles`). This meant newly-created bead stores required a worker restart to be seen.

**Current Behavior:**

As of bf-6anj4 (2026-08-21), re-discovery **runs unconditionally every cycle**:

```rust
async fn evaluate(&self, ...) -> StrandResult {
    // Re-discover workspaces every cycle (bf-3peh4 / bf-6anj4)
    let _cycle = self.cycles_since_rediscovery
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let added = self.rediscover_workspaces();
    
    // ... scan with fresh workspace list
}
```

The legacy `rediscovery_cycles` config field is **still parsed** (for backward compatibility with existing `.needle.yaml` files) but **no longer enforced**. It exists only for documentation purposes.

**Pinned Mode Exception:**

When running in pinned mode (`auto_discovery_mode == false`), `rediscover_workspaces()` is a no-op that immediately returns 0. The workspace list is static after boot.

## 2. Workspace Root Scanning Behavior

### 2.1 Default Workspace Root

**Source:** `src/config/mod.rs:4479-4481`

```rust
fn default_workspace_root() -> PathBuf {
    dirs_or_home("")  // Returns $HOME or falls back to /tmp
}

fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}
```

**Defaults:**
- **Primary:** `$HOME` environment variable (user's home directory)
- **Fallback:** `/tmp` if `HOME` is not set

**Example:**
```bash
# If HOME=/home/coding
workspace_root = /home/coding

# If HOME is unset
workspace_root = /tmp
```

### 2.2 Scanning Depth

**ONLY immediate children of `workspace_root` are scanned.**

Example structure:
```
/home/coding/
├── NEEDLE/           # ✅ Scanned (contains .beads/)
├── SEAM/             # ✅ Scanned (contains .beads/)
├── commitgraph/      # ✅ Scanned (contains .beads/)
├── .config/          # ❌ Skipped (no .beads/)
├── NEEDLE/docs/      # ❌ Skipped (not a direct child)
└── SEAM/.beads/      # ❌ Skipped (not a direct child)
```

**Rationale:** Prevents upward traversal and keeps scanning bounded/predictable.

## 3. Workspaces Vec Filtering Logic

### 3.1 Static Capture at Boot

**Source:** `src/strand/explore.rs:197-212`

```rust
pub fn new(
    config: ExploreConfig,
    home_workspace: PathBuf,
    registry: Registry,
    telemetry: Telemetry,
    qualified_id: String,
) -> Self {
    let auto_discovery_mode = config.workspaces.is_empty();

    // Capture workspace list at construction time
    let workspaces = if auto_discovery_mode {
        Self::discover_workspaces(&config.workspace_root)
    } else {
        config.workspaces  // Use explicit list directly
    };

    ExploreStrand {
        workspaces: std::sync::Mutex::new(workspaces),
        auto_discovery_mode,
        // ...
    }
}
```

**Key Points:**
- Workspace list is read from config **once at construction time**
- In auto-discovery mode, refreshed every cycle (see Section 1.3)
- In pinned mode, list is **static for the worker's lifetime**
- Wrapped in `Mutex` for interior mutability during re-discovery

### 3.2 Home Workspace Exclusion

**Source:** `src/strand/explore.rs:691-696`

```rust
for workspace in &workspaces {
    // Skip the home workspace — Pluck already checked it
    if workspace == &self.home_workspace {
        tracing::debug!(workspace = %workspace.display(), 
                       "skipping home workspace");
        continue;
    }
    // ... scan workspace
}
```

**Rationale:** The home workspace is already scanned by the Pluck strand (the first strand in the waterfall). Explore would redundantly query it and potentially return beads already claimed or excluded.

### 3.3 .beads/ Directory Requirement

**Source:** `src/strand/explore.rs:698-703`

```rust
// Check that .beads/ exists before attempting to query
if !Self::has_beads_dir(workspace) {
    tracing::debug!(workspace = %workspace.display(), 
                   "no .beads/ directory, skipping");
    continue;
}
```

**Defensive Check:** Even though discovery should only return workspaces with `.beads/`, this check handles edge cases:
- Directory deleted after discovery
- Explicit workspaces list contains non-workspace paths
- Race conditions during re-discovery

### 3.4 Store Creation Failure Handling

**Source:** `src/strand/explore.rs:705-717`

```rust
let remote_store = match self.store_factory.create_store(workspace).await {
    Ok(s) => s,
    Err(e) => {
        tracing::warn!(
            workspace = %workspace.display(),
            error = %e,
            "failed to create bead store for workspace, skipping"
        );
        continue;  // Skip to next workspace
    }
};
```

**Failure Modes:**
- Invalid bead store backend configuration
- Corrupted `.beads/` directory
- Missing database file
- Permission errors

**Behavior:** Workspace is **skipped with a WARN log** — does NOT fail the strand.

## 4. Minimum Required Configuration

### 4.1 Absolute Minimum (Zero Config)

The absolute minimum configuration for Explore to scan is **nothing** — all defaults work:

```toml
# ~/.config/needle/config.yaml (minimal)
# Empty file — all fields use defaults
```

This is equivalent to:

```yaml
strands:
  explore:
    enabled: true                  # default_enabled() = true
    workspaces: []                 # serde(default) = empty Vec
    workspace_root: /home/coding   # dirs_or_home("")
    rediscovery_cycles: 60         # default_rediscovery_cycles() = 60
    starvation_threshold_minutes: 15
    scan_interval_cycles: 1
    max_scan_interval_cycles: 8
```

**With this config:**
- ✅ Explore strand is **enabled**
- ✅ Auto-discovery mode (empty `workspaces`)
- ✅ Scans `$HOME/.beads/` directories
- ✅ Picks up new workspaces automatically

### 4.2 Recommended Production Config

```yaml
strands:
  explore:
    enabled: true
    workspaces: []                 # Leave empty for auto-discovery
    workspace_root: /home/coding   # Optional: explicit root
```

**Why this is recommended:**
- Explicit about enabling Explore
- Empty `workspaces` signals intent (auto-discovery)
- Optional `workspace_root` for non-standard homes

### 4.3 Pinned Worker Config (Exception)

```yaml
strands:
  explore:
    enabled: true
    workspaces:                   # Explicit list = pinned mode
      - /home/coding/high-priority-repo
      - /home/coding/another-repo
    # workspace_root is ignored in pinned mode
```

**WARN log emitted at startup:**
```
WARN Explore running in PINNED mode (non-empty workspaces list). 
     Auto-discovery is disabled; only the listed workspaces will be scanned. 
     This is an exception mechanism — the fleet default is empty workspaces.
```

## 5. Error Conditions That Prevent Scanning

### 5.1 Strand-Level Errors

**Explore returns `StrandResult::NoWork` (NOT an error) when:**

| Condition | Behavior | Log Level |
|-----------|----------|-----------|
| `enabled: false` | Strand skipped | INFO |
| Adaptive scan backoff defers scan | Returns NoWork, will retry later | DEBUG |
| No workspaces discovered after discovery | Strand has no work to scan | INFO |
| All workspaces skipped (home, no .beads/, store errors) | No candidates found | INFO (skips) / WARN (store errors) |

### 5.2 Workspace-Level Failures

**Individual workspace failures do NOT prevent scanning other workspaces:**

| Failure Mode | Behavior | Log Level |
|-------------|----------|-----------|
| Workspace doesn't exist | Skipped (defensive) | DEBUG |
| Workspace lacks `.beads/` | Skipped (defensive) | DEBUG |
| Store creation fails | Skipped, continue to next | WARN |
| Store query fails | Skipped, continue to next | WARN |
| No ready beads | Run cross-workspace mend, continue | DEBUG |

### 5.3 Fatal Errors (Prevent Strand Creation)

These prevent `ExploreStrand::new()` from succeeding:

- Invalid config type (e.g., `workspaces` not a Vec)
- Invalid `workspace_root` type (not a PathBuf)

**However**, these are caught at config deserialization time, not during strand evaluation.

## 6. Scanning Cadence and De-Herding

### 6.1 Adaptive Scan Backoff

**Source:** `src/strand/explore.rs:86-137`

Explore implements adaptive backoff to reduce expensive remote scans when all workspaces are consistently empty:

```rust
struct ExploreScanBackoff {
    base_interval_cycles: u32,      // Default: 1
    max_interval_cycles: u32,      // Default: 8
    consecutive_empty_scans: u32,
    cycles_until_scan: u32,
}

// Interval doubles with each empty scan, capped at max
fn effective_interval_cycles(&self) -> u32 {
    let multiplier = 1u32
        .checked_shl(self.consecutive_empty_scans.min(31))
        .unwrap_or(u32::MAX);
    self.base_interval_cycles
        .saturating_mul(multiplier)
        .min(self.max_interval_cycles)
}
```

**Behavior:**
- Empty scan 1: next scan in 1 cycle
- Empty scan 2: next scan in 2 cycles
- Empty scan 3: next scan in 4 cycles
- Empty scan 4+: next scan in 8 cycles (capped)
- **Found candidate:** resets to 1 cycle immediately

**Config:**
```yaml
strands:
  explore:
    scan_interval_cycles: 1        # Minimum interval
    max_scan_interval_cycles: 8    # Maximum interval after backoff
```

### 6.2 Per-Cycle Shuffle (De-Herding)

**Source:** `src/strand/explore.rs:642-663`

```rust
// Shuffle this worker's workspace scan order fresh every cycle (bf-6anj4)
let mut workspaces = {
    let workspaces = self.workspaces.lock().unwrap();
    workspaces.clone()
};
{
    use rand::seq::SliceRandom;
    workspaces.shuffle(&mut rand::thread_rng());
}
```

**Historical Context:**

Previously, each worker had a **static rotation order** derived from `hash(qualified_id) % N`. This meant a worker whose fixed index landed near an always-non-empty workspace could **permanently starve** later workspaces.

**Current Behavior:**

Every cycle, each worker **shuffles its scan order randomly**. This ensures:
- No permanent bias toward any workspace
- All workspaces get equal coverage over time
- Multiple workers naturally de-herd across workspaces

## 7. Cross-Workspace Aggregation

### 7.1 Global Candidate Ranking

**Source:** `src/strand/explore.rs:943-948`

```rust
// Rank aggregated candidates globally
all_candidates.sort_by(|a, b| {
    a.priority
        .cmp(&b.priority)
        .then_with(|| a.created_at.cmp(&b.created_at))
        .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
});
```

**Sort Order (ASC):**
1. `priority` (0 = highest, 4 = lowest)
2. `created_at` (older wins ties)
3. `id` (deterministic tiebreaker)

**Rationale:** Ensures the highest-priority bead across **all workspaces** wins, not just the first workspace in scan order.

### 7.2 Why Aggregate Instead of First-Match?

**Historical Bug (bf-4df1e):**

Previously, Explore returned on the first non-empty workspace. When that workspace's only candidates were excluded (assigned, blocked labels, etc.), the strand returned `NoWork` without scanning the remaining workspaces.

**Fix:** Scan **all** workspaces, aggregate all candidates, rank globally, return the full list. The outer waterfall handles exclusions.

## 8. Cross-Workspace Mend

**Source:** `src/strand/explore.rs:739-876`

When a workspace has no ready candidates, Explore runs **cross-workspace mend** to release orphaned in-progress beads:

```rust
if candidates.is_empty() {
    // Check for in-progress beads
    let all_beads = match remote_store.list_all().await {
        Ok(beads) => beads,
        Err(e) => {
            tracing::warn!("failed to list beads for orphan check, skipping");
            continue;
        }
    };

    let has_in_progress = all_beads
        .iter()
        .any(|b| b.status == BeadStatus::InProgress);

    if !has_in_progress {
        continue;  // No orphans to release
    }

    // Release orphans (workers that died)
    match cleanup_orphaned_in_progress(
        remote_store.as_ref(),
        &self.registry,
        &self.telemetry,
        &self.qualified_id,
    ).await
    {
        Ok(released) if released > 0 => {
            // Re-query ready after cleanup
            // ... re-query with same filters
        }
        // ...
    }
}
```

**Behavior:**
1. Check if workspace has any `in_progress` beads
2. Query worker registry to identify live workers
3. Release beads assigned to dead workers
4. Re-query `ready()` with same filters
5. If candidates found, accumulate them (do NOT return `WorkCreated`)

**Why NOT `WorkCreated`?**

Returning `WorkCreated` would restart the waterfall from Pluck. Instead, Explore accumulates the newly-available beads and returns them in `BeadFound`.

## 9. Example Scenarios

### Scenario 1: Fresh Worker with No Config

```yaml
# No config file exists
```

**Behavior:**
1. `ExploreConfig::default()` → `enabled: true`, `workspaces: []`
2. `workspace_root` = `$HOME` (e.g., `/home/coding`)
3. Discover workspaces under `/home/coding`
4. Scan each for ready beads
5. Return highest-priority candidate globally

### Scenario 2: Pinned Worker for High-Priority Repo

```yaml
strands:
  explore:
    enabled: true
    workspaces:
      - /home/coding/CRITICAL-PROJECT
```

**Behavior:**
1. WARN log: "Explore running in PINNED mode"
2. Only scan `/home/coding/CRITICAL-PROJECT`
3. Ignore all other repos under `$HOME`
4. Re-discovery is a no-op (static list)

### Scenario 3: Workspace Root Doesn't Exist

```yaml
strands:
  explore:
    workspace_root: /nonexistent/path
```

**Behavior:**
1. `discover_workspaces()` finds no directories
2. Returns empty workspace list
3. Strand returns `NoWork`
4. No error — fails safe

### Scenario 4: Worker Without HOME Set

```bash
unset HOME
needle worker --config ...
```

**Behavior:**
1. `dirs_or_home("")` returns `/tmp`
2. Scans `/tmp` for `.beads/` directories
3. Unlikely to find workspaces (returns empty)
4. Strand returns `NoWork`

## 10. Troubleshooting

### Symptom: Explore Returns NoWork But Workspaces Exist

**Check:**
1. Is `strands.explore.enabled: true`?
2. Does `workspace_root` point to the correct directory?
3. Do workspaces have `.beads/` subdirectories?
4. Are all beads in workspaces assigned or excluded?

### Symptom: New Workspace Not Detected

**Check:**
1. Is `workspaces` empty (auto-discovery mode)?
2. Is the new workspace under `workspace_root`?
3. Does the new workspace have a `.beads/` directory?
4. Has at least one selection cycle passed?

### Symptom: WARN Log About Pinned Mode

**Interpretation:**
- Your config has `workspaces: [...]` (non-empty)
- Auto-discovery is disabled
- Only listed workspaces will be scanned
- If unintended, remove `workspaces` entries

### Symptom: High CPU Usage From Frequent Scanning

**Check:**
1. Are all workspaces consistently empty?
2. Is adaptive backoff working? (Check logs for "updated Explore adaptive scan cadence")
3. Consider increasing `scan_interval_cycles` or `max_scan_interval_cycles`

## 11. References

- **Source:** `src/strand/explore.rs:1-3980`
- **Config:** `src/config/mod.rs:4390-4494`
- **Tests:** `src/strand/explore.rs:964-3980` (comprehensive unit tests)
- **Related Beads:**
  - bf-6anj4: Per-cycle shuffle (de-herding fix)
  - bf-4df1e: Cross-workspace aggregation (starvation fix)
  - bf-3peh4: Per-cycle re-discovery (fresh workspace pickup)

---

**Document Status:** ✅ Complete  
**Next Review:** After any Explore strand refactoring
