# ADR-004: Recursive Workspace Discovery as Explore's Default, Static List as Pinning Exception Only

**Status:** Accepted — 2026-07-20
**Deciders:** operator (jedarden), via Claude Code
**Tracking:** plan.md Phase 8; implementation beads in this repo's workspace (genesis TBD — see plan.md Phase 8)

## Context

Found while investigating why six freshly-launched roaming workers processed zero beads over roughly two hours on 2026-07-19 (see `bf-4df1e`, "Explore strand stops scanning at the first workspace with any candidates"). That investigation explained why Explore stalls once it reaches a workspace with a false-positive candidate — but the operator identified a second, independent problem the same evidence pointed at: Explore's list of workspaces to scan was wrong to begin with.

`ExploreStrand::new()` (`src/strand/explore.rs`) already contains the correct, working design, stated verbatim in its own doc comment:

> "The workspace list is captured at construction time and never re-read. If `workspaces` is empty, auto-discovers all dirs with `.beads/` under the configured `workspace_root`."

`discover_workspaces(root)` does exactly what the operator confirmed was the intended behavior: recursively scan the parent workspace root (`/home/coding`) and treat every child directory containing a `.beads/` subdirectory as a workspace. This is real, tested, working code (`discover_workspaces_finds_dirs_with_beads_subdir` in the same file's test module).

The problem is that `explore.workspaces` in the live lab config is **not empty** — it's a hardcoded list of 24 specific repo paths (`/home/coding/miroir`, `/home/coding/HOOP`, `/home/coding/SIGIL`, ...). Because `ExploreStrand::new()`'s branch is `if config.workspaces.is_empty() { discover } else { use the static list }`, this non-empty list unconditionally short-circuits `discover_workspaces()` for every worker — the recursive-scan code path is entirely dead in the running fleet, not because it's broken, but because the config never lets it execute.

Confirmed live: two repos with genuine `.beads/` directories — `commitgraph` and `twitterapi-proxy` — are **not** in the static list. They are therefore permanently invisible to every roaming worker's Explore strand, independent of and in addition to the `bf-4df1e` early-return bug. The static list reads as an ad hoc enumeration of "whatever repos existed when someone last edited this config," not a deliberate, curated exception set — it has already drifted out of sync with the actual filesystem once, and will keep drifting every time a new repo is created under `/home/coding`.

The operator's stated intent: the `workspaces` config field's real purpose is to let an operator **pin** a specific worker to a fixed, restricted set of repositories for a deliberate reason (e.g., a dedicated worker that must never touch anything outside 2-3 sensitive repos) — an exception, used sparingly, not the default or expected way to configure the fleet's scan scope. Recursive discovery under `workspace_root` should be what every worker gets unless an operator has deliberately opted a specific worker out of it.

## Decision

1. **Recursive discovery is the default an operator has to deliberately opt out of, not one they accidentally fall into.** `ExploreStrand::new()`'s existing empty-list-triggers-discovery logic is structurally correct and does not need to change — the fix is operational and documentational: `config.workspaces` must not be populated with a general-purpose "all known repos" enumeration. It should stay empty for the fleet as a whole, letting `discover_workspaces()` run for every worker, by default.
2. **`config.workspaces` remains fully configurable**, but is re-scoped in documentation and operational practice to mean "pin this specific worker to exactly this list" — a deliberate, per-worker exception, not fleet-wide baseline config. Nothing in the code needs to change to support this; the existing branch already implements exactly this contract once the config is used as intended.
3. **Immediate operational fix**: clear the live lab config's `explore.workspaces` back to empty. None of its current 24 entries represent a deliberate pin — restoring the default recursive-discovery path immediately makes `commitgraph`, `twitterapi-proxy`, and any future new repo visible without further config maintenance.
4. **Open question, not resolved here**: discovery is captured once at worker construction and never re-read during that worker's lifetime, so a repo created after a long-lived worker starts still won't be picked up without a restart. Left as a follow-up decision for Phase 8.3 rather than bundled into this ADR.

## Consequences

- Every repo under `/home/coding` with a `.beads/` directory becomes reachable by roaming workers without manual config-list maintenance, present and future.
- Combined with `bf-4df1e` landing, this fully resolves the root cause of the 2026-07-19 zero-throughput incident — `bf-4df1e` alone would still leave `commitgraph`/`twitterapi-proxy` permanently unscanned even after Explore stops giving up early.
- The pin/exception mechanism keeps working exactly as before for anyone who deliberately wants a restricted worker — this ADR does not remove or weaken that capability, only stops it from being (mis)used as the default.
- No source code change is strictly required to realize the default behavior — the existing branch already does the right thing when the list is empty. The only mandatory action is clearing the live config; the code-level acceptance criteria in Phase 8.1/8.4 are about making this contract explicit and regression-tested so it can't silently drift back to a stale static list again.

## Evidence

- `src/strand/explore.rs`, `ExploreStrand::new()` doc comment and branch logic (quoted verbatim above) — confirms empty-list-triggers-discovery is real, existing, working code.
- `src/strand/explore.rs`, `discover_workspaces()` — confirmed via direct read: recursively lists `workspace_root`'s children, checks each for a `.beads/` subdirectory, exactly matching the operator's described intended design.
- Live config dump (`needle config`) on the lab host, 2026-07-19/20: `explore.workspaces` populated with 24 explicit paths.
- Confirmed via direct filesystem check on the lab host: `~/commitgraph/.beads` and `~/twitterapi-proxy/.beads` both exist, neither path appears in the static `explore.workspaces` list.
- Related, compounding defect already filed: `bf-4df1e` (Explore returns as soon as it finds any workspace with a non-empty candidate list, never reaching later workspaces in the same cycle) — that bug determines what happens once a workspace IS reachable; this ADR determines which workspaces are reachable at all.
