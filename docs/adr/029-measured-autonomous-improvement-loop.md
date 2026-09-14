# ADR-029: Improvements are proposed from ledger evidence and kept only by measured impact

**Status:** Accepted — 2026-09-13 (design and implementation authority for
L1–L4; L5 activation remains gated by Gate D, plan section 9)
**Deciders:** operator (jedarden), NEEDLE maintainers
**Tracking:** plan revision 34, sections 4.9–4.10; `needle-73c1958b`
(META ADR contract test); N-T53–N-T56 beads listed in plan section 8

## Context

Plan revision 33 reserved this number for the decision that would let NEEDLE
change its own harness. Until 2026-09-12 that question was hypothetical: no
durable record said what an attempt was, what it cost, or whether it worked.
Between 2026-09-12 and 2026-09-14 the section 4.4 order was executed and
deployed to the codinghome fleet (plan section 4.9). The ledger now records
every dispatch with adapter, model, provider, gate results, semantic outcome,
tokens and estimated cost; attempt history and prior fixes reach retry
prompts; adapter storms resolve as infrastructure; evidence routing and prompt
canaries run as L1/L2 adaptations with receipts.

Two facts from that deployment shape this decision.

First, every improvement that produced a measurable gain was a short wire on
data that already existed, and every regression was caught by the same ledger
that motivated the wire: Python and Node default gates failed 25 times in a
clean extraction before the ledger showed a single fingerprint across two
workspaces; evidence routing moved 75 beads off the strongest adapter because
an adapter with no evidence scored zero rather than unknown. The ledger is a
sufficient sensor for both proposing and rejecting changes.

Second, the operator has stated the objective (2026-09-13): improvements
should be produced without human curation, and they must lead to legitimately
more impact and productivity. "Legitimately" rules out the failure mode the
plan already documents (section 10): tool calls, commits, generated beads,
reflection documents and an agent's own report of success are not impact.
Curation-free therefore cannot mean unmeasured; it means the measurement,
not a person, decides what stays.

The existing plan already owns the parts: N-T07 defines a typed proposal
envelope and an admission controller; N-T12 defines experiments, promotion
and an audited reversible delivery controller; section 5.7 defines authority
levels L0–L5; ADR-026 forbids unevaluated lessons from becoming policy;
ADR-027 forbids any automatic change from widening authority or weakening a
gate. What is missing is the loop that connects them to the ledger and closes
on impact.

## Decision

NEEDLE runs an improvement loop whose every stage is evidence-addressed and
whose only promotion criterion is measured impact against a cohort baseline.

1. **Propose from the ledger, not from prose.** A pure generator (N-T53)
   reads the attempt ledger and mature outcomes over a declared window and
   emits `ImprovementProposal` records in the N-T07 envelope. A proposal
   names its evidence class and instances (attempt IDs, fingerprints,
   workspaces, adapters), the exact intended change, the authority level
   it requires, the expected benefit in the section 10 measures, the
   acceptance measure and horizon, and the rollback. Recognised evidence
   classes at acceptance: repeated identical failures, per-workspace adapter
   regret, unverified spend concentration, gate-broken or red-baseline
   workspaces, recurring fingerprints with a known fix, and canary results.
   A proposal whose acceptance measure cannot be computed from the ledger is
   malformed and never reaches admission.

2. **Admit through the ordinary controller.** N-T54 submits proposals to the
   N-T07 admission controller, which deduplicates by evidence signature
   against existing beads, applies the generated-work budget, and records
   an explicit admit/refuse decision. L1–L3 proposals are applied by the
   controller that owns the envelope (routing, experiments, numeric bounds)
   and need no bead. L4 proposals become ordinary implementation beads in
   the owning repository, with provenance references to the proposal and its
   evidence, worked by the fleet through the normal factory path: shared
   checkout, precise staging, Definition of Done, needle-ci, canary,
   `needle upgrade`. Cross-repository proposals create the bead in the
   owning repository and a `cross-repo-gate` bead here.

3. **Measure, then keep or withdraw.** N-T55 attaches an impact receipt to
   every admitted proposal: the cohort it affected, the baseline before
   exposure, the measures over the declared horizon (verified-closure yield
   per attempt and per dollar, target-fingerprint recurrence, false-close and
   reopen rate, infrastructure share), and a promote or withdraw decision.
   A change that does not move its acceptance measure against the baseline
   is withdrawn regardless of who or what implemented it and regardless of
   the implementing agent's report. Withdrawal is itself a proposal: a revert
   through the same admission and delivery path, never a direct edit.

