# ADR-019: Explore Strand Activation Conditions

## Overview

This document traces all code paths that determine whether the Explore strand activates. It provides a complete map of activation/disabling conditions, environment variable influences, and guard clauses that prevent scanning.

## Executive Summary

The Explore strand has **7 distinct layers** of activation control, from configuration to runtime checks. Understanding all layers is critical for debugging why Explore may or may not scan workspaces.

### Activation Layers (in order of evaluation)

1. **Configuration defaults** - `ExploreConfig::default_enabled()` returns `true`
2. **Config file setting** - `strands.explore.enabled` in `.needle.yaml`
3. **Environment variable override** - `NEEDLE_STRANDS__EXPLORE__ENABLED`
4. **Runtime enabled check** - `if !self.enabled` in `evaluate()`
5. **Adaptive scan backoff** - `should_scan_this_cycle()`
6. **Workspace discovery** - Auto-discovery or pinned mode
7. **Per-workspace guard clauses** - Home workspace, missing `.beads/`, store errors

---

## Layer 1: Configuration Defaults

### Default State: **ENABLED**

**Location:** `src/config/mod.rs`
**Function:** `ExploreConfig::default_enabled()`

```rust
fn default_enabled() -> bool {
    true  // Explore is ON by default
}
```

**Implication:** A freshly initialized NEEDLE installation with no config file will have Explore enabled.

**Test coverage:** `tests/config.rs::test_env_override_explore_enabled()` validates this default.

---

## Layer 2: Config File Setting

### Configuration Path: `strands.explore.enabled`

**File:** `.needle.yaml` (or `.needle.yml`)

**Default behavior (no config file):**
```yaml
# Absent → defaults to true
```

**Explicit disable:**
```yaml
strands:
  explore:
    enabled: false  # Disable Explore globally
```

**Explicit enable (redundant but valid):**
```yaml
strands:
  explore:
    enabled: true  # Explicitly enable (already default)
```

**Configuration loading:** `src/config/mod.rs:ConfigLoader::from_file()`

**Telemetry:** No explicit event logged for config file loading, but `ExploreStrand::new()` emits WARN if running in pinned mode (non-empty `workspaces`).

---

## Layer 3: Environment Variable Override

### Variable: `NEEDLE_STRANDS__EXPLORE__ENABLED`

**Syntax:** Double underscores (`__`) separate path components (YAML nesting).

**Valid values:**
- `true` (case-insensitive: `True`, `TRUE`, `tRuE` all work)
- `false` (case-insensitive: `False`, `FALSE`, `fAlSe` all work)

**Examples:**

```bash
# Disable Explore via environment
export NEEDLE_STRANDS__EXPLORE__ENABLED=false
needle worker

# Enable Explore explicitly (usually redundant with default)
export NEEDLE_STRANDS__EXPLORE__ENABLED=true
needle worker
```

**Parsing code:** `src/config/mod.rs:ConfigLoader::apply_env_overrides()`

**Error handling:** Invalid values (non-boolean) emit a WARN log but don't crash:
```
WARN: invalid value for strands.explore.enabled — expected true or false
```

**Priority:** Environment variables **override** config file settings. If both are present, the environment variable wins.

**Test coverage:** `tests/config.rs::env_override_explore_enabled()`

---

## Layer 4: Runtime Enabled Check

### Location: `src/strand/explore.rs:explore() evaluate()`

**Line:** 571

```rust
async fn evaluate(&self, _store: &dyn BeadStore, _exclusions: &HashSet<BeadId>) -> StrandResult {
    // If disabled, nothing to explore.
    if !self.enabled {
        let _ = self.telemetry.emit(crate::telemetry::EventKind::StrandSkipped {
            strand_name: "explore".to_string(),
            reason: "disabled".to_string(),
        });
        return StrandResult::NoWork;
    }
    // ... rest of evaluation
}
```

**Telemetry emitted:** `StrandSkipped { strand_name: "explore", reason: "disabled" }`

**Behavior:** This is the **first runtime check**. If `self.enabled` is `false`, the strand returns `NoWork` immediately, before any workspace scanning or backoff logic.

**How `self.enabled` is set:**
- In `ExploreStrand::new()`, line 246: `enabled: config.enabled`
- `config.enabled` comes from the final merged config (defaults → file → env vars)

