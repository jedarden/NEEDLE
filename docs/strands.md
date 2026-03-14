# NEEDLE Strand Engine

The strand engine is the core dispatcher that determines what a worker does each cycle. It runs a **priority waterfall** of seven strands, each with a clear responsibility. When a strand finds work, the engine exits and the worker loop restarts. When a strand finds nothing, execution falls through to the next strand.

## Priority Waterfall

```
pluck --> mend --> explore --> weave --> pulse --> unravel --> knot
  1        2         3          4         5          6         7
```

Every strand follows the same contract:

- **Return 0**: Work was found and processed. Engine exits, worker loop restarts.
- **Return 1**: No work found. Engine continues to the next strand.
- **Return 2**: Workspace changed (explore only). Engine restarts from strand 1 with the new workspace.

## Strand Descriptions

### 1. Pluck

**Purpose:** Claim and execute open beads.

This is the primary work strand. Pluck queries the workspace for open, unblocked, unassigned beads, picks the highest-priority candidate, and atomically claims it via `br update --claim`. Once claimed, the bead is dispatched to the configured agent (e.g., Claude Code) for execution.

Pluck only operates on the worker's assigned workspace. It does not discover or search for work elsewhere.

**Produces work:** Yes (executes the bead directly).

### 2. Mend

**Purpose:** Reclaim beads stuck in `in_progress` held by dead or stale workers.

Before searching further afield, mend checks whether any beads in the current workspace are stuck. A bead is considered stuck when:

- **Orphaned claim:** The bead's assignee has no heartbeat file, or the PID in the heartbeat is no longer running.
- **Stale claim:** The bead has been `in_progress` longer than the configured threshold (default: 1 hour).

Mend releases these claims, making the beads available for pluck on the next cycle. It also prunes dead heartbeat files and rotates old logs.

**Produces work:** Indirectly. Freed beads become claimable by pluck on the next iteration.

### 3. Explore

**Purpose:** Find beads in other workspaces by searching the filesystem.

Explore runs in two phases:

**Phase 1 (Down):** Search child directories of the current workspace for `.beads/` folders. If a child workspace has claimable beads, the worker **changes its workspace** to that child and the engine restarts from pluck. This lets the worker claim beads there without spawning a new process.

**Phase 2 (Up):** If no children have beads, walk up to the parent directory. At each level, search sibling directories for `.beads/` folders with claimable beads. If found, switch workspace and restart. This walk is constrained by `explore.max_upward_depth` (default: 3 levels).

At each discovered workspace, explore also checks for stale `in_progress` beads held by dead workers and releases them (the same logic as mend, applied to foreign workspaces).

When explore changes the workspace, the engine restarts from pluck. This means on the next iteration, pluck runs against the new workspace, mend checks its stale beads, and if pluck still finds nothing, explore searches the new workspace's children and siblings. This recursive behavior naturally traverses the directory tree.

**Produces work:** Indirectly. By changing workspace, it enables pluck to claim beads in the new location.

**Configuration:**
- `explore.max_depth` (default: 3) - How deep to search children.
- `explore.max_upward_depth` (default: 3) - How many parent levels to walk up.

### 4. Weave

**Purpose:** Reconstruct the project's intended end-state and find gaps against reality.

Weave is the first "generative" strand. When there are no beads to claim anywhere, weave reconstructs what the project is trying to achieve by examining multiple sources of intent, then compares that against what actually exists. It dispatches an LLM agent with:

- **Closed beads:** Completed work reveals the project's trajectory and direction.
- **Genesis/plan beads:** High-level planning beads describing the intended end-state.
- **Git history:** Recent commits and branch names show what has been built and where development is heading.
- **Documentation contents:** README, ADRs, TODOs, ROADMAPs, plans, AGENTS.md, CLAUDE.md (actual file contents, not just paths).
- **Codebase structure:** Directory tree and inline TODO/FIXME/HACK markers found in source files.
- **Existing beads:** Open and in-progress beads, to avoid creating duplicates.

The agent reconstructs the intended end-state from all sources, then identifies concrete gaps: features planned but not built, incomplete implementations, missing integration between components, untested code paths, and divergence from plans. Each gap becomes a new bead. There is no cap on how many beads weave can create — if it finds many gaps, it creates all necessary beads.

