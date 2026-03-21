# Operational Fleet Lessons

Extracted from memory files, bead history, git log patterns, and the
bead-splitting and gap-analysis docs in NEEDLE-deprecated.

## Fleet Sizing and Capacity

### Worker Limits

The server (Hetzner EX44, 20 cores) could not sustain more than ~20
concurrent NEEDLE workers. At 40+ workers, the explore strand's filesystem
scans drove CPU load to 35+, degrading all workers.

**Lesson:** NEEDLE's worker count must be bounded by the server's capacity
for overhead operations (filesystem scans, database queries, heartbeats),
not just the number of available beads.

### Staggered Launches

Launching all workers simultaneously caused a thundering herd effect:
- All workers hit explore at the same time
- All workers tried to claim the same top-priority bead
- Database lock contention spiked

**Mitigation:** Stagger launches with `sleep 1-2` between each worker.
The rewrite should have built-in stagger logic.

### Workers Exit When Done

Workers complete their workspace's beads and then exit via `idle_timeout`.
This is normal behavior, not a crash. Operators must periodically relaunch
workers into workspaces with remaining work.

**Lesson:** The system needs either: (a) automatic worker redistribution
to busy workspaces, or (b) a supervisor that relaunches workers when work
appears.

## Bead Granularity

### The Splitting Report Findings

The bead-splitting-report.md found that 4 oversized beads (>4,000 characters,
10+ acceptance criteria) had higher failure rates. After splitting into 13
focused beads, timeout risk decreased and parallel work increased.

**Guidelines established:**
- Split if >4,000 characters or >10 acceptance criteria
- Split if bead mixes concerns (CLI + execution + recovery)
- Split if sequential phases ("first X, then Y, then Z")
- Keep together if single file/module, <3,000 characters, <8 criteria

**Common pattern: three-part split:**
1. Setup/infrastructure/framework (P0)
2. Core implementation/execution (P0)
3. Advanced features/cleanup/recovery (P1/P2)

### Timeout Escalations

~20% of closed beads had timeout escalations. Oversized beads were the
primary cause -- the LLM agent could not complete all acceptance criteria
within the timeout.

## The Gap Analysis Cycle

### First Pass (2026-03-07)

The gap analysis (nd-12nu) compared implementation against `docs/plan.md`
and found 14 gaps across CLI commands, strands, hooks, watchdog, telemetry,
self-update, billing, and infrastructure. 17 new beads were created.

### Second Pass (2026-03-08)

Many first-pass beads were completed. A second pass found 5 new gaps
including auto-init detection, workspace-level config overrides, and file
collision reconciliation.

**Lesson:** Gap analysis is valuable but must be repeated. The first pass
catches obvious gaps; subsequent passes catch gaps revealed by implementing
the first batch. However, this creates a feedback loop when running on the
NEEDLE workspace itself (see self-modification-risks.md).

## Version Management

### The Version Bump Problem

The git log shows 30+ `chore: bump version` commits, each paired with a
feature or fix commit. Version management was manual and error-prone:
- `constants.sh` version could drift from the release tag
- The build script had to sync the version from constants.sh
- CI auto-bump was added (commit 82ade80) but late in the project

**Lesson:** Version management should be automated from day one with a
single source of truth.

### Hot-Reload Deployment

Hot-reload (commits ff63d1d, 67dcbff, 2a96b8a) automatically deployed new
versions to running workers. While useful for rapid iteration, it amplified
bad changes (see self-modification-risks.md).

## The Bead Lifecycle Observation

### Agent-Owned Closure

The most important operational lesson was that bead closure should be the
agent's responsibility, not NEEDLE's post-dispatch lifecycle:

**Problem:** NEEDLE's post-dispatch parsing (`exit_code|duration|output_file`
from stream-parser.sh) failed silently, orphaning beads as `in_progress`.

**Solution:** Include instructions in the agent prompt for the LLM to:
1. Do the work
2. Commit and push
3. Validate the result against the bead spec
4. `br close <id>` if valid
5. `br update <id> --status blocked` with a comment if not

NEEDLE's post-dispatch closure became a fallback safety net, not the primary
mechanism.

## Provider-Specific Issues

### ZhipuAI (GLM) Proxy Errors (nd-f0vywe)

GLM workers through the zai-proxy encountered 422 errors from incompatible
Anthropic API fields. The proxy needed field translation for the ZhipuAI
backend.

### Heartbeat Format Mismatch

GLM-4.7 workers showed as `unknown (idle)` in `needle list` due to
heartbeat format differences. They were actually running. Operators had to
check tmux directly.

**Lesson:** Multi-provider support requires careful abstraction of agent
output formats, heartbeat conventions, and error handling.

## The Plan vs. Reality

### Feature Coverage (from plan-vs-beads-analysis.md)

At analysis time:
- **Phase 1 (MVP):** 60% complete, 4 critical beads open
- **Phase 2 (Full System):** 75% complete, 2 in progress
- **Phase 3 (Advanced):** 30% complete, 2 open + 5 missing

Testing infrastructure had 0% coverage (no beads). Documentation was
tracked informally.

### What Completed vs. What Did Not

Successfully implemented: CLI commands (100% tracked), all 7 strands,
core bead management, agent system, telemetry, configuration, process
management.

Never fully stabilized: mitosis (multiple explosion incidents), explore
(unbounded scanning), starvation detection (100% false positive rate),
build integrity (recurring module-missing bugs).

**Lesson:** The features that worked well were simple, isolated modules
(CLI commands, single-strand implementations). The features that caused
problems were cross-cutting concerns that interacted with multiple
subsystems (mitosis touches claiming, labels, database, LLM; explore
touches filesystem, workspace switching, engine restart; starvation
touches all strands plus alerting).

## Source Evidence

- `/home/coding/NEEDLE-deprecated/docs/bead-splitting-report.md`
- `/home/coding/NEEDLE-deprecated/docs/gap-analysis-nd-12nu.md`
- `/home/coding/NEEDLE-deprecated/docs/plan-vs-beads-analysis.md`
- `/home/coding/NEEDLE-deprecated/docs/streaming.md`
- Memory: `feedback_needle_operations.md` -- fleet ops lessons
- Memory: `feedback_needle_bead_lifecycle.md` -- agent-owned closure
- Memory: `project_needle_bugs_fixed.md` -- bugs fixed 2026-03-18
