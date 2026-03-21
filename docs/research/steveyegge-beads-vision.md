# Steve Yegge's Beads: Vision and Processing Model

## Research Date: 2026-03-20
## Source: https://github.com/steveyegge/beads

## The Vision

Steve Yegge designed Beads as "a persistent, structured memory for coding agents." The core insight: AI coding agents lose all context between sessions. Beads gives them a queryable, dependency-aware task graph that persists across sessions, enabling long-horizon autonomous work.

The tagline is "a memory upgrade for your coding agent."

## Architecture: Dolt-Powered

Unlike beads_rust's SQLite+JSONL model, the original beads uses Dolt -- a version-controlled SQL database with:

- **Cell-level merge resolution**: Two agents modifying different fields of the same row merge cleanly
- **Native branching**: Agents can work on branches and merge
- **Built-in sync via Dolt remotes**: Push/pull to DoltHub, S3, or filesystem
- **Automatic audit trail**: Every write is a Dolt commit

This gives the original beads fundamentally better multi-agent story than beads_rust. Concurrent access is handled by Dolt's SQL server mode, not by file-level locks or SQLite transactions.

## Intended Agent Workflow

### The Ready Queue

The primary interface for agents: `bd ready --json` returns beads with no open blockers, sorted for immediate work. This is the "what should I work on next?" query that drives all orchestration.

### Atomic Claiming

`bd update bd-abc --claim` atomically sets:
- `status` -> `in_progress`
- `assignee` -> current agent identity

This is a single-operation claim primitive that beads_rust lacks.

### Dependency Graph

Beads supports 18 relationship types:
- **blocks**: Sequential execution (B waits for A)
- **conditional-blocks**: Error handling paths
- **waits-for**: Gate pattern (B waits for all of A's children)
- **relates_to**, **duplicates**, **supersedes**, **replies_to**: Knowledge graph semantics

The `bd ready` command respects all blocking relationships, surfacing only truly actionable work.

### Hierarchical Decomposition

Beads support dotted ID hierarchies: bd-a3f8, bd-a3f8.1, bd-a3f8.1.1. Agents can decompose epics into subtasks, creating parent-child relationships that track progress through the dependency graph.

## Molecules: Built-in Orchestration

The most advanced feature in original beads. A Molecule is "an epic with execution intent" -- a workflow template that agents traverse:

1. Agent identifies ready work within the molecule (children with no open blockers)
2. Ready children execute in parallel (parallelism is the default)
3. Explicit dependencies create sequencing
4. Agent blocks on dependencies until prerequisites close
5. Continues until all work completes

### Molecule Lifecycle

- **Mol**: Persistent workflow (synced, audited)
- **Wisp**: Ephemeral operations (routine work, no audit trail, auto-deleted)
- **Proto**: Frozen templates for reuse

Operations: `pour` (template -> molecule), `wisp` (template -> ephemeral), `squash` (compress), `burn` (discard).

Molecules are the upstream answer to the orchestration problem NEEDLE solves. They embed workflow semantics directly in the issue tracker rather than requiring an external wrapper.

## Messaging: Agent-to-Agent Communication

Beads includes built-in messaging:
- Messages are special issue types with threading via `replies_to` dependencies
- Agents send to named worker queues
- Routing delegated to an external mail provider (typically GasTown's `gt mail`)
- Hook scripts in `.beads/hooks/` trigger on create/update/close events

This enables orchestrator integration without embedding external dependencies.

## Multi-Agent Design Principles

### Zero Conflict IDs

Hash-based IDs (bd-a1b2) generated from random UUIDs prevent merge collisions. Two agents creating beads simultaneously on different branches will never collide. This is fundamental to the "no coordination overhead" philosophy.

### Shared Database via Worktrees

All git worktrees share one `.beads` database. Multiple agents work in different worktrees, seeing the same beads. Dolt server mode (`bd dolt start`) enables concurrent multi-writer access.

### Stealth Mode

`bd init --stealth` keeps beads out of the repository entirely, operating from a separate directory. This enables agents to track work on projects where modifying `.beads/` is not permitted.

## What Yegge Did NOT Build

Notably absent from the original beads:
- **No orchestrator**: No built-in loop for selecting, claiming, executing, and closing beads automatically
- **No worker fleet management**: No concept of named workers, sessions, or scheduling
- **No agent dispatch**: No mechanism to invoke a CLI agent with a bead's context
- **No outcome handling**: No built-in success/failure/timeout/crash classification
- **No strand escalation**: No policy for what to do when work runs out

These gaps are exactly what NEEDLE fills.

## Relevance to NEEDLE

### What NEEDLE Can Learn from Original Beads

1. **Molecules as orchestration primitives**: Instead of NEEDLE managing workflow externally, molecules embed intent in the bead graph itself. NEEDLE could interpret molecule structure rather than reimplementing workflow logic.

2. **Messaging for coordination**: Instead of relying solely on the bead queue for worker coordination, NEEDLE could use beads' messaging system for worker-to-worker communication.

3. **Dolt's concurrency model**: NEEDLE fights SQLite's single-writer limitation. If beads_rust ever adopts Dolt (unlikely given the deliberate freeze), the concurrency problem evaporates.

4. **Atomic claiming**: `bd --claim` is what NEEDLE needs. With `br`, NEEDLE must compose the claim from separate status + assignee updates within a single command, relying on SQLite transaction isolation.

### Why NEEDLE Exists Despite Molecules

Molecules provide workflow structure but not execution. They describe *what* should happen in *what order*, but they do not:
- Invoke an AI agent with a prompt
- Monitor exit codes
- Handle timeouts and crashes
- Manage retry logic
- Launch and coordinate worker processes

NEEDLE is the execution layer that molecules need. The two are complementary, not competing.
