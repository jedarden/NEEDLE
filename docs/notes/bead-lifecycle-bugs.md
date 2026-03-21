# Bead Lifecycle Bugs

Extracted from git log, bead history, and the memory files documenting the
2026-03-18 bug fix session.

## The Problem

NEEDLE manages beads through a lifecycle: open -> in_progress (claimed) ->
closed (or back to open on failure). Several bugs caused beads to get stuck,
multiply uncontrollably, or become impossible to close.

## Bug 1: Mitosis Infinite Recursion (5,139+ duplicate beads)

**What happened:** Bead nd-3dtt exploded into 5,139 descendants -- 5 unique
titles repeated across 6 generations. Each child bead was itself split into
the same 5 children, cascading exponentially.

**Root cause:** The `mitosis-child` and `mitosis-parent` labels were missing
from the `skip_labels` list in the mitosis guard. Child beads passed the
guard check and were re-split indefinitely.

**Fix (commit 6dbc151):**
1. Added `mitosis-child`/`mitosis-parent` to default `skip_labels`
2. Added a hard guard in `_needle_check_mitosis` rejecting mitosis products
   regardless of config (defense in depth)
3. Stopped the heuristic fallback from copying the full parent description
   into every child

**Follow-up fix (commit a2c266b):** The first fix over-corrected by blocking
ALL mitosis-child beads from re-splitting. A child that genuinely contains
multiple tasks should be split further. Replaced the unconditional block with
a `max_depth` guard (default: 3) tracked via `mitosis-depth:N` labels.

**Cleanup:** Required 8+ batch commits closing recursive mitosis children,
then another commit closing 155 pre-v0.3.8 children, and finally closing
606 zombie `in_progress` beads from stopped workers.

## Bug 2: Labels Never Visible (br show --json upstream bug)

**What happened:** Every label-based guard in mitosis silently failed. The
depth guard never detected `mitosis-child` (depth was always 0). The
`mitosis-parent` check never fired. The `mitosis-pending` lock never worked.

**Root cause:** `br show --json` **never included labels in its output**.
This was an upstream bug in beads_rust. All code reading labels via
`jq '.labels'` on `br show --json` output got an empty result, and the
guards passed when they should have blocked.

**Fix (commits 8e9e706, 86ccfcd):** Read labels via `br label list <id>`
instead of parsing `br show --json`. This was the single most critical fix
-- it unblocked all label-based guards.

**Scale of damage:** 253 duplicate beads in mobile-gaming, 50 in NEEDLE,
41 in kalshi-trading, 40 in ibkr-mcp. The 2026-03-16 explosion alone
produced 5,741 duplicate beads across 5 depth levels.

## Bug 3: Genesis Bead Lifecycle (v0.11.0)

**What happened:** Genesis beads (root tracking beads for multi-phase
projects) were permanently blocked from mitosis. They had `no-mitosis`
labels applied automatically, preventing them from ever creating
next-phase children.

**Root cause:** The mitosis system treated genesis beads as special cases
that should never be split, applying a permanent skip label. But genesis
beads need to trigger phase expansion -- creating child beads for the
next phase based on the project plan.

**Fix (commit 1e8fedf, b6cecd0):** Removed the automatic `no-mitosis`
label from genesis beads. Genesis beads now flow through mitosis normally,
where the LLM analyzes the plan document and creates phase-specific
children.

## Bug 4: Stale Dependency Links

**What happened:** When a blocking bead closed, the dependency links on
beads it blocked remained active. Those blocked beads appeared permanently
stuck even though their blockers were resolved.

**Root cause:** The `br dep` system records dependencies but does not
auto-clean them when the blocking bead closes. The dependency_count field
remained > 0 even though all dependencies were closed.

**Fix:** The mend strand was extended (bead nd-exqdwk) to detect and clean
stale dependency links on closed beads. Manual workaround: `br dep remove`.

## Bug 5: Workers Cannot Close Beads (verify function missing)

**What happened:** Workers executed beads successfully (code was written,
commits were made) but could not close them. Every bead remained in
`in_progress` status indefinitely.

**Root cause (commit 4a35e95):** The `src/bead/verify.sh` file had a
top-level `return 0` as a source guard. When the build script concatenated
all modules into a single file, this `return 0` was no longer inside a
function -- it terminated the entire script at that point, preventing all
subsequent function definitions from being registered.

