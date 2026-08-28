# ExploreConfig Structure and Initialization

This document describes the `ExploreConfig` struct, its fields, default values, and initialization behavior in the NEEDLE project.

## Overview

`ExploreConfig` is a configuration structure that controls the **Explore strand** behavior in NEEDLE workers. The Explore strand is responsible for discovering beads in multiple workspaces when the home workspace has no work available.

## Struct Definition

```rust
pub struct ExploreConfig {
    /// Whether the Explore strand is enabled.
    #[serde(default = "ExploreConfig::default_enabled")]
    pub enabled: bool,

    /// **Pin/exception list** for restricting a worker to specific workspaces.
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,

    /// Root path for workspace auto-discovery (when `workspaces` is empty).
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

## Field Descriptions

### `enabled: bool`

**Purpose:** Controls whether the Explore strand is active.

**Default Value:** `true` (via `ExploreConfig::default_enabled()`)

**Behavior:**
- When `true`: The Explore strand scans other workspaces for beads when the home workspace has no work
- When `false`: The Explore strand is skipped, returning `NoWork` immediately

**Example Usage:**
```yaml
# In .needle.yaml
strands:
  explore:
    enabled: true
```

---

### `workspaces: Vec<PathBuf>`

**Purpose:** Pin/exception list for restricting a worker to specific workspaces.

**Default Value:** Empty vector `Vec::new()`

**Behavior:**

#### Empty (Default) - Auto-Discovery Mode
When empty, Explore runs **recursive workspace discovery** under `workspace_root`:
- All directories containing a `.beads/` subdirectory are automatically scanned
- This is the **intended default for the fleet** — new workspaces are picked up automatically without configuration changes
- Operators should leave this empty unless there's a specific reason to pin a worker

#### Non-Empty - Pinned Mode (Exception)
When non-empty, auto-discovery is **disabled**:
- Only the specified workspace paths are scanned
- Use this to restrict a specific worker to a fixed repo set (e.g., a dedicated worker for a high-priority workspace)
- This is an **exception mechanism** — most workers should leave this empty

**WARNING:** When non-empty, a WARN log is emitted at startup naming the pinned repos, so operators can immediately see when a worker is running in restricted mode.

**Example Usage:**
```yaml
strands:
  explore:
    workspaces: []  # Auto-discovery (default)

    # OR for pinned mode:
    workspaces:
      - /home/coding/SEAM
      - /home/coding/ARMOR
```

---

### `workspace_root: PathBuf`

**Purpose:** Root path for workspace auto-discovery (when `workspaces` is empty).

**Default Value:** User's home directory (via `dirs_or_home("")`)

**Behavior:**
- All directories under this path containing a `.beads/` subdirectory are treated as workspaces
- Used only when `workspaces` is empty (auto-discovery mode)
- Defaults to `$HOME` (or `/tmp` if `HOME` is not set)

**Implementation Detail:**
```rust
fn default_workspace_root() -> PathBuf {
    dirs_or_home("")  // Returns $HOME if set, else /tmp
}

fn dirs_or_home(relative: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(relative)
    } else {
        PathBuf::from("/tmp").join(relative)
    }
}
```

**Example Usage:**
```yaml
strands:
  explore:
    workspace_root: /home/coding  # Override default (default: $HOME)
```

---

### `rediscovery_cycles: u32`

**Purpose:** Re-run workspace discovery every N cycles (0 = disabled).

**Default Value:** `60` (via `ExploreConfig::default_rediscovery_cycles()`)

**Behavior:**
- When set (default: 60), the workspace list is refreshed periodically
- New stores are picked up without requiring worker restarts
- Re-discovery preserves constraints:
  - **No upward traversal:** Only scans immediate children of `workspace_root`
  - **Explicit workspaces override:** When `workspaces` is non-empty, re-discovery is skipped

**Context:** A modest default (60 cycles ≈ 1 hour at typical worker cadence) balances freshness with filesystem churn. Set to 0 to disable periodic re-discovery.

**Example Usage:**
```yaml
strands:
  explore:
    rediscovery_cycles: 60  # Re-discover every 60 cycles (default)
```

---

### `starvation_threshold_minutes: u64`

**Purpose:** Starvation alarm threshold in minutes (0 = disabled).

**Default Value:** `15` (via `ExploreConfig::default_starvation_threshold_minutes()`)

**Behavior:**
- When set (default: 15), emits a WARN telemetry event when:
  - Ready beads exist in scanned workspaces
  - But this worker has not successfully claimed any bead for the specified number of minutes
- Helps detect cases where workers are stuck in exclusion loops or competing for the same beads without making progress
- Set to 0 to disable starvation detection

**Example Usage:**
```yaml
strands:
  explore:
    starvation_threshold_minutes: 15  # Alert after 15 min without claims (default)
```

---

### `scan_interval_cycles: u32`

**Purpose:** Minimum number of selection cycles between Explore scans.

**Default Value:** `1` (via `ExploreConfig::default_scan_interval_cycles()`)

**Behavior:**
- A value of 1 preserves the current behavior before adaptive backoff is applied
- Empty scans increase the effective interval geometrically (see `max_scan_interval_cycles`)
- Controls the base interval for the adaptive backoff mechanism

**Example Usage:**
```yaml
strands:
  explore:
    scan_interval_cycles: 1  # Scan every cycle (default)
