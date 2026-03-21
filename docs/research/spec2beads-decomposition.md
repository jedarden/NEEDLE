# spec2beads: Specification Decomposition into Beads

## Research Date: 2026-03-20
## Source: https://github.com/dcarmitage/spec2beads

## What It Is

A Claude Code skill that decomposes product specifications into structured, dependency-aware beads for the beads_rust (`br`) issue tracker. It bridges the gap between "what we want to build" and "what the autonomous worker should do next." This is the upstream bead creation pipeline that feeds orchestrators like NEEDLE.

## How It Works

### Input

Natural language product descriptions:
- "We need user authentication with email/password and OAuth"
- Feature requests, product goals, vague requirements

### Processing

Claude performs structured decomposition:
1. Identify goals, scope boundaries, and unknown variables
2. Structure work into typed beads: epics, features, tasks, spikes, bugs, chores
3. Assign priority levels (P0-P4) based on criticality
4. Add acceptance criteria for clarity
5. Apply organizational labels
6. Establish task dependencies (DAG)

### Output

Executable `br create` commands ready to run:
```bash
br create "Set up OAuth2 provider integration" --type feature --priority 1 \
  --description "Implement OAuth2 flow for Google and GitHub providers" \
  --label "auth" --label "backend"
br dep add bd-oauth bd-auth-epic
```

A Python script converts JSON plans to executable bash scripts for batch creation.

## Key Design Decisions

### 1. INVEST-Compliant Work Items

Beads are decomposed following the INVEST criteria:
- **I**ndependent: Minimal dependencies between beads
- **N**egotiable: Clear but not over-specified
- **V**aluable: Each bead delivers user-visible value
- **E**stimable: Small enough to estimate
- **S**mall: Completable in one agent session
- **T**estable: Clear acceptance criteria

### 2. Human vs. Agent Routing

Tasks requiring human judgment receive:
- `needs-decision` label
- Assignment to `human`

All other tasks flow to agents via `br ready`. This separates automated work from decisions that require human input.

### 3. Dependency DAG Over Flat List

Beads are not a flat backlog. spec2beads creates a directed acyclic graph:
- Foundation beads (database schema, project setup) have no dependencies
- Feature beads depend on their prerequisites
- Integration beads depend on the features they integrate

This ensures `br ready` surfaces work in the correct order -- agents cannot start on OAuth integration before the auth module exists.

### 4. Multi-Agent Coordination Guidelines

spec2beads includes guidance for distributing work across multiple agents:
- Function-specialized agents (not person-specialized)
- Work partitioned by domain, not by agent
- Shared beads database as coordination point

## Relevance to NEEDLE

### Filling NEEDLE's Input Gap

NEEDLE processes beads but does not create them. spec2beads creates beads from specifications. Together they form a pipeline:

```
Specification -> [spec2beads] -> Bead Queue -> [NEEDLE] -> Completed Work
```

### Bead Quality Affects NEEDLE Performance

spec2beads' decomposition quality directly impacts NEEDLE's effectiveness:
- **Well-scoped beads** (one session of work) -> higher NEEDLE success rate
- **Clear acceptance criteria** -> better agent prompts -> fewer failures
- **Correct dependencies** -> NEEDLE processes beads in the right order
- **Appropriate priority** -> NEEDLE's deterministic ordering makes sense

### What NEEDLE Should Expect from Beads

Based on spec2beads' output format, NEEDLE should handle:
- Typed beads (epic, feature, task, spike, bug, chore) with different handling per type
- Priority 0-4 with P0 as highest
- Labels for filtering and routing
- Acceptance criteria as validation checkpoints
- Dependency graphs that constrain processing order
- Human-labeled beads that should be skipped

### Potential Enhancement

NEEDLE's Weave strand (create beads from documentation gaps) is conceptually similar to spec2beads. If NEEDLE's Weave strand used spec2beads-style decomposition, it could generate higher-quality beads when auto-discovering work.
