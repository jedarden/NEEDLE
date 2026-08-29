# Explore Strand Bead Store Access Decision Tree

This document maps the exact conditions that determine whether the Explore strand can successfully reach and scan bead stores during strand execution.

## Overview

The Explore strand runs in the selection waterfall after Pluck and Mend. It searches configured workspaces for claimable beads when the home workspace has no work.

**Entry point:** `ExploreStrand::evaluate()` in `src/strand/explore.rs`

## Decision Tree

```
Explore strand evaluation starts
│
├── 1. STRAND ENABLED CHECK
│   ├── Condition: `config.enabled == true`
│   ├── ✅ PASS: Continue to next check
│   └── ❌ FAIL: Return `StrandResult::NoWork`
│               Reason: "disabled"
│               Telemetry: `StrandSkipped { reason: "disabled" }`
│
├── 2. ADAPTIVE SCAN BACKOFF CHECK
│   ├── Condition: `should_scan_this_cycle() == true`
│   ├── ✅ PASS: Continue to next check
│   └── ❌ FAIL: Return `StrandResult::NoWork`
│               Reason: "adaptive_scan_backoff" 
│               Telemetry: `StrandSkipped { reason: "adaptive_scan_backoff" }`
│               (Backoff doubles after each empty scan: 1→2→4→8 cycles, capped at max_scan_interval_cycles)
│
├── 3. WORKSPACE RE-DISCOVERY (runs every cycle as of bf-6anj4)
│   ├── Condition: `auto_discovery_mode == true` (empty workspaces config)
│   ├── ✅ AUTO-DISCOVERY MODE:
│   │   ├── Scan `config.workspace_root` for directories containing `.beads/`
│   │   ├── If root doesn't exist → empty workspace list (not error)
│   │   ├── If root unreadable → empty workspace list (not error)
│   │   └── Update `self.workspaces` with discovered paths
│   └── ❌ PINNED MODE (explicit workspaces list):
│       ├── Skip re-discovery
│       └── Use static workspaces list from config
│
├── 4. WORKSPACES DISCOVERED CHECK
│   ├── Condition: `workspaces.is_empty() == false`
│   ├── ✅ PASS: Continue to next check
│   └── ❌ FAIL: Return `StrandResult::NoWork`
│               Reason: "no_workspaces_discovered"
│               Telemetry: `StrandSkipped { reason: "no_workspaces_discovered" }`
│               (Happens when workspace_root has no .beads/ directories)
│
├── 5. WORKSPACE ITERATION (shuffled order each cycle - bf-6anj4)
│   │
│   For each workspace in shuffled list:
│   │
│   ├── 5.1. HOME WORKSPACE SKIP
│   │   ├── Condition: `workspace != home_workspace`
│   │   ├── ✅ PASS: Continue to next check
│   │   └── ❌ FAIL: Skip to next workspace
│   │               Reason: "home_workspace"
│   │               (Pluck already checked home)
│   │
│   ├── 5.2. BEADS DIRECTORY CHECK
│   │   ├── Condition: `workspace.join(".beads").is_dir() == true`
│   │   ├── ✅ PASS: Continue to next check
│   │   └── ❌ FAIL: Skip to next workspace
│   │               Reason: "no_beads_dir"
│   │               Telemetry: Logs debug, continues
│   │
│   ├── 5.3. BEAD STORE CREATION (via `discover_default`)
│   │   ├── This is where workspace-specific backend binding is resolved
│   │   │
│   │   ├── 5.3.1. WORKSPACE CONFIG LOADING
│   │   │   ├── Load `.needle.yaml` from workspace
│   │   │   ├── Parse with `ConfigLoader::load_resolved()`
│   │   │   ├── ❌ FAIL: Skip to next workspace
│   │   │   │               Reason: "store_error: <config error>"
│   │   │   │               (Invalid YAML, missing required fields, etc.)
│   │   │   └── ✅ PASS: Extract `config.bead_cli`
│   │   │
│   │   ├── 5.3.2. BACKEND BINDING CHECK
│   │   │   ├── Condition: `config.bead_cli.backend != Auto`
│   │   │   ├── ✅ PASS: Resolve backend and binary path
│   │   │   └── ❌ FAIL: Skip to next workspace
│   │   │                   Reason: "store_error: no authoritative bead backend binding"
│   │   │                   (Workspace must explicitly declare backend in .needle.yaml)
│   │   │
│   │   ├── 5.3.3. BINARY RESOLUTION
│   │   │   ├── Path resolution via `resolve_bead_cli()`:
│   │   │   │   ├── If `bead_cli.path` set → use explicit path
│   │   │   │   └── If not set → PATH → ~/.local/bin/bead → /usr/local/cargo/bin/bead
│   │   │   ├── ❌ FAIL: Skip to next workspace
│   │   │   │               Reason: "store_error: <binary not found>"
│   │   │   └── ✅ PASS: Binary path resolved
│   │   │
│   │   ├── 5.3.4. BACKEND IDENTITY VERIFICATION
│   │   │   ├── Run binary with `--version` command
│   │   │   ├── Parse version output against backend descriptor's `identity_pattern`
│   │   │   ├── Extract name from output (e.g., "bead", "bf")
│   │   │   ├── Match against expected names for backend:
│   │   │   │   ├── bead-rs → expects ["bead", "bead-rs"]
│   │   │   │   └── bead-forge → expects ["bf", "bead-forge"]
│   │   │   ├── ❌ FAIL: Skip to next workspace
│   │   │   │               Reason: "store_error: bead backend identity mismatch"
│   │   │   │               (Version output doesn't match expected pattern)
│   │   │   └── ✅ PASS: Backend identity confirmed
│   │   │
│   │   ├── 5.3.5. BEAD-RS CAPABILITIES VERIFICATION (bead-rs only)
│   │   │   ├── Run: `bead capabilities --profile native-v1`
│   │   │   ├── Parse JSON output
│   │   │   ├── Verify required capabilities:
│   │   │   │   ├── `implementation == "bead-rs"`
│   │   │   │   ├── `atomic_claim == true`
│   │   │   │   ├── `statuses` contains: ["open", "in_progress", "deferred", "closed"]
│   │   │   │   └── `schemas` contains:
│   │   │   │       ├── "urn:bead-rs:schema:issue:native-v1"
│   │   │   │       ├── "urn:bead-rs:schema:event:native-v1"
│   │   │   │       └── "urn:bead-rs:schema:field-guide:native-v1"
│   │   │   ├── ❌ FAIL: Skip to next workspace
│   │   │   │               Reason: "store_error: bead-rs capability mismatch"
│   │   │   │               (Missing or invalid capabilities)
│   │   │   └── ✅ PASS: Capabilities verified
│   │   │
│   │   └── 5.3.6. STORE CONSTRUCTION
│   │       ├── Create `CliBeadStore` with resolved backend descriptor
│   │       ├── ❌ FAIL: Skip to next workspace
│   │       │               Reason: "store_error: <construction error>"
│   │       └── ✅ PASS: Store created successfully
│   │
│   ├── 5.4. BEAD STORE QUERY
│   │   ├── Call: `remote_store.ready(&filters).await`
│   │   ├── Filters applied:
│   │   │   ├── `assignee: None` (unassigned only)
│   │   │   ├── `exclude_labels: ["deferred", "human", "blocked"]`
│   │   │   └── `exclude_ids: <empty>`
│   │   ├── Result handling:
│   │   │   ├── ✅ SUCCESS (candidates found):
│   │   │   │   ├── Apply defensive filtering (assignee, labels)
│   │   │   │   ├── If non-empty after filtering:
│   │   │   │   │   ├── Tag beads with workspace path
│   │   │   │   │   ├── Add to global candidates list
│   │   │   │   │   └── Continue to next workspace (aggregate all)
│   │   │   │   └── If empty after filtering:
│   │   │   │       ├── Try cross-workspace mend (cleanup_orphaned_in_progress)
│   │   │   │       ├── Re-query after cleanup
│   │   │   │       └── Continue to next workspace
│   │   │   ├── ❌ TRANSIENT ERROR:
│   │   │   │   ├── Store checks: `is_lock_error()`, `is_corruption_error()`, `is_sync_conflict()`
│   │   │   │   ├── Skip to next workspace
│   │   │   │   │               Reason: "query_error: <error message>"
│   │   │   │   └── (No automatic recovery - operator must fix backend issue)
│   │   │   └── ❌ PERMANENT ERROR:
│   │   │       ├── Store is broken or backend is incompatible
│   │   │       ├── Skip to next workspace
│   │   │       │               Reason: "query_error: <error message>"
│   │   │       └── Telemetry: `ExploreScanSummary` with exclusion_reasons
│   │   │
│   └── Continue to next workspace in shuffled order
│
├── 6. AGGREGATE CANDIDATES FROM ALL WORKSPACES
│   ├── Combine all candidates found across all workspaces
│   ├── Sort by: priority ASC → created_at ASC → id ASC
│   └── (This ensures first-match starvation bug is fixed - bf-4df1e / bf-47bfm)
│
├── 7. CANDIDATES FOUND CHECK
│   ├── Condition: `all_candidates.is_empty() == false`
│   ├── ✅ PASS: Return `StrandResult::BeadFound(all_candidates)`
│   │               (Waterfall selects from ranked list)
│   └── ❌ FAIL: Return `StrandResult::NoWork`
│               Reason: "no_candidates_in_any_workspace"
│               Telemetry: `StrandSkipped { reason: "no_candidates_in_any_workspace" }`
│               Record empty scan (increases backoff interval)
│
└── END OF EXPLORE STRAND EVALUATION
```

