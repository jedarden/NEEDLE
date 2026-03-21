# OBC: OpenSpec-Beads-Coordination Multi-Agent Workflow

## Research Date: 2026-03-20
## Source: https://github.com/jkbhagatio/obc_agent_workflow

## What It Is

OBC (OpenSpec-Beads-Coordination) is a multi-agent workflow template that coordinates parallel agent work on a single project. It combines three tools:
- **OpenSpec**: Research -> proposal -> specs artifact generation
- **beads_rust (br)**: Issue tracking and task breakdown
- **Coordination YAMLs**: Conflict detection and file claiming

Unlike orchestrators that manage agent execution (NEEDLE, Ralph, Perles), OBC is a *coordination protocol* -- it defines how agents share a workspace without stepping on each other, but does not dispatch or monitor agents.

## Architecture

### Master-Delta PRD Pattern

A single source of truth: `openspec/specs/prd.md` (the master PRD). Agents work on single-domain changes via "delta PRDs" that contain:
- `## ADDED Requirements`
- `## MODIFIED Requirements`
- `## REMOVED Requirements`

Delta PRDs merge into the master upon approval. This prevents conflicts when multiple agents modify the same spec.

### File Claiming via .coordination/

Each agent has a coordination YAML file (e.g., `agent-claudia.yaml`, `agent-claudius.yaml`) that tracks:
- Agent status
- Claimed files
- Metadata for conflict prevention

Before starting work, agents check `.coordination/` files to ensure no other agent has claimed their target files. This is a cooperative, file-based locking mechanism with no enforcement -- agents must obey the protocol.

### Shared Beads via Symlinks

All agents' worktrees symlink to the same `.beads/` directory and `.coordination/` directory:
```
agent-worktree-1/.beads/ -> main-repo/.beads/
agent-worktree-1/.coordination/ -> main-repo/.coordination/
agent-worktree-2/.beads/ -> main-repo/.beads/
agent-worktree-2/.coordination/ -> main-repo/.coordination/
```

This ensures all agents see the same bead queue and coordination state.

## Workflow

1. Agent reads master PRD domain specifications
2. Runs OpenSpec workflow: research -> proposal -> specs (delta PRD)
3. Uses `br` to create beads from the final specs
4. Implements code changes addressing all beads
5. Submits for review; delta PRD merges into master on approval
6. Clears conversation; awaits next change assignment

## Key Design Decisions

### 1. Coordination Over Orchestration

OBC does not launch, monitor, or manage agents. It provides:
- A shared database (beads)
- A claiming protocol (coordination YAMLs)
- A specification pipeline (OpenSpec)

The actual agent execution is external. This makes OBC compatible with any orchestrator (including NEEDLE).

### 2. File-Level Claiming

Where NEEDLE claims beads, OBC claims files. An agent working on the auth module claims `src/auth/*.rs` in its coordination YAML. Another agent working on the API claims `src/api/*.rs`. This prevents merge conflicts at the file level, not just the task level.

### 3. Spec-Driven Bead Creation

Beads are not created manually. They are generated from OpenSpec's specification pipeline:
- Research produces understanding
- Proposals suggest approaches
- Specs define requirements
- `br create` commands are generated from specs

This closes the loop between planning and execution.

### 4. Domain Isolation

Each delta PRD targets exactly one domain. Agents work on orthogonal domains. This is the primary conflict avoidance mechanism -- if agents never modify the same domain, they never conflict.

## Relevance to NEEDLE

### What OBC Solves That NEEDLE Does Not

1. **File-level conflict prevention**: NEEDLE assigns beads to workers but does not track which files each worker modifies. Two workers could claim non-conflicting beads but modify the same file.

2. **Spec-to-bead pipeline**: NEEDLE processes existing beads but does not create them. OBC's OpenSpec pipeline could feed beads into NEEDLE's queue.

3. **Domain isolation**: OBC's domain-based work partitioning prevents conflicts that bead-level claiming cannot catch.

### What NEEDLE Provides That OBC Lacks

1. **Execution management**: OBC does not dispatch agents, monitor outcomes, handle timeouts, or manage retries. NEEDLE does all of this.

2. **Autonomous operation**: OBC requires agents to manually check coordination files. NEEDLE automates the full claim-execute-close loop.

3. **Multi-worker scheduling**: OBC assumes agents coordinate cooperatively. NEEDLE provides deterministic scheduling with atomic claims.

### Potential Integration

OBC and NEEDLE are complementary:
- OBC manages *what* to work on and *where* agents can write
- NEEDLE manages *who* works on *which bead* and *how* the work executes

A combined system could use OBC's coordination YAMLs to constrain NEEDLE's worker assignments -- e.g., worker-alpha only gets beads in the auth domain, worker-bravo only gets API domain beads.