```

---

### `max_scan_interval_cycles: u32`

**Purpose:** Maximum number of selection cycles between Explore scans after adaptive backoff.

**Default Value:** `8` (via `ExploreConfig::default_max_scan_interval_cycles()`)

**Behavior:**
- The effective interval never exceeds this value
- Works with `scan_interval_cycles` to implement adaptive backoff:
  - Empty scans double the interval: 1 → 2 → 4 → 8 → 8 (capped)
  - Finding a candidate resets to base interval
- Prevents indefinite backoff while reducing scan frequency when workspaces are empty

**Example Usage:**
```yaml
strands:
  explore:
    max_scan_interval_cycles: 8  # Cap backoff at 8 cycles (default)
```

---

## Default Initialization

### `ExploreConfig::default()`

Creates a default `ExploreConfig` instance with all fields set to their default values:

```rust
impl Default for ExploreConfig {
    fn default() -> Self {
        ExploreConfig {
            enabled: Self::default_enabled(),                    // true
            workspaces: Vec::new(),                              // []
            workspace_root: Self::default_workspace_root(),      // $HOME
            rediscovery_cycles: Self::default_rediscovery_cycles(),           // 60
            starvation_threshold_minutes: Self::default_starvation_threshold_minutes(),  // 15
            scan_interval_cycles: Self::default_scan_interval_cycles(),        // 1
            max_scan_interval_cycles: Self::default_max_scan_interval_cycles(), // 8
        }
    }
}
```

### Default Constructor Functions

```rust
impl ExploreConfig {
    fn default_enabled() -> bool {
        true
    }

    fn default_starvation_threshold_minutes() -> u64 {
        15
    }

    fn default_workspace_root() -> PathBuf {
        dirs_or_home("")  // Returns $HOME or /tmp
    }

    fn default_rediscovery_cycles() -> u32 {
        60
    }

    fn default_scan_interval_cycles() -> u32 {
        1
    }

    fn default_max_scan_interval_cycles() -> u32 {
        8
    }
}
```

---

## Configuration Modes

### Auto-Discovery Mode (Default)

When `workspaces` is empty (the default):

```yaml
strands:
  explore:
    enabled: true
    workspaces: []                    # Empty = auto-discovery
    workspace_root: /home/coding      # Scan under this path
```

**Behavior:**
- Recursively discovers all directories with `.beads/` under `workspace_root`
- Re-discovers every `rediscovery_cycles` (default: 60)
- New workspaces are picked up automatically

**Startup Logs:**
```
INFO Explore auto-discovery: workspaces re-discovered every cycle (rediscovery_cycles throttle no longer applied)
```

### Pinned Mode (Exception)

When `workspaces` is non-empty:

```yaml
strands:
  explore:
    enabled: true
    workspaces:                      # Non-empty = pinned
      - /home/coding/SEAM
      - /home/coding/ARMOR
```

**Behavior:**
- Only scans the explicitly listed workspaces
- Auto-discovery is disabled
- Re-discovery is skipped even if `rediscovery_cycles` > 0

**Startup Logs:**
```
WARN Explore running in PINNED mode (non-empty workspaces list). Auto-discovery is disabled;
only the listed workspaces will be scanned. This is an exception mechanism — the fleet default
is empty workspaces (recursive discovery under workspace_root). Verify this is intentional.
```

---

## Usage in ExploreStrand

The `ExploreConfig` is consumed by `ExploreStrand::new()`:

```rust
pub fn new(
    config: ExploreConfig,
    home_workspace: PathBuf,
    registry: Registry,
    telemetry: Telemetry,
    qualified_id: String,
) -> Self
```

**Key initialization logic:**

1. **Determine auto-discovery mode:**
   ```rust
   let auto_discovery_mode = config.workspaces.is_empty();
   ```

2. **Discover or use explicit workspaces:**
   ```rust
   let workspaces = if auto_discovery_mode {
       Self::discover_workspaces(&config.workspace_root)
   } else {
       config.workspaces
   };
   ```

3. **Warn if in pinned mode:**
   ```rust
   if !workspaces.is_empty() && !auto_discovery_mode {
       tracing::warn!(...);  // Emits WARN log
   }
   ```

---

## Complete Default Configuration Example

```yaml
# .needle.yaml
strands:
  explore:
    enabled: true                          # Enable strand
    workspaces: []                         # Auto-discovery mode
    workspace_root: /home/coding           # Scan root
    rediscovery_cycles: 60                # Re-discover every 60 cycles
    starvation_threshold_minutes: 15       # Alert after 15 min starvation
    scan_interval_cycles: 1              # Base scan interval
    max_scan_interval_cycles: 8           # Max backoff interval
```

This configuration represents the **intended default for the fleet** — workers automatically discover and scan all workspaces under `$HOME` with `.beads/` directories, re-discovering periodically to pick up new stores.
