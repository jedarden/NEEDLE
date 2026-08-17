# Explore Strand Access Pattern Map

**Purpose:** Document exactly what code paths, configurations, and environment variables enable the Explore strand to activate and scan bead stores.

**Scope:** Complete trace from configuration loading through strand execution to bead store access.

---

## Configuration Structure

### `ExploreConfig` Definition

Located in `src/config/mod.rs:2670-2774`

```rust
pub struct ExploreConfig {
    /// Whether the Explore strand is enabled.
    pub enabled: bool,

    /// **Pin/exception list** for restricting a worker to specific workspaces.
    pub workspaces: Vec<PathBuf>,

    /// Root path for workspace auto-discovery (when `workspaces` is empty).
    pub workspace_root: PathBuf,

    /// Re-run workspace discovery every N cycles (0 = disabled).
    pub rediscovery_cycles: u32,

    /// Starvation alarm threshold in minutes (0 = disabled).
    pub starvation_threshold_minutes: u64,

    /// Minimum number of selection cycles between Explore scans.
    pub scan_interval_cycles: u32,

    /// Maximum number of selection cycles between Explore scans after adaptive backoff.
    pub max_scan_interval_cycles: u32,
}
```

### Default Values

All fields have defaults - Explore runs with **zero explicit configuration**:

| Field | Default | Source |
|-------|---------|--------|
| `enabled` | `true` | `ExploreConfig::default_enabled()` |
| `workspaces` | `Vec::new()` (empty) | Serde default |
| `workspace_root` | `$HOME` | `dirs_or_home("")` |
| `rediscovery_cycles` | `60` | `ExploreConfig::default_rediscovery_cycles()` |
| `starvation_threshold_minutes` | `15` | `ExploreConfig::default_starvation_threshold_minutes()` |
| `scan_interval_cycles` | `1` | `ExploreConfig::default_scan_interval_cycles()` |
| `max_scan_interval_cycles` | `8` | `ExploreConfig::default_max_scan_interval_cycles()` |

---

## Environment Variables

### Configuration Override Variables

From `src/config/mod.rs:5065-5081`

| Environment Variable | Target Field | Type | Purpose |
|---------------------|--------------|------|---------|
| `NEEDLE_STRANDS__EXPLORE__ENABLED` | `enabled` | bool | Enable/disable Explore strand |
| `NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT` | `workspace_root` | Path | Set workspace discovery root |

**Note:** Environment variables follow the pattern `NEEDLE_<section>__<key>` with double underscore (`__`) as separator.

---

## Minimum Required Configuration

Explore activates **with no configuration whatsoever**:

```yaml
# .needle.yaml - can be completely empty or omitted
strands:
  explore: {}
```

Or entirely absent:

```yaml
# No explore section needed - defaults apply automatically
```

The strand will:
1. Enable itself (`enabled: true` by default)
2. Scan `$HOME` for directories containing `.beads/` subdirectories
3. Query all discovered workspaces for ready beads

---

## Activation Conditions

### `evaluate()` Method Flow

Located in `src/strand/explore.rs:563-925`

The strand is evaluated by `StrandRunner::select()` in the waterfall order:
`Pluck → Resolve → Mend → Explore → Weave → Unravel → Pulse → Reflect → Splice → Knot`

### Decision Tree

