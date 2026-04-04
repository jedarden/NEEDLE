# Self-Learning Agent Systems

## Research Date: 2026-04-04

## Overview

This document surveys current approaches for making autonomous coding agent fleets self-learning — systems that improve their own performance over time without manual prompt engineering. The research is motivated by NEEDLE's gap: workers generate rich telemetry but no feedback loop exists to turn that data into improved behavior.

## The Core Problem

NEEDLE workers execute beads, log outcomes, and move on. The telemetry goes in, but nothing comes back out. A worker that fails on a task today will fail the same way tomorrow. Knowledge discovered by one worker is invisible to all others. The fleet does not learn.

Self-learning closes this loop. The spectrum ranges from simple (file-based memory) to complex (evolutionary prompt optimization), with varying implementation cost and payoff.

---

## 1. File-Based Memory Patterns

### Learnings.md (Simplest, Highest Impact)

A markdown file per workspace that workers read before starting and write to before closing. Each entry records what was discovered during a bead's execution.

**Structure:**
```markdown
### 2026-04-04 | bead: needle-xyz | worker: alpha
- **Task type:** bug fix
- **Observation:** The API rate-limits at 5 req/s, not 10 as documented
- **Confidence:** high (verified empirically)
```

**Properties:**
- Keep under 80 active lines to fit in context windows
- Workspace-scoped and agent-agnostic — any NEEDLE worker can read it
- Entries include date, bead ID, and confidence level for staleness tracking
- Loaded via `context_files` in `.needle.yaml`

**Implementations in the wild:**
- Claude Code's built-in `MEMORY.md` system (per-user, per-project, 200-line index limit)
- The "Learnings.md Skill" pattern — CLAUDE.md instructs the agent to read before start, write before end
- Christopher Allen's bootstrap seed prompt — `.claude/learnings.md` with automatic promotion of repeated patterns to `.claude/rules/`

**Key insight from Allen's system:** Learnings that appear 2+ times get promoted to permanent rules. Rules exceeding 50 lines split into process docs. This creates a natural escalation path from observation to convention.

### AGENTS.md / CLAUDE.md as Shared Convention

AGENTS.md emerged mid-2025 as a cross-tool standard (Claude Code, Cursor, Copilot, Gemini CLI, Windsurf, Aider, Zed). Plain markdown, no schema. The closest AGENTS.md to the file being edited takes precedence.

NEEDLE already uses CLAUDE.md per workspace. The self-learning extension: workers that discover conventions can propose additions to CLAUDE.md, gated by a consolidation step.

### Bead Retrospectives

Extend the `br close` body with a structured retrospective block:

```markdown
## Retrospective
- **What worked:** Approach that succeeded
- **What didn't:** Approach that failed and why
- **Surprise:** Anything unexpected about the codebase/tooling
- **Reusable pattern:** If this task type recurs, do X
```

This data lives in the JSONL log (append-only, already the source of truth). A consolidation process extracts patterns from retrospectives into the workspace learnings file.

---

## 2. Consolidation Daemons

### KAIROS / autoDream Pattern

Discovered in Claude Code's internal system prompts (Piebald-AI/claude-code-system-prompts). A background daemon that performs memory consolidation while idle.

**Four-phase cycle:**
1. **Orient** — scan existing memory structures, check for duplicates
2. **Gather** — prioritize daily logs, detect drifted memories, grep transcript JSONL
3. **Consolidate** — merge new signal into existing topic files, convert relative dates to absolute, delete contradicted facts
4. **Prune** — maintain index under size limits (~150 chars per entry), resolve contradictions

Triggers after 24 hours and at least 5 sessions. The key discipline: consolidation runs in a separate context window from task execution, so it can focus entirely on pattern extraction.

### NEEDLE Application: Reflect Strand or Command

A new strand (Strand 4.5, between Explore and Weave) or standalone `needle reflect` command that:

1. Reads bead close bodies from `.beads/issues.jsonl` for the last N days
2. Reads current `.beads/learnings.md`
3. Extracts patterns: task type success/failure rates, common failure modes, recurring observations
4. Merges new learnings, deduplicates, prunes stale entries
5. Optionally proposes CLAUDE.md updates for validated conventions

**Design constraint:** The reflect agent must be a different invocation than task workers — fresh context focused on meta-analysis, not task execution.

---

## 3. AutoAgent: Meta-Agent Harness Optimization

### Source

