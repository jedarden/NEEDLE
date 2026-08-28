# ExploreConfig Structure and Initialization

## Overview

`ExploreConfig` configures the **Explore strand**, which enables NEEDLE workers to discover and claim beads from multiple workspaces beyond their home workspace. When the home workspace has no work and maintenance is clean, Explore searches configured workspaces for claimable beads.

**Source location:** `src/config/mod.rs:4390-4494`

## Struct Definition

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreConfig {
    /// Whether the Explore strand is enabled.
    #[serde(default = "ExploreConfig::default_enabled")]
    pub enabled: bool,

    /// Pin/exception list for restricting a worker to specific workspaces.
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,

    /// Root path for workspace auto-discovery (when workspaces is empty).
    #[serde(default = "ExploreConfig::default_workspace_root")]
    pub workspace_root: PathBuf,

    /// Re-run workspace discovery every N cycles (0 = disabled).
    #[serde(default = "ExploreConfig::default_rediscovery_cycles")]
    pub rediscovery_cycles: u32,

    /// Starvation alarm threshold in minutes (0 = disabled).
    #[serde(default = "ExploreConfig::default_starvation_threshold_minutes")]
    pub starvation_threshold_minutes: u64,

    /// Minimum number of selection cycles between Explore scans.
    #[serde(default = "ExploreConfig::default_scan_interval_cycles")]
    pub scan_interval_cycles: u32,

    /// Maximum number of selection cycles between Explore scans after adaptive backoff.
    #[serde(default = "ExploreConfig::default_max_scan_interval_cycles")]
    pub max_scan_interval_cycles: u32,
}
```

## Field Documentation

### `enabled: bool`
**Purpose:** Controls whether the Explore strand is active.

**Default:** `true` (via `default_enabled()`)

**Behavior:**
- When `false`, Explore immediately returns `StrandResult::NoWork` and emits a `StrandSkipped` telemetry event
- When `true`, Explore proceeds with workspace scanning

**Example:**
```yaml
# .needle.yaml
explore:
  enabled: false  # Disable multi-workspace discovery
```

---

### `workspaces: Vec<PathBuf>`
**Purpose:** Pin/exception list for restricting a worker to specific workspaces.

**Default:** `Vec::new()` (empty vector)

**Behavior - Two Modes:**

#### **Auto-discovery Mode (default - empty list)**
When `workspaces` is empty, Explore runs **recursive workspace discovery** under `workspace_root`:
- All directories containing a `.beads/` subdirectory are automatically scanned
- New workspaces are picked up without configuration changes
- This is the **intended default for the fleet**

#### **Pinned Mode (exception - non-empty list)**
When `workspaces` contains paths, auto-discovery is **disabled**:
- Only the explicitly listed paths are scanned
- Use this to restrict a specific worker to a fixed repo set
- Emits a **WARN log** at startup naming the pinned repos
- This is an **exception mechanism** — most workers should leave this empty

**WARNING:** The 2026-07-19 fleet incident occurred because `workspaces` was populated with 24 hardcoded paths, permanently disabling discovery fleet-wide. The list drifted stale, missing valid repos.

**Example:**
```yaml
# Auto-discovery mode (recommended for fleet)
explore:
  workspaces: []  # Empty = discover all workspaces under workspace_root

# Pinned mode (exception case)
explore:
  workspaces:
    - /home/coding/NEEDLE
    - /home/coding/commitgraph
```

---

### `workspace_root: PathBuf`
**Purpose:** Root path for workspace auto-discovery when `workspaces` is empty.

**Default:** `$HOME` (via `dirs_or_home("")`)

**Resolution:**
```rust
fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)  // e.g., $HOME/ for empty string
    } else {
        PathBuf::from("/tmp").join(relative)  // Fallback to /tmp
    }
}
```

**Default value:** `$HOME/` (user's home directory)

**Behavior:**
- Only immediate children of `workspace_root` are scanned (no upward traversal)
- Each child is checked for a `.beads/` subdirectory
- Directories with `.beads/` are added to the workspace list

**Example:**
```yaml
# Default: use home directory
explore:
  workspace_root: /home/coding  # Optional override
```

---

### `rediscovery_cycles: u32`
**Purpose:** Controls how often to refresh the workspace list.

**Default:** `60` (via `default_rediscovery_cycles()`)

**Meaning:** Re-run workspace discovery every N selection cycles.

**Calculation:** At typical worker cadence (~1 minute per cycle), 60 cycles ≈ 1 hour.

**Behavior:**
- When `> 0`: Refresh workspace list periodically to pick up new stores
- When `== 0`: Disable periodic re-discovery
- Re-discovery preserves constraints:
  - Only scans immediate children of `workspace_root` (no upward traversal)
  - Skipped when in pinned mode (non-empty `workspaces` list)

**Historical note:** As of bf-6anj4, re-discovery runs **every cycle** regardless of this setting. The `rediscovery_cycles` throttle is no longer applied but remains in config for backward compatibility with existing `.needle.yaml` files.

**Example:**
```yaml
explore:
  rediscovery_cycles: 60   # Refresh every hour (default)
  rediscovery_cycles: 0    # Disable periodic refresh
```

---

### `starvation_threshold_minutes: u64`
**Purpose:** Starvation alarm threshold for detecting worker stalls.

**Default:** `15` (via `default_starvation_threshold_minutes()`)

**Meaning:** Emit a WARN telemetry event when ready beads exist but this worker hasn't successfully claimed any bead for the specified number of minutes.

**Behavior:**
- When `> 0`: Enables starvation detection
- When `== 0`: Disabled (no alarm emitted)
- Helps detect exclusion loops or workers competing for the same beads without progress

**Example:**
```yaml
explore:
  starvation_threshold_minutes: 15  # Alert after 15min without success (default)
  starvation_threshold_minutes: 0   # Disable starvation alarm
