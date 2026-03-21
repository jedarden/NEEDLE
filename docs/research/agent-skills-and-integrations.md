# Agent Skills and Editor Integrations in the Beads Ecosystem

## Research Date: 2026-03-20

## Overview

Multiple projects provide agent-level integration with beads, teaching AI coding assistants how to interact with the bead lifecycle. These are not orchestrators -- they operate within a single agent session, providing the agent with beads awareness and commands.

## Claude Code Skills

### beads-rust-skill (ar1g)
**Source**: https://github.com/ar1g/beads-rust-skill
**Target**: beads_rust (`br`)

Teaches Claude Code agents to manage issues via `br` CLI:
- Create, triage, update, close, and link issues
- Backlog grooming and priority management
- Dependency mapping
- Closure documentation with audit trails
- All operations use `--json` for reliable parsing

**Architecture**: Separation of concerns -- `br` handles data, the skill handles orchestration logic, Claude provides reasoning. The skill is a playbook, not a data layer.

### beads-skill (sttts)
**Source**: https://github.com/sttts/beads-skill
**Target**: Original beads (`bd`)

Claude Code skill with worktree management:
- Auto-primes with `bd prime` at session start
- Worktree management for epics
- Session finalization checklist ("landing the plane")
- PR/MR URL labeling on task records
- Conditional activation (only in projects with `.beads/` directory)

### spec2beads (dcarmitage)
**Source**: https://github.com/dcarmitage/spec2beads
**Target**: beads_rust (`br`)

Not a lifecycle skill but a creation skill:
- Decomposes product specifications into INVEST-compliant beads
- Generates dependency DAGs
- Outputs executable `br create` commands
- Routes human-judgment tasks to `needs-decision` label

(Covered in detail in `spec2beads-decomposition.md`)

## OpenCode Plugin

### opencode-beads (joshuadavidthomas)
**Source**: https://github.com/joshuadavidthomas/opencode-beads
**Target**: Original beads (`bd`)

Lightweight plugin for the OpenCode AI coding platform:
- Auto-injects `bd prime` at session start and after compaction
- All beads operations as `/bd-*` slash commands
- **beads-task-agent**: Subagent for autonomous issue completion -- handles independent work, status updates, and dependency resolution
- Intentionally minimal scope; defers workflow logic to beads itself
- Fork-friendly design (copy and customize)

## Gemini CLI Extension

### beads-workflow (thoreinstein)
**Source**: https://github.com/thoreinstein/beads-workflow
**Target**: Original beads (`bd`)

Full workflow extension with specialist agents. (Covered in detail in `beads-workflow-gemini.md`)

## Editor Extensions (Non-Agent)

### Emacs
- **beads.el** (deangiberson, ChristianTietze, chrisbarrett): Three independent Emacs frontends. ChristianTietze's version uses Unix socket RPC to a beads daemon.

### Neovim
- **nvim-beads** (cwolfe007): Neovim plugin with issues panel.

### VSCode
- **Beads-Kanban** (davidcforbes): VSCode extension with KanBan board, table view, and record-level editing.
- **vscode-beads**: Issues panel extension.

### JetBrains
- **beads-manager**: Kotlin plugin for JetBrains IDEs.

## Community Tools Referenced by steveyegge/beads

The official `docs/COMMUNITY_TOOLS.md` catalogs the ecosystem:

### Orchestration-Adjacent
- **Foolery**: Visual orchestration UI for agent work management
- **beads-compound**: Plugin marketplace with persistent memory hooks
- **claude-handoff**: Multi-session continuity with structured handoff files
- **claude-protocol**: Node.js orchestration optimized for Claude models
- **BeadHub**: Server enabling work claiming, file reservation, and agent messaging

### Data Tools
- **stringer**: Mines git repos for TODOs and code metrics, creates beads
- **beads-sdk**: Typed TypeScript client library

## Relevance to NEEDLE

### Pattern: Context Injection via Prime

Nearly every integration uses `bd prime` or equivalent to inject beads context into the agent session. This is the standard pattern for making agents beads-aware.

NEEDLE's Build step (constructing the prompt from bead context) serves the same purpose but does so externally rather than relying on the agent to prime itself.

### Pattern: Agent Specialization

Skills define what an agent *can do* with beads. NEEDLE defines what an agent *should do* with beads. The distinction:
- Skills: "You can create/update/close beads" (capability)
- NEEDLE: "Claim this bead, complete this task, report the outcome" (direction)

NEEDLE could use these skills as building blocks -- instead of building prompts from scratch, NEEDLE could install a beads skill and reference it in the dispatch prompt.

### BeadHub: Server-Based Claiming

The community tool **BeadHub** provides server-based work claiming and file reservation. This is similar to bead-forge's proposed coordination server. Worth investigating as a potential solution to NEEDLE's SQLite concurrency challenges.

### Gap: No br-Native Skills for NEEDLE Workers

The beads-rust-skill (ar1g) teaches agents to use `br` CLI, but it is designed for interactive sessions, not headless dispatch. NEEDLE workers need a stripped-down skill that focuses on:
1. Read the assigned bead
2. Execute the work
3. Report completion status
4. Close the bead with a reason

This is simpler than a full interactive skill.