[github.com/kevinrgu/autoagent](https://github.com/kevinrgu/autoagent) by Kevin Gu / ThirdLayer (YC W25). Released 2026-04-04.

### Results

- **96.5% on SpreadsheetBench** (#1 on leaderboard)
- **55.1% on TerminalBench** (#1 GPT-5 score)
- Every other entry on those leaderboards was hand-engineered. AutoAgent's wasn't.

### Architecture

AutoAgent splits agent improvement into two roles:

- **Meta-agent**: A coding agent (Claude Code, Codex) that reads `program.md` and iteratively modifies the task agent's harness
- **Task agent**: The actual agent solving benchmark tasks, defined in `agent.py`

The human writes `program.md` (what to optimize, how the loop works). The human never touches `agent.py` directly.

**Key files:**
- `program.md` — meta-agent directive: edit surface, experiment loop, overfitting rules, "NEVER STOP" directive
- `agent.py` — single-file harness with two sections:
  - Editable (above boundary): `SYSTEM_PROMPT`, `MODEL`, `MAX_TURNS`, `create_tools()`, `create_agent()`, `run_task()`
  - Fixed (below boundary): Harbor `BaseAgent` adapter, ATIF trajectory serialization, container entrypoint
- `agent-claude.py` — alternate harness using Claude Agent SDK instead of OpenAI Agents SDK

### The Improvement Loop

```
1. Establish baseline (unmodified harness, run all tasks)
2. Diagnose failures (read run.log + task trajectories + verifier output)
3. Group failures by root cause pattern
4. Choose one general improvement (class of failures, not single task)
5. Edit agent.py (prompt, tools, sub-agents, orchestration)
6. Git commit for traceability
7. Rebuild Docker + rerun all tasks
8. Score comparison → record in results.tsv
9. Keep/discard decision:
   - passed count improved → keep
   - passed same but harness simpler → keep
   - otherwise → git revert
10. Loop forever (meta-agent does not stop until human interrupts)
```

### Key Design Decisions

**Single-file harness:** The meta-agent needs full context of the entire system without navigating multiple files. One file = one context load.

**Same-model pairing ("model empathy"):** Claude meta-agent + Claude task agent outperforms Claude meta-agent + GPT task agent. The meta-agent shares weights with the task agent and has implicit understanding of its own limitations and reasoning patterns. When it reads the task agent's failure traces, it recognizes the failure mode as part of its own worldview.

**Practical consequence:** If your fleet runs Claude workers, use Claude as the meta-agent. Shared-weight pairings consistently outperform cross-model optimization.

**Traces over scores:** When only pass/fail scores are provided (no execution traces), improvement rate drops hard. Understanding *why* something improved matters as much as knowing *that* it improved. Traces give the meta-agent interpretability over the task agent's reasoning.

**The overfitting test:** A self-reflection rule embedded in `program.md`: *"If this exact task disappeared, would this still be a worthwhile harness improvement?"* If the answer is no, it's overfitting. Enforced purely through meta-agent instructions — no code-level mechanism.

**Prompt tuning ceiling:** `program.md` explicitly warns: "Prompt tuning alone has diminishing returns." The high-leverage axis is tool design — adding structured tools that reduce failure modes by surfacing structured data and clear error messages.

### Emergent Behaviors (Not Pre-Programmed)

The meta-agent autonomously developed these patterns during 24+ hour optimization runs:

1. **Spot checking** — ran isolated tasks for small edits instead of full suite, dramatically speeding up iteration
2. **Forced verification loops** — built deterministic self-checks and formatting validators, budgeted extra turns for self-correction
3. **Writing tests** — steered the task agent to build its own unit tests and checks for each task
4. **Progressive disclosure** — dumped long contexts to files when results overflowed, started agents with limited tools and expanded as needed
5. **Orchestration logic** — built task-specific sub-agents and handoffs when the domain required it
6. **Verification sub-agents** — wrapped a verification agent as a tool that re-reads output against task requirements before concluding

### What Doesn't Work

- **Single agent self-improvement** — models building tools for themselves often ignore those tools. The meta/task split is essential — being good at a domain and being good at improving at that domain are different capabilities.
- **Codex as meta-agent** — ignores "LOOP FOREVER" instructions, gives up too early. Claude Opus 4.6 sustained 12+ hours and 118 experiments.
- **Score-only feedback** — without traces, the meta-agent guesses at root causes. Improvement rate drops dramatically.
- **No train-test split** — AutoAgent evaluates on the same tasks it optimizes against. The overfitting rule is a heuristic guard, not a statistical one.

### Why This Matters for NEEDLE

The core insight: **agents are better at understanding agents than we are.** We project our own intuitions onto systems that reason differently. A meta-agent reading failure traces already has implicit understanding of itself — its own limitations, tendencies, reasoning patterns.

As agents surpass 99th percentile human performance on domain tasks, human intuitions about good harness design become the wrong prior. The agent should discover harness improvements from first principles.

---

## 4. Eval-Driven Improvement

### Anthropic's 8-Step Eval Roadmap

From "Demystifying Evals for AI Agents" (anthropic.com/engineering):

1. Start with 20-50 tasks from real failures
2. Convert manual tests from your dev workflow
3. Write unambiguous tasks with reference solutions
4. Build balanced problem sets (positive and negative cases)
5. Create isolated, reproducible harnesses
6. Design graders: prefer deterministic checks, use model-based for subjective quality
7. Read transcripts regularly to verify grader accuracy
8. Monitor saturation and develop harder evals

**Three grader types:** Code-based (fast, deterministic), model-based (flexible, rubric scoring), human (gold standard for calibration).

**Key metrics:** pass@1 (first-try success), pass@k (at least one success in k tries), pass^k (all k trials succeed — critical for consistency).

### DSPy

Stanford's framework for programmatic prompt optimization. MIPROv2 optimizer uses Bayesian search across prompt variations. Defines signatures (what), metrics (how to score), and examples (training data). Production-ready for pipelines but requires measurable evals.

### NEEDLE Application

NEEDLE already has the raw data for eval-driven improvement:

- Telemetry logs every outcome (success/failure/timeout/crash)
- Prompt hashes link outcomes to specific template versions
- Bead close bodies describe what was done

**Missing pieces:**
- Template version tagging in telemetry events
- `needle stats` command aggregating success rates by template version
- A/B test infrastructure for template modifications

---

## 5. Tool and Workflow Learning

### Voyager Pattern (Skill Libraries)

From NVIDIA/UT Austin (Voyager, 2023, TMLR 2024): An LLM-powered agent with an ever-growing skill library of executable code indexed by task type.

**Properties:**
- Skills are temporally extended, interpretable, and compositional
- Voyager obtained 3.3x more unique items and unlocked milestones 15.3x faster than baselines
- The skill library transfers to new environments

### NEEDLE Application

Store proven approaches in `.beads/skills/`:

```
.beads/skills/
  api-rate-limit-handling.md
  database-migration-pattern.md
  flaky-test-diagnosis.md
```

Each skill includes task type tags, success count, last-used date, and the actual procedure. The pluck template retrieves relevant skills based on bead labels/title before dispatching.

**Distinction from learnings:** Learnings are observations ("the API rate-limits at 5/s"). Skills are procedures ("here's how to handle rate limiting"). Learnings feed into skills when a pattern proves reliable.

---

## 6. Fleet-Level Learning

### Cross-Workspace Knowledge

Current state: each NEEDLE workspace is an island. Workers in `kalshi-weather` don't benefit from lessons in `NEEDLE`.

**Options:**
1. **Global learnings file** at `~/.config/needle/global-learnings.md` — loaded into all prompts as supplementary context
2. **Label-based skill retrieval** — skills tagged with generic labels (`rust`, `kubernetes`, `api`) available to any matching workspace
3. **Graduated autonomy** — track per-worker success rates by task type, expand permissions for proven workers

### Multi-Agent Team Patterns

- **Claude Code Agent Teams** (Feb 2026): Team lead coordinates teammates with independent context windows. No shared history — shared context is project files themselves.
- **Ralph Wiggum Loop**: Bash loop restarting agents that read plan files. Community implementations add spend limits, circuit breakers, git checkpointing.
- **Planner-Worker-Judge model**: Planner reads codebase, Workers implement tasks, Judges assess completion.

### Centralized vs. Distributed Knowledge

**Centralized:** CLAUDE.md in the repo as single source of truth. All agents read; designated agents write after validated discoveries.

**Distributed:** Each agent maintains session learnings. Consolidation merges findings.

**Hybrid (most common):** Shared repo-level instructions + per-agent session state. Git is the coordination layer.

---

## 7. Evolutionary Frameworks (Frontier)

| Project | What It Does | Maturity |
|---------|-------------|----------|
| **AutoAgent** (kevinrgu) | Meta-agent optimizes task agent harness via traces and evals | Production-proven (leaderboard #1) |
| **EvoAgentX** | Build, evaluate, auto-evolve LLM agents through iterative feedback | Research-grade, 2.7K stars |
| **A-Evolve** | Git-native self-rewriting with gated validation (Solve→Observe→Evolve→Gate→Reload) | Functional, early |
| **OpenEvolve** | Open-source AlphaEvolve: LLMs for code modifications + automated metrics | HuggingFace release |
| **SICA** | Agent works on its own codebase: evaluate on benchmarks, modify own source | Academic (ICLR 2025 Workshop) |
| **Agent0** | Two competing agents: Curriculum Agent proposes tasks, Solver Agent attempts | Research prototype |
| **DSPy GEPA** | Genetic-Pareto reflective optimizer for textual system components | Production-ready |

---

## 8. Claude Code Hooks for Self-Learning

Claude Code provides 25 hook events that enable self-learning behavior without custom infrastructure:

- **PostToolUse** — log every tool call with context and outcome
- **Stop** — trigger retrospective analysis after task completion
- **SessionStart** — inject learned patterns from previous sessions
- **Agent-type hooks** (`"type": "agent"`) — spawn a sub-agent that reads files, searches code, and verifies conditions

**Practical self-learning setup:** A `PostToolUse` hook logging Bash commands, combined with a `Stop` hook that analyzes the session's tool sequence and appends successful patterns to a playbook loaded via `SessionStart`.

---

## 9. Key Takeaways for NEEDLE

1. **File-based memory is the 80/20 play.** Learnings.md + bead retrospectives + consolidation daemon gives 80% of the value at 20% of the complexity. No infrastructure required.

2. **Traces are everything.** AutoAgent's breakthrough finding: improvement rate drops hard without full execution traces. NEEDLE workers should capture tool call sequences, agent reasoning, and verifier outputs — not just exit codes.

3. **The meta/task split is essential.** Self-improvement where a single agent modifies its own configuration doesn't work well. A separate meta-agent that reads failure traces and edits the harness is dramatically more effective.

4. **Same-model pairing wins.** If NEEDLE workers use Claude, the meta/improvement agent should also use Claude. "Model empathy" produces better harness edits.

5. **Prompt tuning has diminishing returns.** The high-leverage axis is tool design and orchestration logic, not prompt rewording. AutoAgent's emergent behaviors (verification sub-agents, spot checking, progressive disclosure) were all structural changes, not prompt tweaks.

6. **The overfitting test is simple and effective.** "If this exact task disappeared, would this still be a worthwhile improvement?" prevents task-specific hacks without complex validation infrastructure.

7. **Git is the coordination protocol.** Shared files in the repo (CLAUDE.md, learnings, task lists) are how multiple agents share knowledge. Version control gives you auditability, rollback, and A/B testing for free.

---

## References

- [AutoAgent](https://github.com/kevinrgu/autoagent) — Meta-agent harness optimization
- [Self-Improving Claude Code Bootstrap Seed](https://gist.github.com/ChristopherA/fd2985551e765a86f4fbb24080263a2f)
- [Anthropic: Demystifying Evals for AI Agents](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents)
- [DSPy](https://github.com/stanfordnlp/dspy) — Programmatic prompt optimization
- [Voyager](https://voyager.minedojo.org/) — Skill library pattern
- [KAIROS/autoDream](https://github.com/Piebald-AI/claude-code-system-prompts) — Memory consolidation daemon
- [A-Evolve](https://www.opensourceforu.com/2026/04/open-source-a-evolve-brings-self-rewriting-ai-workflows-to-startups/)
- [EvoAgentX](https://github.com/EvoAgentX/EvoAgentX) — Self-evolving agent framework
- [Awesome Self-Evolving Agents](https://github.com/EvoAgentX/Awesome-Self-Evolving-Agents) — Survey + curated list
- [Self-Improving Coding Agents (Addy Osmani)](https://addyosmani.com/blog/self-improving-agents/)
- [Martin Fowler: Harness Engineering](https://martinfowler.com/articles/harness-engineering.html)
- [Mem0](https://mem0.ai/blog/state-of-ai-agent-memory-2026) — Structured agent memory
- [Claude Code Agent Teams](https://code.claude.com/docs/en/agent-teams)
