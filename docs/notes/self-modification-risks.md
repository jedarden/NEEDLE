# Self-Modification Risks

Extracted from git log, bead history, and operational experience documented
in the NEEDLE-deprecated codebase.

## The Problem

NEEDLE workers are LLM agents that write code. When those workers are
assigned beads to implement NEEDLE itself, they modify the very codebase
that controls them. This creates a class of problems unique to
self-modifying systems.

## Incident 1: Workers Breaking Their Own Build

**What happened:** NEEDLE workers were assigned beads to implement NEEDLE
features (e.g., nd-3b23 "Fix build.sh concatenation"). A worker modified
`build.sh`, committed, and pushed. The next `needle upgrade` pulled the
broken build, rebuilt the binary, and deployed it via hot-reload to all
running workers. All workers immediately failed.

**Root cause:** The LLM agent did not validate the build output before
committing. It modified the concatenation logic, but the resulting
`dist/needle` binary had syntax errors.

**Impact:** All workers in the fleet went down simultaneously because
hot-reload deployed the broken binary to every running worker.

## Incident 2: Hot-Reload Amplifying Bad Changes

**What happened:** NEEDLE implemented hot-reload (commits ff63d1d, 67dcbff)
-- workers poll the binary's mtime and re-exec themselves when it changes.
Combined with `needle upgrade` (which pulls latest, rebuilds, and installs),
a broken commit could propagate to the entire fleet within seconds.

**Chain of events:**
1. Worker A completes a bead that modifies NEEDLE source
2. Worker A commits and pushes
3. Another worker (or CI) runs `build.sh` and `needle upgrade --install`
4. Hot-reload triggers on all running workers
5. All workers re-exec with the new (potentially broken) binary

**Mitigation added:** The build script now runs `bash -n` syntax checking.
But semantic errors (wrong logic, missing functions, incorrect behavior)
are not caught.

## Incident 3: Workers Creating Infinite Work

**What happened:** The weave strand (gap analysis) creates new beads when it
finds gaps between the plan and the implementation. Workers running weave on
the NEEDLE workspace created beads for NEEDLE features. Other workers picked
up those beads and implemented them. Some implementations introduced new
gaps, which weave detected, creating more beads, which created more gaps...

**Root cause:** No limit on how many beads weave could create per run. The
original design was "there is no cap on how many beads weave can create --
if it finds many gaps, it creates all necessary beads." Combined with
self-referential work (NEEDLE improving NEEDLE), this created a feedback
loop.

## Incident 4: Mitosis Explosion on NEEDLE's Own Beads

**What happened:** The mitosis system (automatic task decomposition) was
applied to NEEDLE's own beads. A large bead describing a NEEDLE feature was
split into children, which were themselves large enough to split. Without
depth limits, this produced thousands of duplicate beads (see
bead-lifecycle-bugs.md).

**The irony:** The mitosis system was a NEEDLE feature being developed by
NEEDLE workers. Workers split their own task-splitting feature, creating
an explosion of task-splitting tasks.

## Incident 5: Workers Modifying Their Own Prompt

**What happened:** NEEDLE workers were assigned beads to improve NEEDLE's
prompt templates. A worker modified the prompt that future workers (including
itself after hot-reload) would receive. A poorly crafted prompt change caused
subsequent workers to produce lower-quality output, which created more beads
to fix the prompt, which produced more poor changes...

**Mitigation:** Prompt changes were eventually gated behind human review
(HUMAN-type beads).

## The Fundamental Tension

NEEDLE's value proposition is automating software development. NEEDLE itself
is software that needs development. The temptation to use NEEDLE to develop
NEEDLE is strong because:

1. It validates the tool (dogfooding)
2. It accelerates development
3. It demonstrates capability

But self-modification creates:

1. **Feedback loops** -- changes to the system affect the system's ability
   to make changes
2. **Cascading failures** -- a bad change propagates to all workers
3. **Infinite recursion** -- meta-work (improving the improver) has no
   natural stopping condition
4. **Loss of human oversight** -- changes deploy faster than humans can
   review them

## Lessons for the Rewrite

### 1. Self-modification must be gated

NEEDLE should never automatically deploy changes to itself without human
approval. Hot-reload of the orchestration binary is too dangerous when
workers can modify the source. Options:
- Require a human-approved release process for NEEDLE changes
- Pin workers to a specific NEEDLE version during a work session
- Separate "NEEDLE development" from "NEEDLE operation" environments

### 2. Limit blast radius

When a bad binary is deployed, it should affect one worker, not all of them.
Options:
- Canary deployments (one worker gets the new version first)
- Rollback on failure (if the new binary crashes, revert automatically)
- Version pinning per worker (workers don't all upgrade simultaneously)

### 3. Gap analysis needs bounds

The weave strand should have strict limits when operating on the workspace
that contains NEEDLE itself:
- Maximum beads per weave run
- Cooldown between weave runs on the same workspace
- Human review gate for beads that modify orchestration code

### 4. Separate the controller from the controlled

The NEEDLE binary that orchestrates workers should be a stable, tested
artifact, not a moving target. Workers should modify application code in
separate workspaces, not the orchestration code itself. If NEEDLE
development is desired, it should run in a sandboxed environment with
explicit promotion gates.

### 5. Validate before deploying

Any change to NEEDLE source must pass:
- Syntax checking (`bash -n` or equivalent)
- All unit tests
- All integration tests against the bundled binary
- A smoke test (start a worker, claim a bead, close it)

This validation should be mandatory before the change can be deployed,
even (especially) when the change was made by a NEEDLE worker.

### 6. Version immutability during sessions

Once a NEEDLE worker starts, it should run the same version for the duration
of its session. Hot-reload was a net negative because it propagated errors
faster than humans could intervene. Workers should check for updates between
sessions (between bead completions), not mid-execution.

## Source Evidence

- Commits `ff63d1d`, `67dcbff`: hot-reload via binary mtime polling
- Commit `2a96b8a`: auto-signal workers to hot-reload after install
- Commit `06aed0e` (nd-3b23): fix build.sh -- worker broke its own build
- Bead `nd-3b23`: "Fix build.sh concatenation to produce valid bash syntax"
- Weave strand design: strands.md "there is no cap on how many beads weave
  can create"
- Mitosis explosion: 5,741 duplicate beads across NEEDLE's own workspace
- Memory file: feedback_needle_operations.md -- fleet ops lessons
