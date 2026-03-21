# Mitosis Explosion Postmortem

Extracted from git log, bead history, and the 2026-03-18 bug fix session
notes.

## Timeline

### Phase 1: Initial Recursion (pre-v0.3.8)

A bead (nd-3dtt) was evaluated for mitosis. The LLM determined it should be
split into 5 children. Each child was created with `mitosis-child` label.
But `mitosis-child` was not in the `skip_labels` list, so each child was
itself evaluated for mitosis, split into 5 more children, and so on.

**Result:** 5,139 descendants across 6 generations. 5 unique titles repeated
exponentially.

**Cleanup:** 8+ batch commits closing recursive mitosis children.

### Phase 2: Fix Attempt 1 -- Block All Children (commit 6dbc151)

Added `mitosis-child` and `mitosis-parent` to `skip_labels`. Added a hard
guard rejecting all mitosis products.

**Problem:** This over-corrected. A child bead that genuinely contained
multiple tasks (e.g., "implement A, B, and C") could not be further
decomposed.

### Phase 3: Fix Attempt 2 -- Depth Guard (commit a2c266b)

Replaced the unconditional block with `max_depth` (default: 3) tracked via
`mitosis-depth:N` labels. Children at depth < max_depth could be re-split;
at max_depth they were blocked.

**Problem:** Labels were read from `br show --json`, which never included
them (upstream bug). The depth was always calculated as 0.

### Phase 4: The Big Explosion (2026-03-16, 5,741 beads)

With the depth guard silently ineffective (labels always empty from
`br show --json`):
- `mitosis-parent` check never fired -- parents were re-split
- `mitosis-pending` lock never fired -- concurrent workers both entered
- Depth guard never fired -- depth was always 0

**Result:** Recursive explosion across multiple workspaces:
- 253 duplicates in mobile-gaming
- 50 in NEEDLE
- 41 in kalshi-trading
- 40 in ibkr-mcp
- 5,741 total across 5 depth levels

Additionally, 606 zombie beads remained in `in_progress` from stopped
workers that had claimed beads but never released them.

### Phase 5: Root Cause Fix (2026-03-18, commits 8e9e706, 86ccfcd)

Discovered that `br show --json` never includes labels. Switched all label
reads to `br label list <id>`, which returns the correct data.

### Phase 6: Comprehensive Hardening (commit bc3c9f0)

Closed remaining gaps:
1. Use `br label add` instead of `br update --label` (some br versions
   silently fail on the latter)
2. Add workspace context to `br update --blocked-by` and `--release` calls
   (these were missing `cd "$workspace"`)
3. Fix test infrastructure to use `br label list` mock
4. Add 5 depth-guard regression tests

## Root Causes (Layered)

The explosion was not caused by a single bug but by a chain of failures:

```
1. br show --json omits labels (upstream bug)
   -> All label reads return empty
   -> Depth guard sees depth=0 always
   -> mitosis-parent check never fires
   -> mitosis-pending lock never fires

2. No workspace context on blocking calls
   -> br update --blocked-by silently fails
   -> Parent never blocked by children
   -> Parent returns to ready queue
   -> Re-claimed and re-split

3. No session-level loop guard
   -> Same worker can split the same bead multiple times per session
   -> No rate limiting on mitosis per bead

4. No hard limit on total children
   -> Exponential growth uncapped
   -> 5^6 = 15,625 potential beads from one parent
```

Each layer of defense failed silently, and no monitoring detected the
exponential growth until thousands of beads existed.

## Defenses Added (Final State)

1. **Label reads via `br label list`** -- correct data source
2. **Depth guard** (`max_depth: 3`) -- caps recursion depth
3. **Session loop guard** (`/dev/shm/needle-mitosis-guard-$$`) -- prevents
   re-splitting within a single worker session
4. **Label write verification** -- confirm label was actually written
5. **Pre-claim label gate** -- filter mitosis-parent/pending beads before
   claiming
6. **mitosis-pending lock** -- prevent concurrent workers from splitting
   the same bead
7. **Parent labeled before children** -- close the race window between
   analysis and label write

## Lessons for the Rewrite

### 1. Defense in depth for recursive operations

Mitosis is inherently recursive -- a bead creates children, which might
create more children. Every recursive system needs multiple independent
guards:
- Hard depth limit (not dependent on metadata that might be missing)
- Session-level deduplication
- Global rate limiting
- Total descendant count limit

### 2. Never trust metadata from tools you don't control

The `br show --json` upstream bug was invisible because the code assumed
labels would be present. Validate that expected fields exist before using
them. When a field is absent, fail loudly rather than defaulting to an
unsafe value (empty labels = no guards = unlimited recursion).

### 3. Monitor for exponential growth

A monitoring check like "alert if more than N beads are created in M
minutes" would have caught the explosion early. The system had no aggregate
monitoring -- it only checked individual bead states.

### 4. Test with the actual tool, not mocks

The test infrastructure mocked `br show --json` to return labels. The mock
was more correct than the real tool. Tests passed but production exploded.
At least some tests must use the real `br` CLI to catch upstream behavioral
differences.

### 5. Atomic guard-check-and-lock

The mitosis guard (read labels -> check depth -> write lock -> call LLM)
had multiple race windows because the steps were not atomic. Use a single
atomic operation for the guard: e.g., a database transaction that reads the
current state and writes the lock in one step, failing if the state was not
as expected.

### 6. Cleanup tooling must exist before the feature

The 606 zombie beads and 5,741 explosion beads required manual batch cleanup
commits. The system should have had tooling for bulk bead operations (close
all beads matching a pattern, release all stale claims) before mitosis was
enabled.

## Source Evidence

- Commit `6dbc151`: prevent recursive splitting (first attempt)
- Commit `a2c266b`: depth guard (second attempt)
- Commits `8e9e706`, `86ccfcd`: label read fix (root cause)
- Commit `bc3c9f0`: comprehensive hardening
- Commit `a08ba01`: mitosis-pending lock
- Commit `b435179`: label parent before children
- Commit `2d020a6`: pre-claim label gate
- Commit `82c766e`: close 606 zombie in_progress beads
- Commit `8e6f1d1`: close 155 recursive mitosis children
- 8 batch commits: close recursive mitosis children (10/75 per batch)
- 2 batch commits: close recursive mitosis children (10/4363 per batch)
