# Beads Ecosystem Overview

## Research Date: 2026-03-20

## What is Beads?

Beads is a local-first, non-invasive issue tracker created by Steve Yegge. It stores tasks in a dependency-aware graph, designed primarily as "a persistent, structured memory for coding agents." The core thesis: AI agents need queryable, persistent task state that survives across sessions, not ephemeral context windows.

## Two Lineages

The ecosystem has two distinct lineages that share concepts but differ in storage:

### 1. Original Beads (steveyegge/beads) - Go + Dolt

- Written in Go (~276,000 LOC)
- Uses Dolt (version-controlled SQL database) for storage
- Runs an optional background daemon for multi-writer access
- Has cell-level merge resolution via Dolt's native versioning
- Supports Molecules (workflow orchestration primitives), Messaging, and Wisps (ephemeral operations)
- CLI: `bd`

### 2. Beads Rust (Dicklesworthstone/beads_rust) - Rust + SQLite + JSONL

- Written in Rust (~20,000 LOC)
- Uses SQLite + JSONL hybrid storage
- No background daemon; pure CLI
- JSONL provides git-friendly collaboration
- Deliberately frozen at the SQLite+JSONL architecture
- CLI: `br`
- Suffers from FrankenSQLite corruption (issue #171) and concurrent write conflicts (issue #191)

Most ecosystem tools target one lineage or the other. NEEDLE targets beads_rust (`br`).

## Ecosystem Map by Category

### Orchestrators / Autonomous Workers (NEEDLE's Direct Competitors)

| Project | Target | Approach |
|---------|--------|----------|
| **NEEDLE** | br | State machine loop: select-claim-build-dispatch-execute-outcome. Multi-worker via SQLite atomic claims. Agent-agnostic (Claude, OpenCode, Codex, Aider). |
| **Perles** (zjrosen/perles) | bd | BQL query language + multi-agent control plane with Coordinator/Worker model. Workers cycle through impl/review/await/feedback/commit phases. |
| **ralph-beads** (danboyle7/ralph-beads) | bd | Autonomous Claude loop. Fetches `bd ready`, constructs prompt, invokes Claude, monitors for completion tag. Sequential, single-worker. |
| **ralph** (sasha-incorporated/ralph) | br | Shell script loop: `br ready` -> select model from label -> delegate to Claude/Cursor -> validate closure -> retry -> iterate. Sequential, single-worker. |
| **beads-orchestration-claude** (ninjapanzer) | bd | Multi-model orchestration: Opus orchestrator, Sonnet implementer, Haiku reviewer. Workers in git worktrees. Max 3 parallel issues. |
| **Initializer** (carlosbrown2/initializer) | bd | Compound Engineering template with Ralph loop. Sequential one-bead-per-session. Quartet phases: implement -> review -> simplify -> learn. |
| **beads-workflow** (thoreinstein) | bd | Gemini CLI extension. Six-phase lifecycle with specialist agents (Principal Engineer, Security, QA, SRE). Obsidian for architectural memory. |
| **beads-fleet** (jmcy9999) | bd | Browser dashboard with pipeline orchestration. One-click Claude Code agent launches. Pipeline label management for stage transitions. |

### Validation / Quality Gates

| Project | Target | Approach |
|---------|--------|----------|
| **bg-gate** (antonioc-cl) | br | Wraps `br close` with validation gates. Command gates (run tests) and grep-absent gates (check for forbidden patterns). Severity levels block or warn. |

### Concurrency / Coordination Forks

| Project | Target | Approach |
|---------|--------|----------|
| **beads-polis** (Perttulands) | br fork | Event-sourced rewrite. JSONL is source of truth, SQLite is derived index. POSIX flock for write serialization. Claim/heartbeat/unclaim lifecycle with lock expiry. ~3,300 LOC. |
| **bead-forge** (jedarden) | br replacement | Research-phase drop-in replacement with built-in coordination server. Aims to eliminate SQLite thundering herd at 11+ workers. |

### Agent Skills / Integrations

| Project | Target | Approach |
|---------|--------|----------|
| **beads-rust-skill** (ar1g) | br | Claude Code skill teaching agents to create/triage/update/close/link issues via `br` CLI. |
| **beads-skill** (sttts) | bd | Claude Code skill with worktree management for epics. Auto-primes with `bd prime`. |
| **opencode-beads** (joshuadavidthomas) | bd | OpenCode plugin. Auto-injects `bd prime` at session start. Includes beads-task-agent subagent. |
| **spec2beads** (dcarmitage) | br | Claude skill that decomposes product specs into INVEST-compliant beads with dependency DAGs. |

### Multi-Agent Workflow Templates

| Project | Target | Approach |
|---------|--------|----------|
| **obc_agent_workflow** (jkbhagatio) | br | OpenSpec-Beads-Coordination. Master-Delta PRD pattern. File claiming via `.coordination/` YAML. Symlinked `.beads/` across worktrees. |

### TUIs / Viewers

| Project | Target |
|---------|--------|
| **beads_viewer_rust** (Dicklesworthstone) | br |
| **Beads-Kanban-UI** (AvivK5498) | br |
| **beads-tui** (bobisme, davidcforbes) | bd |
| **perles** (zjrosen) | bd |
| **abacus** (ChrisEdwards) | bd |

### Editor Extensions

| Project | Editor |
|---------|--------|
| **Beads-Kanban** (davidcforbes) | VSCode |
| **beads.el** (deangiberson, ChristianTietze, chrisbarrett) | Emacs |
| **nvim-beads** (cwolfe007) | Neovim |

## Key Ecosystem Tensions

1. **SQLite concurrency**: beads_rust's SQLite backend creates contention at scale. beads-polis solves with POSIX flock; bead-forge proposes a coordination server; NEEDLE uses atomic claims with retry loops.

2. **bd vs br divergence**: The original `bd` is evolving toward GasTown (a new backend). `br` is frozen at SQLite+JSONL. Ecosystem tools must pick a side.

3. **Orchestration is unsolved upstream**: Neither `bd` nor `br` provide built-in orchestration. Every team builds their own loop (Ralph, NEEDLE, Perles, etc.).

4. **Claim semantics vary**: `bd` has `--claim` (atomic assignee + in_progress). `br` has `--status in_progress --assignee`. beads-polis adds heartbeats and lock expiry. No standard.

## Relevance to NEEDLE

NEEDLE occupies a unique position in the ecosystem:
- **Agent-agnostic**: Most competitors are Claude-specific. NEEDLE supports any headless CLI.
- **Multi-worker native**: Most competitors are single-worker sequential loops. NEEDLE's atomic claim + deterministic priority model is unusual.
- **Explicit state machine**: No other orchestrator documents every outcome path. Ralph retries; NEEDLE has distinct handlers for success/failure/timeout/crash.
- **br-native**: While the richest orchestration (Perles, beads-workflow) targets `bd`, NEEDLE targets `br`.

The primary gap: NEEDLE must work around `br`'s concurrency limitations (issues #171, #191) that other orchestrators avoid by either using `bd` or forking `br`.