```
ExploreStrand::evaluate()
│
├─ 1. Enabled check (line 571)
│   └─ if !self.enabled → return NoWork
│
├─ 2. Adaptive backoff check (line 584)
│   └─ if !should_scan_this_cycle() → return NoWork
│
├─ 3. Workspace re-discovery (line 608)
│   ├─ if auto_discovery_mode → rediscover_workspaces()
│   └─ else (pinned mode) → skip re-discovery
│
├─ 4. Workspaces existence check (line 620)
│   └─ if workspaces.is_empty() → return NoWork
│
├─ 5. Shuffle workspaces (line 648)
│   └─ Randomize scan order each cycle to de-herd workers
│
├─ 6. Iterate workspaces (lines 681-878)
│   │
│   ├─ For each workspace:
│   │   ├─ Skip if home workspace (line 687)
│   │   ├─ Skip if no .beads/ directory (line 694)
│   │   ├─ Create bead store (line 701)
│   │   ├─ Query ready() with filters (line 714)
│   │   ├─ Defensive filtering (line 721):
│   │   │   └─ Remove beads with assignees or excluded labels
│   │   ├─ Cross-workspace mend if empty (line 742)
│   │   │   └─ cleanup_orphaned_in_progress()
│   │   └─ Accumulate candidates (line 798)
│
├─ 7. Aggregate candidates across all workspaces (line 879)
│
├─ 8. If no candidates → return NoWork (line 892)
│
└─ 9. Rank and return candidates (line 909)
    └─ Sort by: priority ASC, created_at ASC, id ASC
```

### Detailed Condition Checks

#### 1. Enabled Check

```rust
if !self.enabled {
    let _ = self.telemetry.emit(EventKind::StrandSkipped {
        strand_name: "explore".to_string(),
        reason: "disabled".to_string(),
    });
    return StrandResult::NoWork;
}
```

**Activation:** `config.strands.explore.enabled = true`

**Override:** `NEEDLE_STRANDS__EXPLORE__ENABLED=true`

#### 2. Adaptive Backoff Check

```rust
if !self.should_scan_this_cycle() {
    let _ = self.telemetry.emit(EventKind::StrandSkipped {
        strand_name: "explore".to_string(),
        reason: "adaptive_scan_backoff".to_string(),
    });
    return StrandResult::NoWork;
}
```

**Implementation:** `ExploreScanBackoff` (lines 86-137)

**Behavior:**
- Base interval: `config.strands.explore.scan_interval_cycles` (default: 1)
- Max interval: `config.strands.explore.max_scan_interval_cycles` (default: 8)
- Empty scans double the interval: 1→2→4→8 (capped at max)
- Finding a candidate resets to base interval
- Skipped cycles do **not** advance the interval

#### 3. Workspace Re-discovery

```rust
let _cycle = self.cycles_since_rediscovery.fetch_add(1, Ordering::Relaxed) + 1;
let added = self.rediscover_workspaces();
```

**Auto-discovery mode** (`config.workspaces` is empty):
- Runs `discover_workspaces(&config.workspace_root)` every cycle
- Scans immediate children of `workspace_root` for `.beads/` directories
- Updates workspace list dynamically

**Pinned mode** (`config.workspaces` is non-empty):
- Skips re-discovery entirely
- Uses static workspace list from configuration

#### 4. Workspaces Existence Check

```rust
let workspaces = self.workspaces.lock().unwrap();
if workspaces.is_empty() {
    let _ = self.telemetry.emit(EventKind::StrandSkipped {
        strand_name: "explore".to_string(),
        reason: "no_workspaces_discovered".to_string(),
    });
    self.record_scan_result(false);
    return StrandResult::NoWork;
}
```

**Activation requires:** At least one workspace with `.beads/` directory

#### 5. Workspace Shuffle

```rust
let mut workspaces = { /* clone */ };
use rand::seq::SliceRandom;
workspaces.shuffle(&mut rand::thread_rng());
```

**Purpose:** De-herd workers by randomizing scan order each cycle (bf-6anj4)

**Replaces:** Static hash-based rotation that could permanently starve workspaces

#### 6. Workspace Iteration Filters

For each workspace:

```rust
// Skip home workspace
if workspace == &self.home_workspace {
    continue;
}

// Verify .beads/ exists
if !Self::has_beads_dir(workspace) {
    continue;
}

// Create bead store
let remote_store = match self.store_factory.create_store(workspace).await {
    Ok(s) => s,
    Err(e) => continue,
};
```

#### 7. Ready Query with Filters

