# ADR-002: Pluck Telemetry Isolation and Fleet Process Tracking

**Status:** Accepted — 2026-07-14
**Deciders:** operator (jedarden)
**Tracking:** plan.md Phase 6; implementation beads in this repo's workspace (genesis `bf-hzblx`)

## Context

Traced from an 8-day incident on `~/ARMOR` (2026-07-06 through 2026-07-14), discovered and root-caused in a separate session on the ARMOR side, filed here as `bf-hzblx`.

**Bug 1 — Pluck writes self-diagnostics into the scanned repo.** `PluckStrand` (`src/strand/pluck.rs`), when it scans a target workspace and finds beads it cannot dispatch, writes a "Starvation alert" **as a bead into that workspace's own `.beads/issues.jsonl`** rather than into NEEDLE's own telemetry. On ARMOR this produced `bf-3b64` ("Starvation alert: beads invisible to worker") at 2026-07-06T12:45:30Z. A dispatched worker did the standard investigate→fix→verify→monitor decomposition on it — reasonable-looking, except the "fix" step (`bf-36co`, "Fix bead discovery configuration") asks a worker scoped to the *target* repo to edit **NEEDLE's own** filter/exclude_labels configuration, which is not part of that repo and is not reachable or editable from inside it. The fix bead stayed permanently blocked.

Because the alert never actually resolved, later sessions kept re-triggering it and re-investigating from scratch — near-duplicate "Locate Pluck configuration files" beads were independently recreated at least 3 times. One investigative thread guessed the unreachable config was YAML-formatted and pivoted from "find the config" to "build a YAML parser to read it," which — lacking any real target file to validate against — snowballed into open-ended, self-justifying busywork: a full YAML/JSON/TOML parser library, then a pytest-output-parsing tool, then hundreds of near-duplicate test-coverage beads (folded-scalar indentation matrices, schema interfaces, scope-stack tracking), continuously, for 8 days. Total damage on ARMOR alone: 346 fabricated beads, ~2,300 combined bead-cycles across two workers, while 5 real, actually-dispatched beads sat untouched the entire time. A parallel instance of the same pattern was found mid-execution on `~/SIGIL` (worker `echo`), not yet cleaned up.

**Bug 2 — fleet process tracking has blind spots.** Remediating the above required stopping the runaway workers, which surfaced a second, independent defect:
- `needle stop -i <session>` reported success and removed the session from `needle list`, but the underlying OS process (the actual `needle run --workspace ...` process and its dispatched `claude` subprocess) kept running and kept dispatching against the target repo. It had to be killed directly by PID (`kill -9`) after cross-referencing `ps aux`.
- A third worker (`alpha`, also pointed at ARMOR, also actively dispatching) was running the entire time and was **completely invisible to both `needle status` and `needle list`** — discovered only via `ps aux | grep 'needle run'`.

Both bugs share a root theme: NEEDLE's own operational state (what it's confused about, what it's running) leaks into or is undiscoverable from the wrong place — the target repo's bead tracker in Bug 1, nowhere-queryable in Bug 2.

## Decision

1. **Redirect Pluck's starvation self-diagnostic to NEEDLE's own telemetry.** Never write it as a bead into the scanned workspace. Emit a structured `pluck.starvation_detected` telemetry event (workspace, open count, excluded count, candidate reasons) through the existing telemetry pipeline (the same one `ExploreStrand`'s Phase-5 starvation alarm uses — see ADR-001 §5.4). If a persistent, actionable record is wanted, file it as a bead in **NEEDLE's own** workspace, never the target's.
2. **Treat "Pluck configuration is unreachable from a target-scoped worker" as a structural constraint, not a solvable task.** A target-repo worker must never be prompted to investigate or fix NEEDLE's own dispatch configuration — that class of work has no legitimate resolution path from inside the target repo and should be filtered out of what gets auto-decomposed there.
3. **`needle stop` must kill the full process tree**, not just detach/remove the tmux registry entry: parent `needle run` process, its `bash -c` prompt wrapper, and the dispatched `claude` subprocess. Verify the PID is actually gone before reporting success, not just that the tmux session no longer lists it.
4. **`needle status`/`needle list` must not have registry blind spots.** Every `needle run` process, however it was started (tmux-wrapped, bare `NEEDLE_INNER=1` background, etc.), must be discoverable through standard fleet commands. Reconcile the process-table view (`ps aux`) against the registry view as a health check, and WARN on any process matching `needle run --workspace` that isn't in the registry.

## Consequences

- Pluck's starvation signal becomes debuggable (NEEDLE operator telemetry) instead of destructively actionable (a bead a worker will try, and fail, to resolve).
- Removes the specific self-perpetuating loop that produced this incident; does not by itself prevent all forms of worker rabbit-holing (that's the broader failure-circuit-breaker problem tracked elsewhere).
- `needle stop` becomes trustworthy for incident response — an operator (or Claude, acting on an operator's behalf) can rely on "Stopped: X" actually meaning the process is gone, without a manual `ps aux` cross-check.
- Slightly more telemetry volume (one starvation event per strand-scan-with-nothing-claimable); bounded the same way ADR-001's starvation alarm is.
- `~/SIGIL`'s parallel instance of Bug 1 is not fixed by this ADR alone — it needs its own cleanup pass once this fix ships, the same way ARMOR did.

## Evidence

- `~/ARMOR` bead `bf-3b64` (created 2026-07-06T12:45:30Z) and its full descendant lineage — 346 beads total, closed 2026-07-14 across three review passes.
- Lab fleet state 2026-07-14: `bravo` (1671 beads) and `echo` (566 beads) on ARMOR/SIGIL respectively, both >99% consumed by beads matching this lineage; the 5 real ARMOR Phase-5 integrity beads (genesis `bf-4l7q`) untouched throughout.
- `needle stop -i needle-claude-code-glm-4_7-bravo` → "Stopped" → `ps aux` on the lab host still showed PID 144019 (`needle run --workspace /home/coding/ARMOR --identifier bravo`) alive and dispatching, 8+ minutes later.
- `needle status`/`needle list` output on the lab host, checked repeatedly during remediation, never listed a worker named `alpha`; `ps aux` showed PID 143883 (`needle run --workspace /home/coding/ARMOR --identifier alpha`), running since 2026-07-11, still dispatching against the same duplicated bead (`bf-4tshoo`) as `bravo`.