**Secondary root cause (build re-source guard, v0.13.0):** The build script
set `_NEEDLE_*_LOADED=true` variables at the top of the bundled binary but
did not strip the re-source guards from modules. These guards (`if [[ ${_NEEDLE_FOO_LOADED:-} == true ]]; then return 0; fi`) then skipped all
function definitions in the bundled binary.

**Fix:** Build script now strips re-source guard blocks (both `return 0`
and `else` patterns). The bare `return 0` in verify.sh was replaced with
an `if/else` block.

## Bug 6: Post-Dispatch Closure Fails Silently

**What happened:** NEEDLE's post-dispatch lifecycle was supposed to close
beads after the agent exited with code 0. But the parsing of
`exit_code|duration|output_file` from stream-parser.sh failed silently,
orphaning beads as `in_progress`.

**Root cause:** The stream-parser output format was fragile. Any unexpected
output (extra newlines, debug messages, error text) broke the pipe-delimited
parsing. There was no error handling or fallback.

**Workaround:** Changed the architecture so the LLM agent is responsible for
closing its own bead via `br close <id>`. NEEDLE's post-dispatch closure
became a fallback, not the primary mechanism.

## Bug 7: Mitosis Pre-Execution Check on Every Bead (nd-fhtv0t)

**What happened:** Mitosis analysis ran as a pre-execution check on every
claimed bead, not just on failure. This meant the LLM was called twice for
every bead -- once to decide whether to split, and once to actually do the
work. This doubled API costs and added seconds of latency per bead.

**Fix (commits dd82342, 316e49d):** Removed the pre-execution mitosis
check. Mitosis now only triggers on failure -- when a bead is too complex
for the agent, it fails, and mitosis analyzes whether splitting would help.

## Bug 8: Mend Strand Infinite Loop (nd-bt09f0)

**What happened:** The mend strand's orphan release logic returned success
(0) even when the release failed. The worker loop interpreted success as
"work was found," resetting the idle counter and restarting from strand 1.
But mend would fail again the same way, creating an infinite loop of failed
releases that never progressed to other strands.

**Fix (commit e9bc0fd):** Return 1 (failure) when orphan releases fail, so
the strand engine falls through to the next strand.

## Lessons for the Rewrite

### 1. Never trust external tool JSON output

The `br show --json` upstream bug caused cascading failures because the code
assumed labels would be present. Always validate that expected fields exist
in tool output, and use the most specific command available (e.g., `br label
list` instead of parsing a general-purpose JSON dump).

### 2. Mitosis needs hard limits from day one

A depth guard, a session-level loop guard, and a per-bead lock should all
be in place before mitosis is enabled. The exponential explosion risk is
too high to rely on soft guards (labels) alone.

### 3. Build concatenation is inherently fragile

When concatenating bash files into a single binary, top-level `return`
statements, source guards, and variable scoping all behave differently than
in the source tree. The build system needs:
- Automated validation that all expected functions are defined
- Syntax checking of the concatenated output
- Integration tests that run the bundled binary, not just the source tree

### 4. Bead closure should be the agent's responsibility

NEEDLE's post-dispatch lifecycle parsing was too fragile for a critical path.
The agent is in the best position to validate its work and close the bead.
NEEDLE should verify closure happened (and release the claim if not) but not
be the primary closer.

### 5. Dependency cleanup must be automated

Stale dependency links silently block beads from being worked. The system
needs automatic dependency resolution when blocking beads close, not manual
`br dep remove` commands.

### 6. Every strand must distinguish "did work" from "found nothing"

The mend strand returning 0 on failed releases caused an infinite loop.
Return codes must clearly separate success (did useful work), nothing found
(fall through), and failure (error, try something else).

## Source Evidence

- Commit `6dbc151`: prevent recursive splitting of mitosis children
- Commit `a2c266b`: replace hard block with depth guard
- Commits `8e9e706`, `86ccfcd`: read labels via `br label list`
- Commit `1e8fedf`: genesis beads trigger next-phase expansion
- Commit `bc3c9f0`: prevent explosion via reliable lock + workspace context
- Commit `4a35e95`: fix bare return in verify.sh guard
- Commit `e9bc0fd`: mend strand return 1 on failure
- Commits `dd82342`, `316e49d`: remove pre-execution mitosis check
- Cleanup: 606 zombie beads, 155 recursive mitosis children, 5,741 explosion beads