```rust
let filters = Filters {
    assignee: None,
    exclude_labels: vec![
        "deferred".to_string(),
        "human".to_string(),
        "blocked".to_string(),
    ],
    exclude_ids: HashSet::new(),
};

let candidates = remote_store.ready(&filters).await?;
```

**Defensive filtering** (lines 721-726):
```rust
candidates.retain(|b| {
    let assignee_ok = b.assignee.is_none();
    let labels_ok = !b.labels.iter().any(|l| filters.exclude_labels.contains(l));
    assignee_ok && labels_ok
});
```

**Purpose:** Belt-and-suspenders filtering in case backend implementations don't respect Filters

#### 8. Cross-Workspace Mend

If no ready candidates after initial query:

```rust
match super::cleanup_orphaned_in_progress(
    remote_store.as_ref(),
    &self.registry,
    &self.telemetry,
    &self.qualified_id,
).await
{
    Ok(released) if released > 0 => {
        // Re-query ready after cleanup
        match remote_store.ready(&filters).await {
            Ok(retry_candidates) => {
                all_candidates.append(&mut retry_candidates);
            }
            // ...
        }
    }
    // ...
}
```

**Purpose:** Release beads assigned to dead workers across workspaces

**Note:** Does NOT return `WorkCreated` - released beads become available in next natural selection cycle

#### 9. Candidate Aggregation

```rust
let mut all_candidates: Vec<Bead> = Vec::new();

for workspace in &workspaces {
    // ... query and accumulate ...
    all_candidates.append(&mut candidates);
}
```

**Critical fix (bf-4df1e / bf-47bfm):** Previously returned on first non-empty workspace, causing silent starvation when early workspace beads were excluded by the outer waterfall

#### 10. Global Ranking

```rust
all_candidates.sort_by(|a, b| {
    a.priority
        .cmp(&b.priority)
        .then_with(|| a.created_at.cmp(&b.created_at))
        .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
});
```

**Sort order:** Priority (ASC) → Created at (ASC) → ID (ASC)

---

## Two Operational Modes

### Auto-Discovery Mode (Default/Recommended)

**Configuration:**
```yaml
strands:
  explore:
    workspaces: []  # empty (or omit)
```

**Behavior:**
- Scans `workspace_root` (default: `$HOME`) for `.beads/` directories
- Re-discovers workspaces every cycle
- Picks up new workspaces without configuration changes
- Logs: `"Explore auto-discovery: workspaces re-discovered every cycle"`

**Intended for:** Fleet-wide default behavior

### Pinned Mode (Exception)

**Configuration:**
```yaml
strands:
  explore:
    workspaces:
      - /path/to/workspace1
      - /path/to/workspace2
```

**Behavior:**
- Scans **only** explicitly listed workspaces
- Auto-discovery disabled
- Re-discovery skipped even if `rediscovery_cycles` is set
- Logs: `"Explore running in PINNED mode"` with WARN level

**Intended for:** Dedicated workers for high-priority workspaces

**Startup WARN log:**
```
Explore running in PINNED mode (non-empty workspaces list).
Auto-discovery is disabled; only the listed workspaces will be scanned.
This is an exception mechanism — the fleet default is empty workspaces.
pinned_repos: ["workspace1", "workspace2"]
```

---

## Helper Functions

### `dirs_or_home()`

**Location:** `src/config/mod.rs:5479-5485`

```rust
fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}
```

**Purpose:** Resolve paths relative to `$HOME`, with fallback to `/tmp`

**Used by:**
- `ExploreConfig::default_workspace_root()` → `dirs_or_home("")` → `$HOME`
- `AgentConfig::default_adapters_dir()` → `dirs_or_home(".config/needle/adapters")`
- `WorkspaceConfig::default_default()` → `dirs_or_home(".needle")`

### `discover_workspaces()`

**Location:** `src/strand/explore.rs:353-396`

```rust
fn discover_workspaces(root: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();

    if !root.exists() {
        return discovered; // empty, not an error
    }

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => return discovered, // empty, not an error
    };

    for entry in entries {
        let path = entry.path();
        if path.is_dir() && Self::has_beads_dir(&path) {
            discovered.push(path);
        }
    }

    discovered
}
```

