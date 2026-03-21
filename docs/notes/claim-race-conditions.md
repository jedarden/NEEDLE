# Claim Race Conditions

Extracted from git log analysis, bead history, and the lock/claim commit
messages in NEEDLE-deprecated.

## The Problem

Multiple NEEDLE workers compete for the same bead queue. Without proper
serialization, several classes of race condition emerged:

1. **Thundering herd** on `br ready` + `br update --claim`
2. **Duplicate bead execution** from TOCTOU races
3. **Mitosis split storms** from concurrent workers analyzing the same bead
4. **Claim loops** from re-claiming beads that were just released

## Race 1: Thundering Herd on Claim

**What happened:** All workers called `br ready` at roughly the same time,
got the same list of candidates, and all attempted `br update --claim` on
the same top-priority bead. Only one succeeded; the rest wasted a full
iteration.

**Root cause:** No serialization between the read (query) and the write
(claim). This is a classic TOCTOU (time-of-check, time-of-use) race.

**Fix (commit 06387e0):** Added a per-workspace `/dev/shm` flock that
serializes all bead mutation operations. Only one worker can read candidates
and claim within a workspace at a time.

```
fix(claim): add per-workspace /dev/shm lock to prevent thundering herd
```

**Secondary fix (commit 1a89b77):** Wired the lock into the bundler and all
mutation paths -- pluck, mend, explore, and claim all acquire the workspace
lock before any bead operation.

## Race 2: TOCTOU on Claim of Closed Beads

**What happened:** Worker A claims bead X. Worker B queries `br ready` and
sees bead X (the query ran before A's claim propagated). B's claim attempt
succeeds on a bead that A already finished and closed, because `br --claim`
checked `status != closed` but A cleared the assignee field before setting
status to closed (two separate operations).

**Root cause:** The claim and close operations were not atomic. The window
between clearing `claimed_by` and setting `status = closed` allowed another
worker to re-claim.

**Fix:** The `/dev/shm` workspace lock (same as thundering herd fix) ensures
only one worker performs bead operations at a time, closing the TOCTOU window.

## Race 3: Mitosis Split Storm (nd-v2kgi)

**What happened:** Two workers simultaneously evaluated the same bead for
mitosis (task splitting). Both passed the skip-label checks (because no
mitosis labels existed yet), both called the LLM for analysis (seconds-long
operation), and both independently created a full set of child beads --
doubling the work.

**Root cause:** The guard check (reading labels) and the lock write (adding
`mitosis-pending` label) were not atomic. The LLM call created a wide window
where both workers were past the guard but neither had written the lock.

**Fix (commit a08ba01):** Immediately after all fast guards pass and before
the LLM call, write a `mitosis-pending` label to the bead. A second worker
reading the bead after this write sees the label and bails out. This narrows
the race window from seconds (full LLM duration) to milliseconds (single
label write).

## Race 4: Mitosis-Parent Re-Claim Loop (nd-o18v2z)

**What happened:** Workers re-claimed mitosis-parent beads every 3 seconds
in an endless loop: claim -> mitosis-check -> release (reason=mitosis) ->
claim again.

**Root cause chain:**
1. After mitosis, `br update --blocked-by` was supposed to block the parent
   on its children. But `--blocked-by` is not a valid `br` flag -- the call
   silently failed.
2. The parent bead returned to `open` (unblocked) status immediately.
3. The `mitosis-parent` skip guard only ran inside `_needle_check_mitosis()`,
   which executes **after** claiming -- too late to prevent the loop.

**Fix (commit 2d020a6):** Added a **pre-claim** label gate. Before sorting
and iterating candidates, query `br list --label-any mitosis-parent
--label-any mitosis-pending` and filter those IDs from the candidate pool.
This prevents the claim from ever happening on already-split beads.

## Race 5: Mitosis Parent Label Timing (nd-s5wcm)

**What happened:** `_needle_perform_mitosis` applied the `mitosis-parent`
label **after** creating all children. A concurrent worker could claim and
split the same parent bead in the window between the first child creation
and the late label write.

**Fix (commit b435179):** Move the `mitosis-parent` label write to
immediately after the "mitosis started" event and before the child-creation
loop. On failure (no children created), the label is rolled back.

## The Missing Lock Functions (nd-096dsr)

**What happened:** The functions `_needle_acquire_claim_lock` and
`_needle_release_claim_lock` were called in 7+ locations throughout the
bundled needle script but were **never defined**. Every workspace claim
attempt failed with `command not found`, putting all 4 workers into a dead
loop.

**Root cause:** The lock functions were added to call sites but the
implementation file (`src/lib/locks.sh`) was not included in the build. The
build script's module list was manually maintained, and the new file was
missed.

## Lessons for the Rewrite

### 1. All bead operations need serialization

The per-workspace flock approach worked. The rewrite should use a similar
mechanism from day one -- not bolt it on after discovering races in production.
Consider using a proper mutex (flock, database-level locking, or an advisory
lock) rather than label-based soft locks.

### 2. Labels are not locks

Using bead labels (`mitosis-pending`, `mitosis-parent`) as concurrency
control was fragile. Labels are metadata, not synchronization primitives.
The write-read-check pattern has an inherent race window. Use actual locks
(filesystem flock, database advisory locks) for mutual exclusion.

### 3. Pre-claim filtering is essential

Filtering at the candidate selection stage (before claim) is cheaper and
safer than post-claim validation. The mitosis-parent re-claim loop was caused
by validating after claiming. Filter early, validate late.

### 4. Silent failures are deadly

`br update --blocked-by` silently failing (invalid flag) caused an infinite
loop. Every external command invocation must check its exit code and log
failures. In bash, this means `set -e` or explicit `|| { handle_error; }`
on every `br` call.

### 5. Build systems must validate completeness

The missing lock functions reached production because the build script had a
manually maintained module list. The rewrite's build system should
auto-discover source files or validate that all referenced functions are
defined in the output.

### 6. Atomic operations prevent most races

The root cause of most races was non-atomic multi-step operations (read then
write, label then create, clear then close). Where possible, use single
atomic operations (database transactions, `flock` held across the full
read-modify-write cycle).

## Source Evidence

- Commit `06387e0`: per-workspace /dev/shm lock
- Commit `1a89b77`: wire lock into bundler and all mutation paths
- Commit `a08ba01`: mitosis-pending lock for split storms
- Commit `b435179`: label parent before child creation
- Commit `2d020a6`: pre-claim label gate for mitosis-parent loop
- Bead `nd-096dsr`: missing lock functions breaking all workers
- Bead `nd-v2kgi`: concurrent split storm
- Bead `nd-o18v2z`: claim-loop
- Bead `nd-s5wcm`: parent labeled after children created
