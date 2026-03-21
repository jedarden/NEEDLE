# beads-orchestration-claude: Multi-Model Orchestration with Worktrees

## Research Date: 2026-03-20
## Source: https://github.com/ninjapanzer/beads-orchestration-claude

## What It Is

A Claude Code integration that orchestrates multi-agent development workflows using beads (`bd`) for task tracking. Unlike Ralph (single-worker sequential), this project implements a multi-model, multi-worker approach with specialized roles and git worktree isolation.

## Architecture

### Three-Role Model

| Role | Model | Function |
|------|-------|----------|
| **Orchestrator** | Opus | Workflow management, worker dispatch, merge handling |
| **Implementer** | Sonnet | Code development in isolated git worktrees |
| **Reviewer** | Haiku | Code review, testing, approval decisions |

The Orchestrator is a persistent agent that spawns Implementer and Reviewer workers as separate processes. Workers run with `claude --dangerously-skip-permissions` to prevent memory exhaustion in the Orchestrator's context.

### Worktree Isolation

Each Implementer works in its own git worktree, providing:
- Code isolation (no merge conflicts during implementation)
- Independent build/test environments
- Clean merge paths back to main

### Batch Processing

The `/beads-orchestrate` command supports:
- `--max=N`: Parallel issues (default 2, max 3)
- `--once`: Single batch then stop
- `--dry-run`: Preview without execution
- `--batches=N`: Limit total iterations
- `--spec=<path>`: Create beads from a specification file before orchestrating

## Workflow

1. Orchestrator queries `bd ready` for available work
2. Orchestrator selects up to N beads for parallel processing
3. For each bead, Orchestrator spawns an Implementer in a fresh worktree
4. Implementer works on the bead, commits changes
5. Orchestrator spawns a Reviewer to evaluate the work
6. On approval, Orchestrator handles the merge
7. Loop continues until queue is empty or batch limit reached

## Key Design Decisions

### 1. Model Specialization

Different models for different roles:
- **Opus for orchestration**: Best reasoning for complex scheduling and merge decisions
- **Sonnet for implementation**: Best cost/quality ratio for coding tasks
- **Haiku for review**: Cheapest model for pass/fail evaluation

This is cost-optimized. An Opus orchestrator managing Sonnet workers is significantly cheaper than running Opus for everything.

### 2. Conservative Parallelism

Max 3 parallel issues by default. This is not a beads limitation -- it is a resource constraint:
- Each worker consumes API tokens
- Git worktrees consume disk space
- Merge conflicts increase with parallelism
- The Orchestrator's context grows with more workers to manage

### 3. Orchestrator as Worker Dispatcher

The Orchestrator does not do implementation work. It is a pure coordinator:
- Reads the queue
- Assigns work
- Monitors progress
- Handles merges
- Manages retries

This separation is similar to Perles' Coordinator/Worker split but uses different Claude models rather than different agent types.

## Relevance to NEEDLE

### Similarities to NEEDLE

- Multi-worker architecture (though max 3 vs. NEEDLE's 10-20)
- Work selection from the ready queue
- Agent dispatch as separate processes
- Loop-until-done pattern

### Differences from NEEDLE

| Aspect | beads-orchestration-claude | NEEDLE |
|--------|---------------------------|--------|
| Coordinator | Dedicated Opus agent | No coordinator; workers are independent |
| Worker limit | Max 3 | 10-20+ |
| Claim mechanism | Orchestrator assigns (no claim race) | Workers compete via `br update --claim` |
| Model selection | Role-based (Opus/Sonnet/Haiku) | Agent-adapter YAML configuration |
| Review step | Built-in Haiku reviewer | No built-in review; validation is per-adapter |
| Merge handling | Orchestrator merges worktrees | Not in scope (NEEDLE processes beads, not git merges) |
| Backend | Original beads (bd/Dolt) | beads_rust (br/SQLite) |

### What NEEDLE Could Adopt

1. **Review step**: After an agent completes a bead, dispatch a second (cheaper) agent to review the work before closing the bead. This catches quality issues without expensive Opus for everything.

2. **Model specialization**: Different beads could be routed to different models based on complexity. P0 critical bugs -> Opus; P3 chores -> Haiku.

3. **Worktree isolation**: For beads that modify code, having each worker in a separate worktree prevents conflicts. NEEDLE currently assumes workers operate in the same workspace.

### What Would Not Work for NEEDLE

1. **Central Orchestrator**: The Orchestrator is a single point of failure. If it crashes, all work stops. NEEDLE's independent workers continue even if some die.

2. **Max 3 workers**: Too low for NEEDLE's use case. The beads-orchestration-claude limit exists because the Orchestrator must track all workers in its context window. NEEDLE's workers have no shared context to manage.

3. **Claude-only**: Tied to Claude models for all three roles. NEEDLE's agent-agnostic design is more flexible.