**Purpose:** Find all directories under `root` containing `.beads/` subdirectory

**Constraints:**
- Only scans immediate children of `root` (no recursion)
- Returns empty vector if root doesn't exist or can't be read
- No upward traversal (never scans above `root`)

### `has_beads_dir()`

**Location:** `src/strand/explore.rs:399-401`

```rust
fn has_beads_dir(workspace: &Path) -> bool {
    workspace.join(".beads").is_dir()
}
```

**Purpose:** Check if a directory is a valid bead workspace

---

## Complete Access Path Summary

### For Auto-Discovery Mode (Default)

1. **Configuration loaded** (`src/config/mod.rs`)
   - `ExploreConfig::default()` → `enabled: true`, `workspaces: []`, `workspace_root: $HOME`

2. **Environment variables applied** (optional)
   - `NEEDLE_STRANDS__EXPLORE__ENABLED=true` sets `enabled`
   - `NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT=/custom/path` sets `workspace_root`

3. **Strand constructed** (`src/strand/mod.rs:162-168`)
   ```rust
   let explore = ExploreStrand::new(
       config.strands.explore.clone(),
       config.workspace.default.clone(),
       explore_registry,
       telemetry.clone(),
       worker_id.to_string(),
   );
   ```

4. **Strand evaluates** (`src/strand/explore.rs:563-925`)
   - Enabled check → pass
   - Adaptive backoff → scan this cycle
   - Re-discovery → `discover_workspaces($HOME)`
   - Workspaces check → at least one `.beads/` directory found
   - Shuffle → random order
   - Iterate → query each workspace's `ready()` with filters
   - Aggregate → collect all candidates
   - Rank → sort by priority/created/id
   - Return → `StrandResult::BeadFound(candidates)`

### For Pinned Mode (Exception)

1. **Configuration loaded**
   - `ExploreConfig` with `workspaces: [explicit paths]`

2. **Strand evaluates**
   - Enabled check → pass
   - Adaptive backoff → scan this cycle
   - Re-discovery → **skipped** (pinned mode)
   - Workspaces check → explicit list non-empty
   - Iterate → query only listed workspaces
   - Return → candidates from pinned workspaces only

---

## Critical Historical Fixes

### bf-4df1e / bf-47bfm: Multi-Workspace Aggregation

**Problem:** Early return on first non-empty workspace caused silent starvation when early workspace beads were race-lost or excluded by the outer waterfall.

**Fix:** Aggregate candidates across ALL workspaces before returning, then rank globally.

**Before:**
```rust
for workspace in &workspaces {
    let candidates = remote_store.ready(&filters).await?;
    if !candidates.is_empty() {
        return StrandResult::BeadFound(candidates); // EARLY RETURN
    }
}
```

**After:**
```rust
let mut all_candidates: Vec<Bead> = Vec::new();
for workspace in &workspaces {
    let candidates = remote_store.ready(&filters).await?;
    all_candidates.append(&mut candidates);
}
all_candidates.sort_by(...);
StrandResult::BeadFound(all_candidates)
```

### bf-6anj4: Per-Cycle Workspace Shuffle

**Problem:** Static hash-based rotation (`hash(qualified_id) % N`) caused permanent starvation when a worker's fixed index landed near an always-non-empty workspace.

**Fix:** Shuffle workspace list fresh every cycle using `rand::thread_rng()`.

**Before:**
```rust
let start = self.compute_start_index(); // hash(qualified_id) % N
let rotated = self.rotated_workspace_order(); // static per worker
```

**After:**
```rust
let mut workspaces = self.workspaces.lock().unwrap().clone();
workspaces.shuffle(&mut rand::thread_rng()); // fresh every cycle
```

### bf-3peh4: Cycle-by-Cycle Re-discovery

**Problem:** Newly created bead stores weren't detected until worker restart (re-discovery throttled to every 60 cycles).