---

## Layer 5: Adaptive Scan Backoff

### Location: `src/strand/explore.rs:evaluate()`

**Lines:** 584-596

```rust
// A backoff skip is still reported as NoWork so the waterfall continues
// evaluating Weave and all later escalation strands in their normal
// order; only Explore's remote scan is deferred.
if !self.should_scan_this_cycle() {
    let _ = self.telemetry.emit(crate::telemetry::EventKind::StrandSkipped {
        strand_name: "explore".to_string(),
        reason: "adaptive_scan_backoff".to_string(),
    });
    tracing::debug!(
        worker = %self.qualified_id,
        "Explore scan deferred by adaptive empty-scan backoff"
    );
    return StrandResult::NoWork;
}
```

**Telemetry emitted:** `StrandSkipped { strand_name: "explore", reason: "adaptive_scan_backoff" }`

**Purpose:** Prevents excessive scanning when workspaces are consistently empty. Reduces filesystem load and log spam.

**How it works:**
1. **Configuration:** `ExploreConfig::scan_interval_cycles` (default: 1) and `max_scan_interval_cycles` (default: 8)
2. **State tracking:** `ExploreScanBackoff` struct tracks:
   - `consecutive_empty_scans` - how many scans in a row found nothing
   - `cycles_until_scan` - countdown to next scan
3. **Backoff formula:** `effective_interval = base_interval * 2^min(empty_scans, 31)`, capped at `max_interval`
4. **Reset:** Finding a candidate immediately resets backoff to base interval

**Example sequence:**
```
Cycle 1: Scan → empty → next scan in 1 cycle
Cycle 2: Scan → empty → next scan in 2 cycles
Cycle 3: Skip (backoff)
Cycle 4: Scan → empty → next scan in 4 cycles
Cycle 5-7: Skip (backoff)
Cycle 8: Scan → empty → next scan in 8 cycles
Cycle 9-16: Skip (backoff)
Cycle 17: Scan → FOUND → reset to 1 cycle
```

**Configuration impact:**
- `scan_interval_cycles: 1` - minimum interval (default)
- `max_scan_interval_cycles: 8` - maximum interval (default)
- Higher values = more aggressive backoff, less frequent scanning

**Does NOT emit:** `ExploreScanSummary` telemetry (since no scan occurred).

---

## Layer 6: Workspace Discovery

### Location: `src/strand/explore.rs:evaluate()`

**Lines:** 604-630

#### 6a. Re-discovery (every cycle)

```rust
// Re-discover workspaces every cycle (bf-3peh4 / bf-6anj4)
let _cycle = self.cycles_since_rediscovery.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
let added = self.rediscover_workspaces();
```

**Behavior:**
- **Auto-discovery mode** (`config.workspaces.is_empty()`): Runs `discover_workspaces()` every cycle
- **Pinned mode** (non-empty `config.workspaces`): Skips re-discovery (line 414)

**What `discover_workspaces()` does:**
```rust
fn discover_workspaces(root: &Path) -> Vec<PathBuf> {
    // 1. Check if root exists
    if !root.exists() {
        tracing::debug!(root = %root.display(), "workspace root does not exist");
        return vec![];  // Empty → no workspaces
    }

    // 2. Read directory entries
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(root = %root.display(), error = %e, "failed to read workspace root");
            return vec![];  // Empty → no workspaces
        }
    };

    // 3. Filter for directories containing .beads/
    for entry in entries {
        let path = entry.path();
        if path.is_dir() && Self::has_beads_dir(&path) {
            discovered.push(path);  // Found a workspace
        }
    }

    discovered
}

fn has_beads_dir(workspace: &Path) -> bool {
    workspace.join(".beads").is_dir()
}
```

**Key points:**
- **No upward traversal:** Only scans immediate children of `workspace_root`
- **Requires `.beads/` directory:** A directory without `.beads/` is not a workspace
- **Non-existent root:** Returns empty list (not an error)

#### 6b. Empty Workspace Check

**Lines:** 618-630

```rust
// Empty workspaces (after discovery attempt) means no workspaces found.
{
    let workspaces = self.workspaces.lock().unwrap();
    if workspaces.is_empty() {
        let _ = self.telemetry.emit(crate::telemetry::EventKind::StrandSkipped {
            strand_name: "explore".to_string(),
            reason: "no_workspaces_discovered".to_string(),
        });
        self.record_scan_result(false);
        return StrandResult::NoWork;
    }
}
```

