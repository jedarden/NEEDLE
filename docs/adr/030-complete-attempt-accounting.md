# ADR-030: Attempt accounting is complete: decomposition, killed attempts and red baselines are classified, never credited

**Status:** Accepted — 2026-09-13
**Deciders:** operator (jedarden), NEEDLE maintainers
**Tracking:** plan revision 34, section 4.9; N-T46, N-T47, N-T48, N-T49,
N-T52 beads listed in plan section 8

## Context

The `attempt.resolved` ledger (N-T16) shipped on 2026-09-12 and by 2026-09-14
held 852 non-fixture attempts. Reading it for the first time exposed four
places where the accounting is structurally wrong rather than merely noisy,
and one place where it is contaminated.

- **A decomposition is booked as a verified success.** Verified-closure yield
  is 54% on first attempts and 46% on second attempts, then 86% on fourth and
  97% on fifth-or-later attempts. The jump is the auto-split template: when
  a bead is split into children, the parent attempt closes and the ledger
  records `verified_success`. Competence, routing and any future proposal
  generator would learn that splitting is winning.
- **A killed attempt costs nothing.** 124 of 852 attempts (15%) timed out and
  every one carries `estimated_cost_usd: 0`, because cost is read from the
  final result envelope that a killed process never writes. The 44% of spend
  recorded on unverified attempts is therefore a floor, and a per-attempt
  spend cap cannot exist without a running total.
- **A red baseline is charged to the agent.** Some `default_rust` gate
  failures are repositories whose HEAD already fails `cargo check`. The
  attempt is recorded as a task failure, the bead accrues a failure count,
  and routing evidence records the adapter as having failed.
- **Evidence is fleet-scoped when the signal is workspace-scoped.** In
  reddit-media-player codex verified 33 of 34 attempts while flash verified
  9 of 24; in pdftract flash verified 11 of 15 while glm-5.3 verified 4 of
  48. A fleet-wide choice averages this away, and a workspace where every
  adapter does badly is a workspace signal that the average hides.
- **Fixture rows are in the live ledger.** Rows with worker
  `echo-test-test-worker` and workspace `.` appear in `~/.needle/logs`
  because tests resolve state directories from the real home. Provider-health
  and gate-health fixtures were found under the live state directory during
  the 2026-09-13 deployment.

ADR-023 already established that a gate which cannot execute is
infrastructure, not bead failure. ADR-024 established the attempt as the unit
of work and its resolution as the unit of truth. This ADR extends the same
principle to the four classifications above.

## Decision

1. **`decomposed` is a resolution class, not a success.** An attempt whose
   requested action is a split, or whose closure is accompanied by created
   child beads and no delivered artifact, resolves as `decomposed`. It earns
   no verified credit in stats, routing evidence, experiments, competence or
   proposal generation; it is reported on its own line. Delivered work on the
   children earns credit on the children's attempts.
2. **Every dispatched attempt is charged.** Tokens and estimated cost are
   accumulated from the adapter's per-turn usage events as they stream, so
   a timed-out, crashed or interrupted attempt carries the usage it consumed.
   The final envelope, when present, reconciles the total; it is not the only
   source. An adapter that reports no usage yields `costed: false`, never a
   silent zero. A per-attempt spend cap (`limits.attempt.max_cost_usd`)
   becomes possible and stops a dispatch with resolution `budget_exhausted`,
   which is a task-neutral infrastructure-class outcome.
3. **A red baseline is a workspace condition.** Before dispatch, the declared
   or default gates run once against the pre-dispatch revision, cached per
   revision. A post-attempt gate failure whose normalized fingerprint matches
   the baseline failure resolves as `workspace_red`: infrastructure-class per
   ADR-023, no failure count, no adapter penalty, adaptation frozen for the
   workspace, and one infrastructure bead filed through the existing
   gate-health path. A gate that passed at baseline and fails after the
   attempt remains task evidence.
4. **Routing evidence is keyed by workspace first.** Evidence routing reads
   `(workspace, adapter)` evidence with the existing floor; below the floor it
   falls back to fleet `(adapter)` evidence, and below that to the static
   default, recording which scope decided. A workspace whose every candidate
   is below the verified-yield threshold emits a workspace-level signal rather
   than a routing change.
5. **Fixture rows never enter the live ledger.** Every state writer (ledger
   logs, gate health, provider health, experiments, attempt journals, spool)
   resolves its root from one configured state directory that the test
   harness overrides; a test that spawns NEEDLE without an isolated state
   directory fails. Rows already in the live ledger from fixtures are
   excluded by worker/workspace identity in every consumer until the files
   age out.

## Consequences

### Positive

- Verified-closure yield, cost per verified closure and unverified spend
  become honest enough to drive ADR-029's proposals and receipts.
- Retry, routing and competence stop learning that splitting or a red main
  is the agent's doing.
- A spend cap gives the fleet its first cost guardrail that acts before an
  attempt ends in nothing.

### Negative

- Reported verified yield will drop when decomposition stops counting; the
  drop is a correction, not a regression, and section 4.9 records the
  pre-correction numbers so the two are not confused.
- The baseline gate run costs one gate execution per new revision per
  workspace; it is cached and skipped for `gates: []` workspaces.
- Streaming usage accounting must tolerate adapters whose per-turn events
  are absent (codex, opencode, omp today); those attempts stay `costed:
  false` until N-T51 lands.

## Alternatives considered

- **Exclude splits in the consumers instead of the ledger**: rejected. Every
  consumer would re-derive the rule and any new consumer would get it wrong;
  the resolution class is the contract.
- **Estimate killed-attempt cost from duration**: rejected. A price per second
  is a fabricated number; per-turn usage is the adapter's own report.
- **Treat red baselines as gate-degraded workspaces only**: insufficient. The
  fingerprint detector needs five failures; the baseline run classifies the
  first one and never charges it to an agent.
- **Per-workspace routing without a fleet fallback**: rejected. Most
  workspaces are below the evidence floor for most adapters; the fallback is
  what keeps routing from reverting to static rules everywhere.

## Implementation and verification

1. N-T46 `decomposed` resolution class with stats, routing and experiment
   exclusion; fixture proves a split parent earns no verified credit.
2. N-T47 streaming usage accounting and `costed` flag; fixture replays a
   killed transcript and asserts nonzero cost; per-attempt spend cap.
3. N-T49 baseline gate run and `workspace_red` classification; fixture with
   a red HEAD proves no failure count, no adapter penalty, one infra bead.
4. N-T48 workspace-scoped evidence with recorded deciding scope; replay of the
   2026-09-12..14 ledger proves reddit-media-player and pdftract route to
   their best adapter without moving LOOM.
5. N-T52 env-scoped state directory; the fixture suite runs with the live
   home mounted read-only and writes nothing outside the override.

## Related

- ADR-023 (gate execution errors are infrastructure), ADR-024, ADR-029.
- Plan sections 4.4, 4.9, 8 (N-T46–N-T49, N-T52), 10.
- Existing owners this ADR does not replace: Mitosis attribution gating
  `needle-0be52050`, split-parent reconciliation `needle-1c1974d7`,
  HOME-isolation umbrella `needle-595e2622`.