**Fix:** Re-discovery now runs every cycle (unconditionally), since scanning ~40 entries is cheap.

**Before:**
```rust
if cycles_since_rediscovery >= rediscovery_cycles {
    rediscover_workspaces();
}
```

**After:**
```rust
let _cycle = self.cycles_since_rediscovery.fetch_add(1, Ordering::Relaxed) + 1;
let added = self.rediscover_workspaces(); // runs every cycle
```

---

## Testing Isolation Requirements

### Critical: `HOME` Isolation for Subprocess Tests

**ADR-006 Postmortem:** Non-isolated tests created 284 phantom beads across 22 repos under fixture worker identifiers.

**Requirement:** Any integration test spawning `needle` as a subprocess MUST isolate `$HOME`:

```rust
cmd.env("HOME", temp_dir.path())
```

**Reason:** Explore strand defaults to scanning `$HOME` for `.beads/` directories. Without isolation, tests leak into the real user environment.

**In-Process Tests:** Pin `workspace_root` explicitly:

```rust
config.strands.explore.workspace_root = temp_home.to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

**Reference:** `CLAUDE.md`, "Test Isolation Policy" section

---

## Acceptance Criteria Verification

✅ **Complete map of Explore access paths showing exactly when Explore can and cannot reach bead stores:**

| Scenario | Can Reach Stores? | Why |
|----------|-------------------|-----|
| Default config (empty `workspaces`) | ✅ Yes | Auto-discovery finds all `.beads/` under `$HOME` |
| Pinned config (explicit `workspaces`) | ✅ Yes | Scans only listed paths |
| `enabled: false` | ❌ No | Strand returns `NoWork` immediately |
| No `.beads/` directories in `workspace_root` | ❌ No | `discover_workspaces()` returns empty list |
| Adaptive backoff active | ❌ No | Scan deferred to next eligible cycle |
| Workspace without `.beads/` directory | ❌ No | Skipped by `has_beads_dir()` check |
| All beads assigned/excluded | ❌ No | Filters remove all candidates; cross-workspace mend runs but returns `NoWork` if still empty |

✅ **Minimum required configuration identified:**

- **Zero configuration required** - all fields have defaults
- Explore activates automatically: `enabled: true`, `workspaces: []` (auto-discovery), `workspace_root: $HOME`

✅ **Environment variables documented:**

- `NEEDLE_STRANDS__EXPLORE__ENABLED` → controls `enabled`
- `NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT` → controls `workspace_root`

✅ **Code paths traced:**

- Config loading: `src/config/mod.rs:2670-2774` (ExploreConfig), `src/config/mod.rs:5065-5081` (env vars)
- Strand construction: `src/strand/mod.rs:162-168`
- Strand evaluation: `src/strand/explore.rs:563-925`
- Workspace discovery: `src/strand/explore.rs:353-396`
- Helper functions: `src/config/mod.rs:5479-5485` (`dirs_or_home`)

---

## Reference Summary

**Key Files:**
- `src/config/mod.rs` - ExploreConfig structure, defaults, environment variable loading
- `src/strand/explore.rs` - ExploreStrand implementation, evaluation logic, workspace discovery
- `src/strand/mod.rs` - StrandRunner construction, waterfall integration

**Key Configuration Paths:**
- Global config: `~/.config/needle/config.yaml`
- Workspace config: `.needle.yaml`
- Environment: `NEEDLE_STRANDS__EXPLORE__*`

**Key Runtime Paths:**
- Default workspace root: `$HOME`
- Workspace marker: `.beads/` directory
- Home workspace exclusion: `config.workspace.default`

**Historical Context:**
- ADR-006: Test isolation incident (phantom beads from non-isolated `HOME`)
- bf-4df1e / bf-47bfm: Multi-workspace aggregation fix
- bf-6anj4: Per-cycle workspace shuffle (de-herding)
- bf-3peh4: Cycle-by-cycle re-discovery (dynamic workspace pickup)