```

---

### `scan_interval_cycles: u32`
**Purpose:** Minimum number of selection cycles between Explore scans.

**Default:** `1` (via `default_scan_interval_cycles()`)

**Behavior:**
- Base interval for adaptive scan backoff
- Empty scans increase the effective interval geometrically
- A value of 1 preserves current behavior before adaptive backoff is applied

**Adaptive cadence:**
- Consecutive empty scans double the interval (exponential backoff)
- Finding a candidate resets to base interval
- Interval never exceeds `max_scan_interval_cycles`

**Example:**
```yaml
explore:
  scan_interval_cycles: 1    # Scan every cycle (default)
  scan_interval_cycles: 3    # Base interval of 3 cycles
```

---

### `max_scan_interval_cycles: u32`
**Purpose:** Maximum number of selection cycles between Explore scans after adaptive backoff.

**Default:** `8` (via `default_max_scan_interval_cycles()`)

**Behavior:**
- Caps the exponential backoff growth
- Effective interval never exceeds this value
- Prevents Explore from becoming completely dormant

**Backoff progression with defaults:**
- Scan 1: interval = 1
- Empty → interval = 2
- Empty → interval = 4
- Empty → interval = 8 (capped)
- Further empties → interval stays at 8
- Finding candidate → interval resets to 1

**Example:**
```yaml
explore:
  max_scan_interval_cycles: 8   # Cap at 8 cycles (default)
  max_scan_interval_cycles: 16 # Cap at 16 cycles
```

---

## Default Initialization

### `Default` Trait Implementation

```rust
impl Default for ExploreConfig {
    fn default() -> Self {
        ExploreConfig {
            enabled: Self::default_enabled(),
            workspaces: Vec::new(),
            workspace_root: Self::default_workspace_root(),
            rediscovery_cycles: Self::default_rediscovery_cycles(),
            starvation_threshold_minutes: Self::default_starvation_threshold_minutes(),
            scan_interval_cycles: Self::default_scan_interval_cycles(),
            max_scan_interval_cycles: Self::default_max_scan_interval_cycles(),
        }
    }
}
```

### Static Default Functions

```rust
impl ExploreConfig {
    fn default_enabled() -> bool {
        true
    }

    fn default_workspace_root() -> PathBuf {
        dirs_or_home("")  // Returns $HOME/ or /tmp/ if HOME not set
    }

    fn default_rediscovery_cycles() -> u32 {
        60
    }

    fn default_starvation_threshold_minutes() -> u64 {
        15
    }

    fn default_scan_interval_cycles() -> u32 {
        1
    }

    fn default_max_scan_interval_cycles() -> u32 {
        8
    }
}
```

### Complete Default Values Summary

| Field | Default Value | Meaning |
|-------|--------------|---------|
| `enabled` | `true` | Explore strand is active by default |
| `workspaces` | `Vec::new()` (empty) | Auto-discovery mode enabled |
| `workspace_root` | `$HOME/` | Scan user's home directory for workspaces |
| `rediscovery_cycles` | `60` | Refresh workspace list every hour |
| `starvation_threshold_minutes` | `15` | Alert after 15 min without successful claim |
| `scan_interval_cycles` | `1` | Base scan interval: 1 cycle |
| `max_scan_interval_cycles` | `8` | Maximum scan interval: 8 cycles |

---

## Configuration Example

### Full `.needle.yaml` with Explore Config

```yaml
explore:
  enabled: true
  workspaces: []  # Empty = auto-discover all workspaces
  workspace_root: /home/coding
  rediscovery_cycles: 60
  starvation_threshold_minutes: 15
  scan_interval_cycles: 1
  max_scan_interval_cycles: 8
```

### Minimal `.needle.yaml` (all defaults)

```yaml
# No explore section needed — all fields have sensible defaults
```

### Exception Case: Pinned Worker

```yaml
explore:
  enabled: true
  workspaces:
    - /home/coding/NEEDLE
    - /home/coding/commitgraph
  # workspace_root ignored when workspaces is non-empty
```

---

## Related Documentation

- **Explore strand implementation:** `src/strand/explore.rs`
- **Workspace discovery logic:** `ExploreStrand::discover_workspaces()`
- **Adaptive scan backoff:** `ExploreScanBackoff` struct
- **Test isolation policy:** `docs/testing-isolation-patterns.md`

---

## Historical Context

### 2026-07-19 Fleet Incident

The fleet incident occurred because `explore.workspaces` was populated with 24 hardcoded paths. This:
1. Permanently disabled recursive discovery fleet-wide
2. Created a stale list that missed valid repos (commitgraph, twitterapi-proxy)
3. Required manual intervention for each new workspace

**Resolution:** Made `workspaces` default to empty (auto-discovery mode) and emit WARN logs when non-empty to alert operators to pinned mode.

### bf-6anj4: Per-Cycle Workspace Re-discovery

Prior to bf-6anj4, workspace re-discovery was throttled by `rediscovery_cycles`. This meant:
- New workspaces were not detected until the throttle expired
- Workers required restarts to see new stores

**Resolution:** Re-discovery now runs every cycle unconditionally. The `rediscovery_cycles` field remains for config compatibility but no longer gates re-discovery.

### bf-4df1e / bf-47bfm: Cross-Workspace Aggregation

Prior to these fixes, Explore would return on the first workspace with candidates, even if all were filtered out. This caused fleet-wide starvation when:
- First workspace had only excluded/assigned beads
- Later workspaces had valid claimable beads (never scanned)

**Resolution:** Explore now aggregates candidates across ALL workspaces, filters them globally, and returns the ranked list.
