# beads-workflow: Gemini CLI Extension with Specialist Agents

## Research Date: 2026-03-20
## Source: https://github.com/thoreinstein/beads-workflow

## What It Is

A Gemini CLI extension that enforces a "Planning First" philosophy by integrating beads (`bd`) for task tracking, Obsidian for architectural memory, and Git for version control. Unlike NEEDLE's autonomous worker loop, this is a human-directed workflow with AI specialist agents providing expertise at gate points.

## Architecture

### Four Central Mandates

1. **Beads as Ground Truth**: Never work without an active ticket. Synchronize with `bd ready` or `bd prime`.
2. **Obsidian as Institutional Memory**: Implementation plans and architectural records must live in Obsidian.
3. **Atomic Execution**: `/implement` enforces phased execution with commits after each phase.
4. **Knowledge Compounding**: `/compound` captures lessons into permanent Obsidian artifacts.

### Six-Phase Lifecycle

```
Refine -> Plan -> Execute -> Audit -> Compound -> Release
```

1. **Refine** (`/refine <id>`): Establish scope in beads
2. **Plan** (`/analyze <id>`): Generate Obsidian architectural records
3. **Execute** (`/implement <id>`): Build features with atomic commits
4. **Audit** (`/rams`, `/review`): Validate quality
5. **Compound** (`/compound <id>`): Transform lessons into knowledge
6. **Release** (`/release`): Prepare production artifacts

### Six Specialist Agents

| Agent | Role |
|-------|------|
| Principal Engineer | Architectural decisions, deep debugging |
| Software Architect | Validates `/analyze` outputs |
| Security Engineer | OWASP-aligned reviews |
| SDET/QA Engineer | Testing and validation |
| Agile Delivery Lead | Owns `/refine` process |
| SRE Engineer | Reliability and infrastructure |

These are not separate processes. They are prompt personas invoked at specific gate points within the Gemini CLI session.

### Enforcement Hooks

Three automated hooks ensure discipline:
- **obsidian-guardrail** (BeforeTool): Blocks local markdown writes except GEMINI.md
- **session-context** (SessionStart): Reminds user to sync with beads
- **compound-reminder** (SessionEnd): Flags completed tickets missing `/compound`

## Key Design Decisions

### 1. Planning Over Execution

beads-workflow is the opposite of NEEDLE philosophically. NEEDLE automates execution; beads-workflow automates planning discipline. Every code change must be backed by:
- A beads ticket (ground truth)
- An Obsidian plan (architectural record)
- A compound artifact (lessons learned)

### 2. Specialist Agents as Gates

Instead of one agent doing everything, specialist personas provide targeted expertise:
- Security review happens at a specific point, by a specific persona
- Architecture validation happens before implementation, not after
- QA review happens before release, not as an afterthought

### 3. Obsidian as External Memory

Plans and learnings persist in Obsidian, not in the beads database. This separates "what to do" (beads) from "how to think about it" (Obsidian). The obsidian-guardrail hook enforces this separation -- agents cannot write markdown to the project directory.

### 4. Closed-Loop Learning

The `/compound` step is mandatory. After completing a ticket, the agent must extract lessons into Obsidian. This creates a knowledge base that improves future work -- similar to Initializer's Compound Engineering pattern but formalized as a workflow requirement.

## Relevance to NEEDLE

### Fundamentally Different Approach

beads-workflow is human-directed with AI assistance. NEEDLE is AI-directed with human oversight. They solve different problems:
- beads-workflow ensures humans plan well before agents execute
- NEEDLE ensures agents execute autonomously and correctly

### What NEEDLE Could Adopt

1. **Knowledge compounding**: After closing a bead, NEEDLE could trigger a "compound" step that extracts learnings into persistent files. Over time, this builds a knowledge base that improves prompt construction.

2. **Specialist validation**: Instead of a single agent doing implementation + validation, NEEDLE could dispatch a second agent (with a "reviewer" persona) to validate work before closing.

3. **Architectural records**: Beads track what to do; but NEEDLE has no mechanism for recording why decisions were made. Obsidian-style records could help debug failures and improve future work.

### What Would Not Apply

1. **Human-in-the-loop**: NEEDLE is autonomous. beads-workflow's mandate of human approval at each gate would break NEEDLE's continuous processing loop.

2. **Gemini-specific**: The extension is built for Gemini CLI. NEEDLE is agent-agnostic.

3. **Single-session**: beads-workflow operates within one Gemini session. NEEDLE spawns many independent sessions.