**Produces work:** Yes (creates beads). If beads are created, returns 0 and pluck will claim them on the next cycle.

**Configuration:**
- `strands.weave.frequency` (default: 3600) - Minimum seconds between runs per workspace.
- `strands.weave.max_doc_files` (default: 15) - Maximum documentation files to include.

### 5. Pulse

**Purpose:** Automated health scans (security, dependencies, documentation quality).

Pulse runs a set of detectors against the workspace:

- **Security:** Scans for exposed secrets, insecure patterns, known vulnerabilities.
- **Dependencies:** Checks for outdated or vulnerable dependencies.
- **Docs:** Validates documentation completeness and freshness.

Each detector that finds an issue creates a bead. Pulse is rate-limited to avoid flooding the workspace with scan-generated beads.

**Produces work:** Yes (creates beads from detector findings).

### 6. Unravel

**Purpose:** Attempt to solve human-tagged beads without human intervention.

Some beads are tagged `[human]` because they appear to require human action (e.g., providing credentials, making a business decision, performing a physical operation). Unravel looks at these blocked beads and asks: *is there an automated alternative?*

For each human bead (by default, immediately — no waiting period), unravel dispatches an LLM agent to analyze the blocker and propose alternative approaches that:

- Work around the blocker without the human decision.
- Are reversible or easily changed later.
- Are labeled `[ALTERNATIVE]` and `pending-human-review`.

The alternatives are created as child beads of the original human bead. The human bead remains open for the human to address, but workers can pick up the alternative beads and make progress.

**Produces work:** Yes (creates alternative beads).

**Configuration:**
- `unravel.min_wait_hours` (default: 0) - Hours before considering alternatives. Set to 0 for immediate experimentation.
- `unravel.max_alternatives` (default: 3) - Maximum alternatives per human bead.

### 7. Knot

**Purpose:** Alert the human that the system is stuck.

Knot is the failure state. If all six prior strands found no work and created no beads, the system is genuinely stuck. Knot creates a `[human]` bead with priority 0 (critical) and the label `needle-stuck`, containing diagnostic information:

- Recent worker events.
- Bead status summary (open, in_progress, blocked, counts).
- Active worker heartbeats.
- Strand configuration.

Before creating the alert, knot runs pre-flight verification to prevent false positives: it double-checks all bead queries, verifies database integrity (WAL corruption can cause empty query results), and checks if all beads are assigned to active workers (which is expected behavior, not a stuck state).

Alerts are rate-limited to one per hour per workspace.

**Produces work:** Yes (creates a human alert bead).

## Engine Mechanics

### Workspace Changes

When explore finds beads in another workspace, it sets `NEEDLE_EXPLORE_NEW_WORKSPACE` and returns 2. The engine updates its workspace variable and resets the strand index to 0, restarting from pluck. This is capped at 5 restarts per engine invocation to prevent infinite loops.

### Strand Enablement

Each strand can be enabled or disabled via config:

```yaml
strands:
  pluck: true       # or false, or auto
  mend: true
  explore: true
  weave: true
  pulse: true
  unravel: true
  knot: true
```

`auto` defaults to enabled for all strands. Setting a strand to `false` skips it entirely.

### Worker Loop Integration

The engine is called once per iteration of the worker loop:

```
while true:
    result = strand_engine(workspace, agent)
    if result == 0:
        reset idle counter
    else:
        increment idle counter
        if idle_timeout exceeded:
            exit worker
    sleep polling_interval
```

## Source Files

| File | Description |
|------|-------------|
| `src/strands/engine.sh` | Main dispatcher, strand enablement, workspace change handling |
| `src/strands/pluck.sh` | Bead claiming and agent dispatch |
| `src/strands/mend.sh` | Orphaned/stale claim cleanup, heartbeat pruning |
| `src/strands/explore.sh` | Filesystem search (children + upward walk) |
| `src/strands/weave.sh` | Gap analysis via LLM agent |
| `src/strands/pulse.sh` | Health scan detectors |
| `src/strands/unravel.sh` | Human bead alternative generation |
| `src/strands/knot.sh` | Human alerting (failure state) |
