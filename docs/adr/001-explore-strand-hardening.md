# ADR-001: Explore Strand Hardening

**Status:** Accepted — 2026-07-13
**Deciders:** operator (jedarden)
**Tracking:** plan.md Phase 5; implementation beads in this repo's workspace

## Context

On 2026-07-11, 24 ready beads were created across all 24 lab bead workspaces (one "hygiene sweep" bead each) and 4 roaming workers (`--workspace /home/coding`) were available to process them. Observed throughput: **one bead per ~40 minutes**, with three separate interventions required (priority bump, stale-assignee clear, worker restarts). Pinned workers (`claim_auto` on a home workspace) processed hundreds of beads on the same host during the same window.

Root causes, verified in code and worker logs:

1. **Return-on-first-candidates deadlock.** `ExploreStrand::evaluate` (src/strand/explore.rs) iterates a static workspace list in filesystem-discovery order and returns at the first workspace with ready candidates. Worker-level exclusions (race-lost TTL 30s) and unclaimable assignees are applied *after* the strand returns. A workspace whose candidates are all unclaimable therefore satisfies "has candidates" every cycle, the worker nets zero, sleeps the idle backoff (900s observed), and never advances to workspace #2. One poisoned store starves the entire estate.
2. **Thundering herd.** Every worker walks the same list in the same order — all roamers converge on the same hot store, race for the same bead, and the losers idle.
3. **Store-layer limit bugs.** The `br ready --json` invocation passes no `--limit` (bead-forge's default limit truncates priority-sorted output — low-priority beads become invisible in busy stores) and another path passes `--limit 0`, which returns an empty set on deployed bead-forge 0.2.0. This produced both "P3 beads invisible" and the persistent `bf list failed` log noise.
4. **Stale assignees are permanent.** `candidates.retain(|b| b.assignee.is_none())` is correct as a claim guard, but nothing ever *releases* a stale assignee on an **open** bead (cross-workspace mend only handles orphaned in-progress beads). A reopened bead became permanently unclaimable by every worker. **Note:** As of 2026-08-24 (ADR-018), `bead reopen` clears the assignee, fixing this specific failure mode. Mend's assignee healing remains valuable for other cases where beads become stuck with stale assignees.
5. **Claim errors masquerade as races.** CLI claim failures collapse into `claimed_by=(race)`, so version-skew or store corruption presents as endless quiet race losses instead of a loud error.
6. **Boot-only discovery.** The workspace list is captured at construction and never refreshed (a deliberate v1 constraint), so newly created stores require worker restarts.

## Decision

Harden the explore strand along six axes (plan.md Phase 5):

1. **Claimable-aware filtering** — pass worker exclusion state into the strand's `Filters` so scan advancement is driven by *claimable-by-me* candidates, eliminating the deadlock class.
2. **Per-worker scan rotation** — start iteration at `hash(qualified_id) % N` to de-herd workers across stores.
3. **Explicit store limits + version handshake** — never rely on CLI default or zero limits; WARN at boot on known-bad bead-forge versions.
4. **Mend heals open+assigned beads** — clear assignees whose workers have no live heartbeat. (The root fix — `reopen` clears assignee — belongs in bead-forge; NEEDLE stays defensive regardless.)
5. **Claim-error taxonomy** — distinguish error from race; escalate repeated errors via telemetry instead of silent cycling.
6. **Event-driven cadence + periodic re-discovery + starvation telemetry** — inotify/mtime wakeups on `issues.jsonl` with a jittered 60–120s floor; found-but-excluded triggers short retry, never idle backoff; re-run workspace discovery periodically; emit per-cycle scan summaries and a "ready beads exist but nothing claimed for X minutes" alarm.

Kept from v1 by design: no upward traversal; workers return home after one remote bead; explicit `workspaces` config overrides discovery.

## Consequences

- Roam mode becomes a dependable dispatch path; fleet-wide sweeps (one bead in each of N repos) stop requiring per-repo pinned workers and manual babysitting.
- Slightly more store traffic (shorter polling floor, re-discovery) — bounded by jitter and mtime pre-checks, and far cheaper than the operator interventions it replaces.
- New telemetry events extend the existing schema (additive; JSONL/OTLP consumers unaffected).
- The starvation alarm gives FABRIC/HOOP a signal for the fleet-immune-system layer identified in the 2026-07 corpus audit.
- As of 2026-08-24 (ADR-018), `bead reopen` clears the assignee, fixing the core failure mode that motivated defensive assignee healing. The `--limit 0` fix remains pending in bead-forge. NEEDLE's defensive workarounds (assignee healing, explicit limits) stay in place for robustness and to handle other cases where beads become stuck with stale assignees.

## Evidence

- Lab worker logs 2026-07-11 20:51–21:22Z: `strand found candidates strand=explore candidates=2 excluded=2` repeating against a single workspace; `claim race lost ... claimed_by=(race)`; 900s idle loops.
- Throughput comparison, same host, same hour: roamers 1 bead/40 min; pinned workers ~40 beads/hour.
- bead-forge 0.2.0 `--limit 0` returns empty (known bug, fixed at HEAD, unreleased at time of writing).
