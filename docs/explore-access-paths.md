# Explore Strand Access Paths

Complete map of code paths, configurations, and conditions that enable the Explore strand to activate and scan bead stores.

## Overview

The Explore strand is NEEDLE's multi-workspace discovery mechanism. It scans directories for bead workspaces and queries them for ready beads. This document maps every condition that enables or prevents Explore from reaching bead stores.

## ExploreConfig Structure

**Location**: `src/config/mod.rs:3514-3572`

```rust
pub struct ExploreConfig {
    /// Whether the Explore strand is enabled
    #[serde(default = "ExploreConfig::default_enabled")]
    pub enabled: bool,

    /// Pin/exception list for restricting worker to specific workspaces
    /// Empty (default) = auto-discovery mode, non-empty = pinned mode
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,

    /// Root path for workspace auto-discovery (when workspaces is empty)
    #[serde(default = "ExploreConfig::default_workspace_root")]
    pub workspace_root: PathBuf,

    /// Re-run workspace discovery every N cycles (0 = disabled)
    #[serde(default = "ExploreConfig::default_rediscovery_cycles")]
    pub rediscovery_cycles: u32,

    /// Starvation alarm threshold in minutes (0 = disabled)
    #[serde(default = "ExploreConfig::default_starvation_threshold_minutes")]
    pub starvation_threshold_minutes: u64,

    /// Minimum selection cycles between Explore scans
    #[serde(default = "ExploreConfig::default_scan_interval_cycles")]
    pub scan_interval_cycles: u32,

    /// Maximum cycles between Explore scans after adaptive backoff
    #[serde(default = "ExploreConfig::default_max_scan_interval_cycles")]
    pub max_scan_interval_cycles: u32,
}
```

### Default Values

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `true` | Strand is enabled by default |
| `workspaces` | `Vec::new()` | Empty = auto-discovery mode |
| `workspace_root` | `dirs_or_home("")` | User's home directory |
| `rediscovery_cycles` | `60` | Re-discover every 60 cycles |
| `starvation_threshold_minutes` | `15` | Warn if no work for 15 minutes |
| `scan_interval_cycles` | `1` | Minimum 1 cycle between scans |
| `max_scan_interval_cycles` | `8` | Maximum 8 cycles (adaptive backoff) |

## Configuration Sources (Priority Order)

1. **Hardcoded defaults** in `ExploreConfig::default_*()` methods
2. **Global config file** (`~/.needle/config.yaml` or `.needle.yaml`)
3. **Workspace overrides** (`.needle.yaml` in workspace directory)
4. **Environment variables** (`NEEDLE_STRANDS__EXPLORE__*`)
5. **CLI arguments** (command-line flags)

### Environment Variables

| Variable | Effect | Example |
|----------|--------|---------|
| `NEEDLE_STRANDS__EXPLORE__ENABLED` | Enable/disable strand | `export NEEDLE_STRANDS__EXPLORE__ENABLED=false` |
| `NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT` | Override discovery root | `export NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT=/opt/workspaces` |

### Configuration File Example

```yaml
# .needle.yaml
strands:
  explore:
    enabled: true
    workspaces: []          # Empty = auto-discovery mode
    workspace_root: ~/       # Root for auto-discovery
```

## Strand Initialization

**Location**: `src/strand/mod.rs:154-160`

```rust
let explore = ExploreStrand::new(
    config.strands.explore.clone(),
    config.workspace.default.clone(),
    explore_registry,
    telemetry.clone(),
    worker_id.to_string(),
);
```

**Waterfall Position**: 3rd strand
```
Pluck → Mend → Explore → Weave → Unravel → Pulse → Reflect → Splice → Knot
```

Explore only runs if both Pluck and Mend return `NoWork`.

## Workspace Discovery Mechanism

**Location**: `src/strand/explore.rs:353-396`

### Discovery Contract

```rust
let auto_discovery_mode = config.workspaces.is_empty();

let workspaces = if auto_discovery_mode {
    Self::discover_workspaces(&config.workspace_root)  // Auto-discover
} else {
    config.workspaces  // Use explicit list
};
```

### Workspace Validation

```rust
fn has_beads_dir(workspace: &Path) -> bool {
    workspace.join(".beads").is_dir()
}
```

**Rules**:
- Scans only immediate children of `workspace_root` (no upward traversal)
- Directory must contain `.beads/` subdirectory
- Returns empty vector if root doesn't exist or can't be read
- Re-discovery runs every cycle in auto-discovery mode (bf-6anj4)

### Discovery Modes

#### Auto-Discovery Mode (Default)
```yaml
strands:
  explore:
    workspaces: []          # Empty triggers auto-discovery
    workspace_root: ~/workspaces
```

Behavior:
- Scans `workspace_root` for directories containing `.beads/`
- Dynamically adjusts to new/deleted workspaces
- No hard-coded workspace list

#### Pinned Mode
```yaml
strands:
  explore:
    workspaces:
      - ~/repos/project1
      - ~/repos/project2
    workspace_root: ~/
```

Behavior:
- Only scans explicitly listed workspaces
- Auto-discovery is completely disabled
- Emits WARN log at startup with pinned repo names

## Complete Access Path Map

### ENABLE CONDITIONS (All must be true)

1. ✅ **Config enabled**: `config.strands.explore.enabled == true`
2. ✅ **Adaptive backoff permits scan**: No active backoff or backoff interval elapsed
3. ✅ **Workspaces discovered**: At least one valid workspace found
4. ✅ **Valid bead stores**: At least one workspace has accessible `.beads/` directory
5. ✅ **Ready candidates exist**: At least one bead is ready to claim

