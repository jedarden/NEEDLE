# Worker Starvation Lessons

Extracted from 16 starvation alert analyses, the false-alarm/false-positive docs,
the stuck-behavior research, and the alternative-solutions doc in NEEDLE-deprecated.

## The Problem

NEEDLE workers cycle through a 7-strand priority waterfall looking for work.
When all strands return "no work found," the worker creates a HUMAN-type bead
(a starvation alert) asking a human to investigate. In practice, **nearly 100%
of these alerts were false positives** -- work existed but the worker could not
see it.

Over a single week (2026-03-03 to 2026-03-07), at least 16 starvation alert
beads were created and investigated. Every single one was resolved as a false
positive. The database typically showed 20-40 ready beads at the time of each
alert.

## Root Causes

### 1. Broken dependency filtering (nd-329)

The bead selection fallback used `dependency_count == 0` to find claimable
beads. This excluded beads whose dependencies were all **closed** -- they
looked blocked but were actually ready. NEEDLE's `select.sh` was fixed to
check each dependency's status, but the external worker and some code paths
still used the old logic.

### 2. Debug output polluting stdout (nd-165 true positive)

`_needle_debug` and `_needle_verbose` wrote to stdout. When called inside
subshells (`result=$(some_function)`), the debug text corrupted the JSON
output, causing jq to fail silently and return zero candidates.

**Fix:** Redirect all debug/verbose output to stderr (`>&2`).

### 3. Workspace directory not honored (nd-165)

The `br` CLI operates on the current working directory. The fallback bead
selection in `select.sh` did not `cd "$workspace"` before calling `br list`,
so it queried the wrong database (or no database at all).

Similarly, `claim.sh` ran `br update --claim` without switching to the
workspace directory first, causing `NOT_INITIALIZED` errors.

### 4. Hardcoded workspace path in br wrapper (nd-2nl)

The `br` wrapper script at `~/.local/bin/br` had a hardcoded path to the
FABRIC workspace's ready-queue JSON. Workers in the NEEDLE workspace received
FABRIC beads (`bd-*` prefixed) instead of NEEDLE beads (`nd-*`). Fixed by
making the wrapper detect the workspace dynamically via `.beads/` directory
walk.

### 5. All beads claimed by other workers (nd-7dp3)

One legitimate scenario: all open beads were assigned to active workers.
The starvation detection did not distinguish "no beads exist" from "all beads
are claimed." The correct response for the latter is to wait, not alert.

### 6. Stuck claims blocking the ready queue (nd-356)

Beads stuck in `in_progress` with stale `claimed_by` values prevented other
workers from seeing them as available. The `br update --status open` command
failed due to a CHECK constraint requiring claim fields to be cleared
simultaneously with status change. Resolution required direct SQL.

### 7. Transient issues

Database lock contention during high concurrency, brief windows where all
beads were mid-claim, and stale worker state all caused transient failures
that resolved by the next iteration -- but the alert had already been created.

## The False Positive Feedback Loop

The starvation system created a vicious cycle:

```
Worker cannot find work (transient or bug)
  -> Worker creates HUMAN alert bead
  -> Alert bead consumes human attention to investigate
  -> Investigation finds work exists
  -> Human closes alert as false positive
  -> Worker creates another alert bead
  -> Repeat indefinitely
```

At the peak, workers were generating multiple false alerts per hour,
each requiring manual investigation and closure.

## What Was Tried

1. **Pre-flight verification** before creating alerts -- run `br ready` as a
   double-check. This was the most effective single intervention.
2. **Cleanup scripts** (`bin/needle-cleanup-false-starvation`) to batch-close
   false positives.
3. **Alert rate limiting** -- one alert per hour per workspace.
4. **Bug bead tracking** (nd-32x) for the external worker discovery mechanism.
5. **Diagnostic report generation** -- collecting environment, PATH, database
   status, and strand results before alerting.

## Lessons for the Rewrite

### 1. Never alert without verification

Before creating any "stuck" or "starvation" alert, the system must
independently verify the claim using a different code path than the one
that failed. The pre-flight check was the single most impactful fix.

### 2. Separate "no work exists" from "no work available to me"

Three distinct states:
- No open beads at all (create work or exit)
- Open beads exist but all are claimed (wait)
- Open beads exist but I cannot see them (discovery bug)

Each requires a different response. The old system conflated all three.

### 3. Debug output must never go to stdout

Any function whose return value is captured via `$()` must send diagnostics
to stderr. This should be enforced architecturally (e.g., a logging subsystem
that always writes to fd 2), not per-function.

### 4. Workspace context must be explicit, not implicit

The `br` CLI's reliance on `$PWD` caused bugs in every subsystem that
forgot to `cd` first. The new system should pass workspace as an explicit
parameter to all database operations, not rely on current directory.

### 5. Stale claims need automated cleanup

The mend strand's stale claim detection was the right idea but needed to
run more aggressively (the default threshold was 1 hour, and the CHECK
constraint bug prevented programmatic release). Claim TTLs with automatic
expiry would prevent stuck claims from blocking the queue.

### 6. Rate-limit alerts and require cooldown

Creating HUMAN beads for every failure iteration floods the system. Alerts
should be rate-limited (implemented), de-duplicated (partially implemented),
and require a cooldown period before a new alert can be created for the same
condition.

### 7. The "stuck worker" research was solid but never fully implemented

The worker-stuck-behavior-research.md document identified 8 alternative
strategies with a detailed comparison matrix. The hybrid approach (phased
escalation: immediate retry -> health check -> pre-flight -> backoff ->
diagnostic report) was recommended but only partially implemented. The
rewrite should adopt this phased approach from the start.

## Source Files

- `/home/coding/NEEDLE-deprecated/docs/worker-starvation-alert-nd-*.md` (16 files)
- `/home/coding/NEEDLE-deprecated/docs/worker-starvation-false-positive.md`
- `/home/coding/NEEDLE-deprecated/docs/worker-starvation-false-alarm-analysis.md`
- `/home/coding/NEEDLE-deprecated/docs/worker-starvation-alternatives.md`
- `/home/coding/NEEDLE-deprecated/docs/worker-stuck-behavior-research.md`
- `/home/coding/NEEDLE-deprecated/docs/br-wrapper-workspace-fix.md`
- `/home/coding/NEEDLE-deprecated/docs/nd-356-resolution.md`
