# beads-fleet: Visual Dashboard with Pipeline Orchestration

## Research Date: 2026-03-20
## Source: https://github.com/jmcy9999/beads-fleet

## What It Is

beads-fleet is a browser-based dashboard for managing fleets of software development agents backed by the beads issue tracker (`bd`). It provides kanban boards, dependency graph visualization, pipeline orchestration, and one-click autonomous agent launches. It targets the original beads (Dolt backend).

## Architecture

### Thin Wrapper Design

beads-fleet does not duplicate data. It reads from existing beads infrastructure:

```
Browser -> Next.js API Routes -> bv CLI (Robot Protocol)
                                  |
                        SQLite fallback (.beads/beads.db)
                                  |
                        JSONL fallback (.beads/issues.jsonl)
```

API routes shell out to `bv --robot-*` commands (beads_viewer's structured JSON API). When `bv` is unavailable, direct SQLite access via `better-sqlite3` provides fallback analytics. JSONL parsing is the last resort.

Short-TTL in-memory caching keeps the UI responsive without hammering the filesystem.

### Pipeline Orchestration

The "Fleet Board" tracks epics through configurable project stages:
```
research -> development -> review -> submission
```

Epics move through columns as pipeline labels. When a Claude Code agent completes a phase, the system automatically updates issue labels to advance the epic to the next stage.

### Agent Management

**Agent Launcher** (`agent-launcher.ts`):
- Manages Claude Code CLI as background subprocesses
- Handles process lifecycle (start, stop, status polling)
- Captures exit codes and logs for failure detection
- Status transitions trigger pipeline label updates

**Live Monitoring**:
- Continuous subprocess monitoring via `/api/agent` endpoints
- Log tailing for real-time visibility
- Token usage tracking per-issue and aggregate

### Signals Polling

A query endpoint detects issue state changes since timestamps, filterable by label and status. External automation can react to issue transitions without constant polling.

## Key Design Decisions

### 1. Dashboard as Control Plane

beads-fleet is a visual orchestrator. Humans click "launch agent" and monitor progress through the browser. This is the opposite of NEEDLE's headless, autonomous approach.

### 2. Pipeline Labels as State Machine

Instead of tracking agent state in a separate system, beads-fleet uses beads labels as the state machine:
- `pipeline:research` -> `pipeline:development` -> `pipeline:review`
- Label transitions driven by agent completion events
- Queryable through standard beads label filters

This is clever: the orchestration state is stored in the beads themselves, visible to any tool that reads beads.

### 3. Multi-Repo Support

beads-fleet aggregates across multiple projects:
- Watch directories auto-discover new projects
- API-driven repo management (`POST /api/repos`)
- Activity timeline correlates agent sessions across all tracked issues

### 4. Token Cost Awareness

Per-issue and per-project token usage tracking. This enables cost-per-bead analysis and helps teams budget AI usage.

## Relevance to NEEDLE

### Different Niche, Complementary

beads-fleet is a human-facing dashboard. NEEDLE is a headless orchestrator. They solve different problems:
- beads-fleet gives humans visibility and one-click control
- NEEDLE gives machines autonomous execution

### What NEEDLE Could Adopt

1. **Pipeline labels as state machine**: NEEDLE could use bead labels to track processing state (e.g., `needle:claimed`, `needle:in-progress`, `needle:retry-1`). This makes NEEDLE's state visible to any beads viewer or dashboard.

2. **Token usage tracking**: NEEDLE could record per-bead token consumption (if the agent CLI reports it) for cost analysis.

3. **Signals endpoint**: NEEDLE could expose a simple HTTP endpoint for external monitoring (e.g., "which beads are currently being processed?", "which workers are active?").

4. **Multi-repo aggregation**: NEEDLE's Explore strand already searches other workspaces. beads-fleet's approach of watch directories for auto-discovery could inform NEEDLE's workspace discovery.

### What Would Not Apply

1. **Browser UI**: NEEDLE is headless by design. A dashboard is useful for monitoring but should be a separate tool, not embedded in NEEDLE.

2. **One-click launches**: NEEDLE auto-launches continuously. Manual launch defeats the purpose.

3. **bd dependency**: beads-fleet requires the original beads with Dolt. NEEDLE uses beads_rust with SQLite.