**Telemetry emitted:** `StrandSkipped { strand_name: "explore", reason: "no_workspaces_discovered" }`

**Scenarios that trigger this:**
1. Auto-discovery mode with `workspace_root` that doesn't exist
2. Auto-discovery mode with `workspace_root` that has no `.beads/` subdirectories
3. Pinned mode with an empty `workspaces` list (shouldn't happen, but defensive)

**Behavior:** Records the empty scan result (increasing backoff counter) and returns `NoWork`.

---

## Layer 7: Per-Workspace Guard Clauses

### Location: `src/strand/explore.rs:evaluate()`

**Lines:** 686-910

Explore iterates through all workspaces (shuffled each cycle) and applies **three guard clauses** per workspace:

#### 7a. Home Workspace Skip

**Lines:** 692-696

```rust
for workspace in &workspaces {
    // Track this workspace as visited
    let workspace_str = workspace.display().to_string();
    workspaces_visited.push(workspace_str.clone());

    // Skip the home workspace — Pluck already checked it.
    if workspace == &self.home_workspace {
        tracing::debug!(workspace = %workspace.display(), "skipping home workspace");
        exclusion_reasons.insert("home_workspace".to_string());
        continue;
    }
}
```

**Telemetry:** "home_workspace" added to `exclusion_reasons` in `ExploreScanSummary`

**Rationale:** The home workspace is already scanned by the `Pluck` strand (line 105-111 in `strand/mod.rs`). Re-scanning it would be redundant and could cause duplicate work.

**How `home_workspace` is set:**
- In `ExploreStrand::new()`, line 247: `home_workspace` parameter
- In `StrandRunner::from_config()`, line 155: `config.workspace.default.clone()`
- `config.workspace.default` is typically the repo's root directory

#### 7b. Missing `.beads/` Directory Check

**Lines:** 699-703

```rust
// Check that .beads/ exists before attempting to query.
if !Self::has_beads_dir(workspace) {
    tracing::debug!(workspace = %workspace.display(), "no .beads/ directory, skipping");
    exclusion_reasons.insert("no_beads_dir".to_string());
    continue;
}
```

**Telemetry:** "no_beads_dir" added to `exclusion_reasons` in `ExploreScanSummary`

**Rationale:** A workspace without a `.beads/` directory cannot contain beads. This is a defensive check for:
- Workspaces that were deleted after discovery
- Symlinks that no longer point to valid directories
- Race conditions between discovery and scanning

**Note:** This check is redundant with `discover_workspaces()` (which already filters by `.beads/` existence), but is kept for defense-in-depth.

#### 7c. Store Creation Errors

**Lines:** 706-717

```rust
// Create a store for this workspace and query for ready beads.
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
```

**Telemetry:** "store_error: {error}" added to `exclusion_reasons` in `ExploreScanSummary`

**Scenarios that trigger this:**
1. **Corrupt bead store:** SQLite database is locked or malformed
2. **Backend mismatch:** Workspace uses `bead-forge` but needle is configured for `bead-rs` (or vice versa)
3. **Permission denied:** Cannot read `.beads/` directory or files
4. **Workspace disappeared:** Directory was deleted between discovery and scanning

**Behavior:** Logs a WARN (not DEBUG) because this indicates a real problem. Continues to the next workspace rather than failing the entire scan.

---

## Complete Activation Flow Diagram

```
┌─────────────────────────────────────────────────────────────┐
│ LAYER 1: Configuration Defaults                             │
│ ExploreConfig::default_enabled() → true                     │
└────────────────────┬────────────────────────────────────────┘
                     │ (enabled by default)
                     v
┌─────────────────────────────────────────────────────────────┐
│ LAYER 2: Config File Setting                                │
│ .needle.yaml: strands.explore.enabled                       │
└────────────────────┬────────────────────────────────────────┘
                     │ (value from file, or default)
                     v
┌─────────────────────────────────────────────────────────────┐
│ LAYER 3: Environment Variable Override                      │
│ NEEDLE_STRANDS__EXPLORE__ENABLED                             │
└────────────────────┬────────────────────────────────────────┘
                     │ (final merged value)
                     v
┌─────────────────────────────────────────────────────────────┐
│ LAYER 4: Runtime Enabled Check (evaluate() line 571)      │
│ if !self.enabled → return NoWork                            │
└────────────────────┬────────────────────────────────────────┘
                     │ (enabled check passed)
                     v
┌─────────────────────────────────────────────────────────────┐
│ LAYER 5: Adaptive Scan Backoff (evaluate() line 584)        │
│ if !should_scan_this_cycle() → return NoWork               │
└────────────────────┬────────────────────────────────────────┘
                     │ (backoff check passed)
                     v
┌─────────────────────────────────────────────────────────────┐
│ LAYER 6a: Workspace Re-discovery (evaluate() line 604)     │
│ rediscover_workspaces() → updates workspaces list           │
└────────────────────┬────────────────────────────────────────┘
                     │
                     v
┌─────────────────────────────────────────────────────────────┐
│ LAYER 6b: Empty Workspace Check (evaluate() line 618)      │
│ if workspaces.is_empty() → return NoWork                   │
└────────────────────┬────────────────────────────────────────┘
                     │ (workspaces found)
                     v
┌─────────────────────────────────────────────────────────────┐
│ LAYER 7: Per-Workspace Guard Clauses (for each workspace)   │
│                                                              │
│  7a. Skip home workspace (line 692)                         │
│  7b. Skip if no .beads/ directory (line 699)                │
│  7c. Skip if store creation fails (line 706)                │
│                                                              │
│  All passed → query workspace for ready beads               │
└────────────────────┬────────────────────────────────────────┘
                     │ (candidates found or not)
                     v
┌─────────────────────────────────────────────────────────────┐
│ RESULT:                                                      │
│ - Candidates found → return StrandResult::BeadFound         │
│ - No candidates → return StrandResult::NoWork               │
└─────────────────────────────────────────────────────────────┘
```

---

## Configuration vs Runtime: Key Differences

### Configuration layers (1-3)
- **Evaluated once** at worker startup
- **Cannot change** without restarting the worker
- **Affect all future cycles** equally

### Runtime layers (4-7)
- **Evaluated every selection cycle** (typically once per minute)
- **Can change dynamically** (e.g., backoff state, workspace list)
- **Can differ per cycle** (e.g., one cycle scans, next skips due to backoff)

---

## Debugging Explore Activation

### Symptom: "Explore never scans"

**Checklist:**
1. ✅ Verify `config.strands.explore.enabled` is `true`
2. ✅ Check `NEEDLE_STRANDS__EXPLORE__ENABLED` environment variable (should be unset or `true`)
3. ✅ Look for `StrandSkipped { strand_name: "explore", reason: "disabled" }` in telemetry
4. ✅ Look for `StrandSkipped { strand_name: "explore", reason: "adaptive_scan_backoff" }` in telemetry
5. ✅ Look for `StrandSkipped { strand_name: "explore", reason: "no_workspaces_discovered" }` in telemetry
6. ✅ Verify `workspace_root` exists and contains directories with `.beads/` subdirectories
7. ✅ Check that `workspaces` config is not an empty list (unless auto-discovery is intended)

### Symptom: "Explore scans too frequently"

**Checklist:**
1. ✅ Verify `scan_interval_cycles` and `max_scan_interval_cycles` config values
2. ✅ Look for consecutive empty scans in logs (should trigger backoff)
3. ✅ Check that `record_scan_result(found_candidate)` is being called correctly

### Symptom: "Explore skips valid workspaces"

**Checklist:**
1. ✅ Verify `home_workspace` is not set to a workspace you want to scan
2. ✅ Check that all workspaces have `.beads/` directories
3. ✅ Look for "store_error" messages in `ExploreScanSummary` telemetry
4. ✅ Verify workspace paths are correct and readable

---

## Telemetry Events Related to Activation

### `StrandSkipped`
**Emitted when:** Explore is disabled, deferred by backoff, or has no workspaces

**Fields:**
- `strand_name`: "explore"
- `reason`: One of:
  - `"disabled"` - Layer 4 (runtime enabled check failed)
  - `"adaptive_scan_backoff"` - Layer 5 (backoff deferred this cycle)
  - `"no_workspaces_discovered"` - Layer 6b (empty workspace list)

### `ExploreScanSummary`
**Emitted when:** A full scan completes (regardless of whether candidates were found)

**Fields:**
- `workspaces_visited`: List of workspace paths examined
- `workspaces_with_candidates`: List of workspace paths that had ready beads
- `total_candidates`: Total number of candidates found across all workspaces
- `exclusion_reasons`: Set of reasons why workspaces were skipped:
  - `"home_workspace"` - Layer 7a (home workspace skip)
  - `"no_beads_dir"` - Layer 7b (missing `.beads/` directory)
  - `"store_error: {error}"` - Layer 7c (store creation failed)
  - `"no_ready_candidates"` - No ready beads after initial query
  - `"no_orphans"` - Cross-workspace mend found no orphans to release
  - `"orphans_released_no_candidates"` - Mend released orphans but re-query found nothing
  - `"filtered_{count}"` - Candidates were filtered by labels/assignee
  - And others...
- `duration_ms`: Total scan duration in milliseconds

---

## Environment Variable Quick Reference

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `NEEDLE_STRANDS__EXPLORE__ENABLED` | bool | `true` | Master enable/disable switch for Explore strand |
| `NEEDLE_STRANDS__EXPLORE__WORKSPACE_ROOT` | path | `$HOME` | Root directory for workspace auto-discovery |

**Note:** Double underscores (`__`) are required to represent YAML nesting. `NEEDLE_STRANDS__EXPLORE__ENABLED` maps to `strands.explore.enabled`.

---

## Test Coverage

### Unit tests in `src/strand/explore.rs`

| Test | Validates |
|------|-----------|
| `disabled_returns_no_work` | Layer 4 (disabled check) |
| `empty_workspace_list_returns_no_work` | Layer 6b (empty workspaces) |
| `adaptive_scan_backoff_*` | Layer 5 (backoff behavior) |
| `skips_home_workspace` | Layer 7a (home workspace skip) |
| `skips_workspace_without_beads_dir` | Layer 7b (missing `.beads/`) |
| `nonexistent_workspace_path_returns_no_work` | Layer 7c (store errors) |
| `default_config_is_enabled_with_empty_workspaces` | Layer 1 (defaults) |
| `discover_workspaces_*` | Layer 6a (discovery logic) |
| `empty_workspaces_config_triggers_discovery` | Layer 6a (auto-discovery mode) |
| `explicit_workspaces_list_skips_discovery` | Layer 6a (pinned mode) |

### Integration tests in `tests/config.rs`

| Test | Validates |
|------|-----------|
| `env_override_explore_enabled` | Layer 3 (environment variable override) |

---

## Related ADRs

- **ADR-006:** Test isolation policy (relevant for testing Explore activation)
- **ADR-015:** Concurrent worker isolation (no worktrees policy)
- **ADR-018:** Reopen assignee contract (affects bead availability for Explore)

---

## Appendix: Full Activation Condition Checklist

For Explore strand to activate and scan workspaces, **ALL** of the following must be true:

- [ ] **Layer 1:** `ExploreConfig::default_enabled()` returns `true` (always true)
- [ ] **Layer 2:** Config file does NOT set `strands.explore.enabled: false`
- [ ] **Layer 3:** `NEEDLE_STRANDS__EXPLORE__ENABLED` is NOT set to `false`
- [ ] **Layer 4:** `self.enabled` is `true` at runtime (checked in `evaluate()`)
- [ ] **Layer 5:** `should_scan_this_cycle()` returns `true` (not in backoff)
- [ ] **Layer 6a:** `rediscover_workspaces()` completes successfully
- [ ] **Layer 6b:** `workspaces` list is NOT empty after discovery
- [ ] **Layer 7a:** At least one workspace is NOT the home workspace
- [ ] **Layer 7b:** At least one workspace HAS a `.beads/` directory
- [ ] **Layer 7c:** At least one workspace CAN create a bead store successfully

For Explore to **find and return candidates**, **ALL** of the above must be true **PLUS**:

- [ ] At least one workspace has ready (unassigned, non-excluded) beads
- [ ] Candidates pass defensive filtering (assignee check, label check)

---

## Document Metadata

**Created:** 2026-08-28  
**Author:** NEEDLE exploration trace (bead: needle-ea446315)  
**Status:** Complete  
**Related beads:** needle-ea446315