## Access Condition Categories

### 1. Configuration-Level Conditions

These are checked at strand initialization or during config reload:

- **Strand enabled:** `config.strands.explore.enabled == true`
- **Workspace binding:** Each workspace must have `.needle.yaml` with `bead_cli.backend != Auto`
- **Backend availability:** Binary must exist at resolved path
- **Backend identity:** Binary version output must match expected pattern

### 2. Runtime Conditions

These are checked during each `evaluate()` call:

- **Adaptive backoff:** Strand may skip cycles based on empty scan history
- **Workspace discovery:** Auto-discovery runs every cycle (pinned mode uses static list)
- **Home workspace exclusion:** Always skipped (already checked by Pluck)
- **Beads directory presence:** `.beads/` must exist and be a directory

### 3. Backend-Specific Conditions

#### bead-rs backend
- **Binary resolution:** `bead` on PATH → `~/.local/bin/bead` → `/usr/local/cargo/bin/bead`
- **Identity verification:** `--version` output must match bead-rs pattern
- **Capabilities negotiation:** `bead capabilities --profile native-v1` must return:
  - `implementation: "bead-rs"`
  - `atomic_claim: true`
  - All four statuses: `["open", "in_progress", "deferred", "closed"]`
  - All three schema URNs

### 4. Filesystem and Permission Conditions

