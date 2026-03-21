# The Ralph Loop Pattern: Sequential Bead Processing

## Research Date: 2026-03-20
## Sources:
## - https://github.com/danboyle7/ralph-beads (original Ralph, targets bd)
## - https://github.com/sasha-incorporated/ralph (simplified Ralph, targets br)
## - https://github.com/carlosbrown2/initializer (Compound Engineering template using Ralph)

## What It Is

The "Ralph loop" is the most common pattern in the beads ecosystem for autonomous bead processing. Named after its originator, it is a simple sequential loop: fetch the next ready bead, invoke an AI agent, check if the bead was completed, repeat. Multiple independent implementations exist.

## The Core Algorithm

```
while beads remain:
    bead = bd ready | head -1     # or br ready
    prompt = build_prompt(bead)
    agent_output = invoke_agent(prompt)
    if bead_is_closed(bead):
        continue  # success
    else:
        retry_count += 1
        if retry_count > MAX_RETRIES:
            skip(bead)
```

This is fundamentally simpler than NEEDLE's state machine. There is no claim step, no explicit outcome classification, no strand escalation.

## Implementation Variants

### ralph-beads (danboyle7) - The Original

- **Target**: Original beads (`bd` CLI, Dolt backend)
- **Agent**: Claude Code exclusively (via `--dangerously-skip-permissions --print`)
- **Completion detection**: Monitors for `<promise>COMPLETE</promise>` tags in agent output
- **Retry logic**: Up to 5 attempts per bead if not closed
- **State management**: `.ralph/` directory with progress.txt (append-only log), state.json, config.toml
- **Budget**: Default 10 iterations per run (configurable)
- **Post-processing**: Periodic reflection passes (quality-check, code-review-check, validation-check)
- **Prompt construction**: Multi-part prompts from `.ralph/prompts/` templates (ralph.md, issue.md, cleanup.md, repair.md)
- **Archive**: Timestamped run archives with full logs and snapshots

### ralph (sasha-incorporated) - Simplified for br

- **Target**: beads_rust (`br` CLI)
- **Agent**: Model-routed -- Claude models go to Claude CLI, others go to Cursor agent
- **Model selection**: Per-bead via `model:<name>` label (defaults to haiku)
- **Queue**: Optional `--queue` flag for deterministic ordering
- **Retry**: Configurable retry count
- **Workflow**: `br ready` -> select model -> delegate -> validate closure -> retry -> iterate

### Initializer (carlosbrown2) - Compound Engineering Template

- **Target**: Original beads (`bd` CLI)
- **Pattern**: One-bead-per-session (fresh context each time to prevent context rot)
- **Quartet phases**: Each bead goes through implementation -> review -> simplification -> learning
- **Compound memory**: Patterns extracted into `patterns.md` and `docs/skills/`, rules into `CLAUDE.md` (max 200 lines enforced by hooks)
- **Quality gates**: Pre-commit hooks enforce scope, dependencies, and size limits
- **Learning loop**: Bugs generate test cases; patterns become future context

## Key Design Decisions Across Ralph Variants

### 1. Sequential Over Parallel

All Ralph implementations are single-worker, single-bead-at-a-time. This is deliberate:
- Avoids concurrency bugs in beads backends
- Simplifies reasoning about state
- Each agent session gets full system resources
- No claim races, no lock contention

### 2. No Explicit Claiming

Ralph implementations generally do not claim beads before processing. They rely on:
- Single-worker assumption (no races)
- Completion detection (check if bead was closed, not if it was claimed)
- The ready queue as the coordination point

### 3. Agent-as-Black-Box

Ralph delegates work to the agent and checks the result. It does not:
- Monitor agent progress during execution
- Inject additional context mid-execution
- Manage agent resource usage
- Handle timeouts (waits for agent to finish)

### 4. Fresh Context Per Session (Initializer)

The Compound Engineering variant resets the agent context between beads. Memory persists through files (git, progress.txt, CLAUDE.md), not through conversation history. This prevents context rot in long sessions.

## Relevance to NEEDLE

### What Ralph Gets Right

1. **Simplicity**: A bash loop is easy to understand, debug, and modify. NEEDLE's state machine is more robust but more complex.

2. **No concurrency overhead**: By being single-worker, Ralph avoids the entire class of bugs that plague NEEDLE at scale (claim races, SQLite contention, thundering herd).

3. **Model routing**: Per-bead model selection via labels is a good idea. NEEDLE could route beads to specific agent types based on labels.

4. **Compound memory**: The Initializer's pattern of extracting learnings into persistent files is valuable for long-running projects.

### Where NEEDLE Improves on Ralph

1. **Multi-worker**: NEEDLE runs 10-20 workers in parallel. Ralph is single-threaded. On a project with 200 beads, NEEDLE is 10-20x faster.

2. **Explicit outcomes**: NEEDLE classifies every result (success, failure, timeout, crash) and has handlers for each. Ralph checks "did the bead close?" -- everything else is a retry.

3. **Claiming**: NEEDLE claims beads before processing, preventing duplicate work across workers. Ralph assumes single-worker.

4. **Strand escalation**: When work runs out, NEEDLE explores other workspaces, runs maintenance, creates new beads. Ralph stops.

5. **Agent agnostic**: NEEDLE works with any headless CLI via YAML adapters. Ralph is tied to Claude (or Cursor in one variant).

6. **Timeout handling**: NEEDLE can timeout stuck agents. Ralph waits indefinitely.

### What NEEDLE Could Adopt from Ralph

1. **Model-label routing**: Let bead labels specify which agent/model to use, overriding defaults.
2. **Compound memory**: Extract patterns from successful bead processing into persistent knowledge files.
3. **Fresh context**: Consider resetting agent context between beads (NEEDLE already does this by invoking agents as separate processes).
4. **Prompt templates**: Ralph's `.ralph/prompts/` directory with reusable templates is cleaner than building prompts inline.