### DISABLE CONDITIONS (Any one prevents scanning)

#### 1. Configuration Disabled

```yaml
strands:
  explore:
    enabled: false
```

**Effect**:
- Early return with `StrandResult::NoWork`
- Emits `StrandSkipped` telemetry with reason `"disabled"`

#### 2. Adaptive Scan Backoff

Empty scans trigger exponential backoff to reduce unnecessary I/O:

- Base interval: `scan_interval_cycles` (default 1)
- Max interval: `max_scan_interval_cycles` (default 8)
- Backoff doubles on each empty scan: 1 → 2 → 4 → 8 → 8
- Resets immediately when candidates are found

**Effect**:
- Returns `StrandResult::NoWork` with reason `"adaptive_scan_backoff"`
- Defers scan until interval elapses

#### 3. No Workspaces Discovered

Conditions:
- Auto-discovery finds no directories with `.beads/`
- `workspace_root` doesn't exist or is unreadable
- Explicit workspaces list is empty

**Effect**:
- Returns `StrandResult::NoWork` with reason `"no_workspaces_discovered"`

#### 4. All Workspaces Filtered Out

Each workspace is checked and may be excluded:

| Filter | Condition | Skip Reason |
|--------|-----------|-------------|
| Home workspace | `workspace == config.workspace.default` | Pluck covers it |
| Missing `.beads/` | `!has_beads_dir(workspace)` | `"no_beads_dir"` |
| Store error | BeadStore creation fails | Error message |
| Empty candidates | `bf ready --limit 1` returns nothing | Cross-workspace mend |
| All excluded | Deferred, human, blocked labels | Continue to next |
| All assigned | Every ready bead has assignee | Continue to next |

**Effect**:
- Skips workspace with appropriate log message
- Continues to next workspace in list
- Returns `NoWork` if all workspaces filtered

#### 5. Pinned Mode Override

```yaml
strands:
  explore:
    workspaces:
      - ~/repos/only-this-one
```

**Effect**:
- Auto-discovery completely disabled
- Only explicitly listed workspaces scanned
- Emits WARN at startup:
  ```
  running in pinned mode (explicit workspaces list): repo1, repo2, ...
  ```

## Filtering During Scans

For each discovered workspace:

1. **Home workspace check**: Skip if matches `config.workspace.default` (Pluck covers it)
2. **Bead directory check**: Skip if `.beads/` doesn't exist
3. **Store creation**: Attempt to create `BeadStore` for workspace
4. **Candidate query**: Run `bf ready --limit 1` in workspace
5. **Cross-workspace mend**: If empty, run mend then re-query
6. **Label filtering**: Exclude deferred, human, blocked labels
7. **Assignment check**: Exclude beads already assigned to live workers

## Waterfall Integration

Explore is positioned 3rd in the strand evaluation waterfall:

```rust
Pluck      → Check home workspace for ready beads
Mend       → Run maintenance tasks
Explore    → Scan other workspaces for ready beads
Weave      → Dispatch agent to work on bead
Unravel    → Handle agent outcome
Pulse      → Heartbeat monitoring
Reflect    → Post-work analysis
Splice     → Commit completed work
Knot       → Close completed beads
```

**Explore runs only if**:
- Pluck returned `NoWork` (home workspace has no ready beads)
- Mend returned `NoWork` (maintenance found no work to do)

## Minimum Configuration for Explore to Run

### Absolute Minimum (Recommended)

```yaml
strands:
  explore:
    enabled: true
    workspaces: []          # Empty triggers auto-discovery
    workspace_root: ~/workspaces
```

With valid workspace structure:
```
~/workspaces/
├── project1/.beads/      # Valid workspace - will be discovered
├── project2/.beads/      # Valid workspace - will be discovered
└── project3/             # Ignored - no .beads/
```

### Alternative: Pinned Mode

```yaml
strands:
  explore:
    enabled: true
    workspaces:
      - ~/repos/project1
      - ~/repos/project2
```

No workspace structure requirements beyond explicit list.

## Telemetry Events

Explore emits these telemetry events:

| Event | When Emitted | Details |
|-------|--------------|---------|
| `StrandSkipped` | Strand disabled | reason: `"disabled"` |
| `StrandSkipped` | Adaptive backoff | reason: `"adaptive_scan_backoff"` |
| `StrandSkipped` | No workspaces | reason: `"no_workspaces_discovered"` |
| `StrandSkipped` | Pinned mode empty list | reason: `"no_workspaces_configured"` |
| `BeadFound` | Ready bead discovered | bead_id, workspace path |

## Testing Isolation

**Critical**: Tests that spawn the `needle` binary MUST isolate both `$HOME` and `workspace_root`. Without isolation, the binary will leak into the real user environment and scan production bead stores.

Required isolation:
```rust
cmd.env("HOME", temp_dir.path())

// For in-process tests:
config.strands.explore.workspace_root = temp_home.to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

See `docs/testing-isolation-patterns.md` for comprehensive isolation patterns.

## Related Documentation

- `docs/testing-isolation-patterns.md` - Test isolation requirements
- `docs/adr/006-test-contamination-2026-07-20.md` - Postmortem of contamination incident
- `CLAUDE.md` - "NEEDLE Learnings" section for historical context

## Code References

- **Config structure**: `src/config/mod.rs:3514-3572`
- **Strand initialization**: `src/strand/mod.rs:154-160`
- **Explore implementation**: `src/strand/explore.rs`
- **Workspace discovery**: `src/strand/explore.rs:353-396`
- **Adaptive backoff**: `src/strand/explore.rs:685-723`
- **Environment parsing**: `src/config/mod.rs:2670-2730`