- **Workspace root readability:** Must be able to `read_dir(workspace_root)`
- **Workspace config readability:** Must be able to read `.needle.yaml`
- **Beads directory existence:** `.beads/` must exist and be a directory
- **Binary executability:** Resolved bead binary must be executable
- **Store accessibility:** Backend must be able to read/write its database

### 5. Runtime Failure Modes

These conditions cause a workspace to be skipped with a warning logged:

- **Config errors:** Invalid YAML, missing required fields
- **Backend binding errors:** No explicit backend in `.needle.yaml`
- **Binary resolution failures:** Binary not found at any expected path
- **Identity mismatches:** Binary doesn't claim to be the expected backend
- **Capability drift:** Backend missing required capabilities (bead-rs)
- **Store creation failures:** Cannot construct BeadStore instance
- **Query failures:** Store returns error (lock, corruption, incompatibility)
- **Empty results:** No candidates after filtering (triggers mend attempt)

## Failure Mode Summary

### Strand-Level Failures (return NoWork immediately)

1. **Disabled:** Strand is not enabled in config
2. **Adaptive backoff:** Strand is deferring due to empty scan history
3. **No workspaces:** Workspace discovery found no `.beads/` directories

### Per-Workspace Failures (skip to next workspace)

1. **Home workspace:** Always skipped (Pluck's responsibility)
2. **No beads directory:** `.beads/` doesn't exist or isn't a directory
3. **Config errors:** Cannot load or parse `.needle.yaml`
4. **No backend binding:** Workspace hasn't declared `bead_cli.backend`
5. **Binary not found:** Bead CLI binary doesn't exist at resolved path
6. **Identity mismatch:** Binary version output doesn't match backend
7. **Capability mismatch:** Backend missing required capabilities
8. **Store construction failure:** Cannot create BeadStore instance
9. **Query failure:** Backend returns error (lock, corruption, etc.)
10. **No candidates after filtering:** All beads are assigned or excluded (triggers mend)

## Telemetry Events

### Success Path
- `ExploreScanSummary`: Emitted after scanning all workspaces
  - `workspaces_visited`: List of workspaces scanned
  - `workspaces_with_candidates`: Workspaces that had beads
  - `total_candidates`: Total beads found (pre-filtering)
  - `exclusion_reasons`: Set of reasons workspaces were skipped
  - `duration_ms`: Scan duration
  - `scan_start_at`: Timestamp

### Failure Path
- `StrandSkipped`: Emitted when strand returns early
  - `"disabled"`: Strand not enabled
  - `"adaptive_scan_backoff"`: Deferring due to empty scan history
  - `"no_workspaces_discovered"`: No `.beads/` directories found
  - `"no_candidates_in_any_workspace"`: All workspaces empty or failed

## Key Implementation Notes

1. **Aggregation over early return:** Explore scans ALL workspaces and aggregates candidates before returning (bf-4df1e / bf-47bfm). Previously it would return on the first non-empty workspace, causing starvation when candidates were excluded by the waterfall.

2. **Workspace shuffling:** Each cycle shuffles the workspace list to avoid static ordering that could cause starvation (bf-6anj4).

3. **Home workspace exclusion:** The home workspace is always skipped because Pluck already checked it. This is deliberate separation of concerns.

4. **Backend binding is mandatory:** Auto-detection (`bead_cli.backend = Auto`) is not allowed for workspace access. Each workspace must explicitly declare its backend in `.needle.yaml`.

5. **Capabilities verification:** bead-rs backends must pass a capabilities check to ensure they support atomic claims and the required status set. This prevents silent capability drift.

6. **Identity verification:** The binary is actually executed with `--version` to verify it claims to be the expected backend. This prevents configuration mistakes where the wrong binary is bound.

7. **Defensive filtering:** Even though `store.ready()` receives filters, Explore applies additional client-side filtering to catch cases where the backend doesn't filter correctly.

8. **Cross-workspace mend:** When no ready candidates are found but in-progress beads exist, Explore runs cleanup_orphaned_in_progress to release stale assignments, then re-queries.

## Dependencies

- **Store creation:** `discover_default()` in `src/bead_store/mod.rs`
- **Config loading:** `ConfigLoader::load_resolved()` in `src/config/mod.rs`
- **Backend resolution:** `resolve_bead_cli()` in `src/config/mod.rs`
- **Identity verification:** `verify_backend_identity()` in `src/bead_store/mod.rs`
- **Capabilities check:** `verify_bead_rs_capabilities()` in `src/bead_store/mod.rs`
- **Workspace discovery:** `discover_workspaces()` in `src/strand/explore.rs`
- **Beads directory check:** `has_beads_dir()` in `src/strand/explore.rs`