4. **Authority does not widen inside the loop.** Section 5.7 and ADR-027
   stand. No proposal can weaken a gate, raise a budget, change an authority
   level, or edit this ADR, the plan, or repository policy files. Proposals
   about the loop itself, including harness and model pairing changes, are
   the META epic (`needle-31ed0db2`) and remain held until Gate D evidence
   exists. L5 (deploying code or policy without a human release) requires
   the audited delivery controller (`needle-99e8c972`) and Gate D.

5. **The operator reads receipts; the operator does not curate.** The human
   role reduces to: accepting Gate D, widening authority, and answering the
   escalations the guardrails raise (budget exhausted, conflicting evidence,
   a withdrawal that cannot revert cleanly). Everything else is a receipt
   the operator may read, and `needle improvements` (N-T56) is the read-only
   view of proposals, admitted beads, receipts and the fleet trend.

6. **Honest inputs first.** The loop cannot run on a ledger that counts a
   decomposition as a verified success, books a killed attempt at zero cost,
   or penalises an agent for a red baseline. ADR-030 fixes those
   classifications and precedes N-T53 in the dependency graph.

## Consequences

### Positive

- Improvement work is generated from the same evidence that later judges it,
  so the loop cannot reward itself for activity.
- Regressions are withdrawn by measurement within one horizon, which is the
  same mechanism that caught the two 2026-09-12 regressions by hand.
- The operator's time moves from choosing work to reading receipts, which is
  the productivity gain the operator asked for and the plan measures
  (accepted deliverables per human hour).
- Every generated bead carries provenance to attempts, so a later reader can
  see why the factory chose to change itself.

### Negative

- Impact takes a horizon to measure; the loop is slower than an agent's
  self-report, by design. Budgets start small (one admitted proposal per day)
  and rise only on two consecutive net-positive horizons.
- A cohort baseline can be contaminated by concurrent operator work; receipts
  must name the cohort and exposure, and adaptation freezes when the cohort
  is contaminated (Gate C).
- Proposal generation adds a read-only consumer of the ledger and mature
  outcomes; it is budgeted and asynchronous like reflection (section 4.8.6).

## Alternatives considered

- **Let the meta-agent harness propose and apply changes directly**
  (the revision-33 epic as the first loop): rejected for now. It is the L5
  end of this loop, and it has no evidence path until proposals, admission
  and receipts exist. It becomes one proposal class once Gate D holds.
- **Operator curates a ranked list produced by an agent** (what happened on
  2026-09-13): rejected as the steady state. It produced good work once but
  the operator has said it does not scale, and a curated list carries no
  receipt that the chosen item helped.
- **Promote on the implementing agent's verification alone**: rejected.
  The ledger shows 44% of spend on attempts that verified nothing and six
  false closes in eleven hours; the agent's report is exactly the signal the
  plan says is not impact.
- **Unbounded autonomy with a kill switch**: rejected. A kill switch is a
  rollback of the loop, not of the change; every change needs its own
  receipt-backed revert path.

## Implementation and verification

1. ADR-030 classifications land first: `decomposed` resolution class,
   killed-attempt cost, red-baseline classification, workspace-scoped
   evidence (N-T46–N-T49).
2. N-T53 generator runs in shadow: proposals are recorded and visible in
   `needle improvements`, none admitted. Acceptance: a replay of the
   2026-09-12..14 ledger produces the unchanged-retry, workspace-regret and
   unverified-spend proposals with the numbers section 4.9 reports.
3. N-T54 admission at budget one per day, L4 only, dedup proven against the
   existing graph (a proposal for R1/R2's failure class must resolve to
   `needle-d6c5397a`/`needle-7b9718bc`, not a new bead).
4. N-T55 receipts on the first admitted proposals; the first withdrawal
   proves the revert path end to end.
5. Budget rises only after two consecutive horizons with net-positive
   receipts; `needle-73c1958b` encodes the authority contract as a test.
6. The META epic's first leaf may begin once steps 2–4 have live receipts.

## Related

- Plan sections 4.4, 4.8.5, 4.9, 4.10, 5.7, 9 (Gate D), 10.
- ADR-024 (attempt/evidence/resolution), ADR-026 (evidence-gated lessons),
  ADR-027 (policy authority), ADR-030 (complete attempt accounting).
- N-T07 leaves `needle-2b0a309b`, `needle-43c0d818`, `needle-f754b4cb`;
  N-T12 delivery controller `needle-99e8c972`; META `needle-31ed0db2`.
