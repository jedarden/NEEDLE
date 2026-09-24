# NEEDLE-Internal Artifact Classification

One shared answer to one question: **was this bead produced by NEEDLE's own
control plane rather than by a human or a target-repository workflow?**

- Predicate: [`crate::internal::is_internal_artifact`](`src/internal.rs`)
- Consumers: Unravel, Analyze, Mitosis, Pluck, Explore, and the low-water
  generator gate — every current or future work generator

Internal artifacts (starvation and Knot alerts, gate-broken alerts, crash
reports, pulse findings, generation-ratio alerts, splice diagnostic beads,
Unravel proposals) describe the scheduler, not the target repository. They
have no legitimate resolution path inside any target repository, so no
generator may use them as source material: no prompt may be built from them,
and no child bead may be created from them.

## Why the boundary exists

Control-plane artifacts used to land in bead stores as ordinary beads
(bf-3b64: "Starvation alert: beads invisible to worker", empty workspace
column, no fingerprint label). A diagnostic bead that also carries the
`human` label is exactly what Unravel selects, so the scheduler was fed its
own output: it dispatched prompts against the alert and generated child beads
from it — recursive work generation with no resolution path. The same shape
reaches Mitosis and the analysis ladder through the failure/quarantine
machinery.

## The three layers

Classification matches on three layers, strongest first. The layers exist so
the boundary survives the ways a store loses metadata over time, and so
historical pre-classification beads stay quarantined:

1. **Labels** (authoritative, structured). Any reserved internal label —
   `alert`, `crash`, `starvation`, `knot-starvation`, `pluck-starvation`,
   `gate-broken`, `generation-ratio`, `pulse-finding`, `unravel-proposal`,
   `worker-failure`, `worker-loop`, `infra` — or any label starting with
   `fingerprint:` (written by `crate::fingerprint::build_alert_labels` on
   every fingerprinted alert).
2. **Body markers** (structural). Exact section headers NEEDLE writes into
   generated bodies (`## Agent Crash Report`, `## Gate Execution Error`,
   `## Worker Failure`, `## Live Worker Loop Detected`, `## Scanner Finding`,
   the Unravel alternative-child marker). Matched against the body only.
3. **Content patterns** (historical fallback). The title+body patterns from
   the bf-3b64 lineage ("starvation alert", "beads invisible to worker", …).
   Matched against title AND body combined — **never the title alone**.
   Deliberately conservative; do not broaden this layer for shapes that can
   carry a label instead.

An internal label wins even when the bead also carries the `human` label.
That combination is the shape that historically defeated a label-only filter.

## Producer contract

**Every generator that writes beads into a store must mark its artifacts with
a reserved internal label** (or a `fingerprint:` label) and, where the shape
is new, add the label to `INTERNAL_LABELS` in `src/internal.rs`. A generator
that labels its output needs no other change to be quarantined — the label
layer is what keeps the boundary cheap and future-proof.

## Consumer contract

**Every generator must consult the shared classifier before building a prompt
from a selected bead or creating a child bead from it.** Do not reimplement
the check locally: a private pattern list narrows over time and silently
re-opens the boundary for every other generator. Current consultation points:

| Generator | Where the gate runs |
|---|---|
| Unravel | `UnravelStrand::filter_human_beads` — internal beads are excluded from selection even when `human`-labeled, and each skip is counted in telemetry |
| Mitosis | `MitosisEvaluator::evaluate` and the timeout-mitosis path return `OutOfScope`; `would_release_as_split_out_of_scope` on the release path |
| Pluck | candidate gate in `src/strand/pluck.rs` |
| Analyze | `AnalyzeStrand::select_candidates` — an internal artifact is never a rung-4 re-scope candidate, whatever its quarantine history |
| Explore | `ExploreStrand::admit_candidates` — remote internal artifacts are excluded before ranking or claim |
| Generator gate | `GeneratorGate::count_eligible_ready` — diagnostics do not make a low-water frontier look healthy |

Weave does not consult the classifier because it never selects store beads as
source material — its agent proposes new work and existing beads are used only
for dedup. A future generator that reads store beads into a prompt must add
itself to this table.

All of these route through `crate::mitosis::detects_needle_internal_config`
or `crate::internal` directly, so extending `INTERNAL_LABELS`, the body
markers, or the content patterns extends the boundary everywhere at once.

## Telemetry

Unravel emits `bead.unravel.skipped` with `reason: "internal_artifact"` for
every internal artifact it finds, and dispatches nothing — no prompt is built
and no agent is invoked. Other generators keep their existing out-of-scope
events (`MitosisOutOfScope`, …).

## The reserved-label trade-off

`INTERNAL_LABELS` are reserved. A human who labels ordinary work with one of
them (e.g. `alert` on an alerting *feature*) puts that bead behind the
boundary. That false positive is recoverable — the bead keeps its assignee,
and an operator can relabel it — while a false negative (an unlabeled
artifact feeding the generator loop) recreates the recursive generation the
classifier exists to stop. Document the reservation wherever labels are
documented to users.

## Regression coverage

- Generator regression fixtures cover the reserved-label and body-marker
  boundary, ordinary human eligibility, title-only non-matches, and the
  Mitosis delegate agreement.
- `src/strand/unravel.rs` regression fixtures include the verbatim bf-3b64
  starvation alert (`historical_starvation_alert`) proves no prompt is built
  and no children are created, and a `human`-labeled gate-broken alert beside
  ordinary work proves the quarantine is per-bead.
