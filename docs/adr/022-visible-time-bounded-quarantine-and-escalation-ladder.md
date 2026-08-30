# ADR-022: Quarantine Is Visible, Time-Bounded, and Escalates — the Human Is the Last Rung

**Status:** Accepted — 2026-08-29
**Deciders:** operator (jedarden), via Claude Code
**Supersedes:** ADR-012's quarantine *mechanism* (`BeadStore::block` → `bead update --status blocked`). ADR-012's circuit-breaker threshold and failure-aware Pluck ordering stand.
**Tracking:** Phase 19 (§19.2, §19.3, §19.7); beads under label `phase-19`

## Context

ADR-012 (2026-07-30) stopped runaway redispatch by quarantining a bead after `quarantine_after_failures` consecutive failures. The mechanism chosen was `store.block()`, realised on bead-rs as `bead update <id> --status blocked`. On bead-rs that command does not change `base_status`; it sets a `manual_blocked` overlay that `bead list --json`, `bead show`, and `bead doctor` all omit — the bead prints as an ordinary `open` bead and only `bead why --id` reveals the flag.

Audit on 2026-08-29 (ex44, 62 bead-rs workspaces): **171 open beads** carried `manual_blocked` — 138 quarantined by NEEDLE this week, 31 inherited from the bf migration — including `needle-3386daef` (P0) and `needle-44e7e5cd`. Ten workspaces had open work and a ready frontier of zero; warden and sun-sim were 2-of-2 blocked with no visible cause anywhere in the normal tooling. Ninety-one such roots pinned **2,129** dependency-blocked beads. Nothing ever unquarantined a bead; nothing reported the state; the only exit was a human running `bead update --status open` after reading source.

Two properties of ADR-012's mechanism caused this, independent of the (separately fixed) reason so many beads hit the threshold:

1. **Invisibility.** The quarantine state lived in a field the store's own tools do not surface. A safety mechanism whose activation cannot be seen is indistinguishable from data loss.
2. **Permanence.** Quarantine had no expiry and no next step. A bead quarantined for a transient cause (a red tree, a misconfigured gate, a provider outage) stayed quarantined after the cause cleared — until a human noticed.

Principle 7 (added 2026-08-29): the human is the absolute last resort. A mechanism whose only exit is a human violates it by construction.

## Decision

1. **Quarantine is expressed as labels on an `open` bead**, never as a status or overlay: `quarantined`, `quarantine-until:<rfc3339>`, `quarantine-round:N` (plus the existing `cycling`). The backend descriptor's `block` operation is deleted; no NEEDLE code path may issue `--status blocked`.
2. **Quarantine expires.** Pluck skips a bead only while `quarantine-until` is in the future. Backoff per round: 2h, 4h, 8h (cap 48h). `failure-count` survives the round.
3. **Quarantine escalates, and the ladder is fixed:** retry → decompose (Mitosis) → quarantine rounds 1–3 → one plan-grounded analysis dispatch → `human`. Rung 4 must leave an `analysis:` note and either a re-scoped child bead or an explicit statement of what the plan does not answer. A `human` label without that note is a defect.
4. **Roots are ranked by what they pin.** Pluck's effective priority inherits from transitive open dependents, and `needle status`/`doctor` report blocked-tree size and the top roots per workspace.
5. **Existing `manual_blocked` beads are migrated automatically** by Mend under the first binary carrying this ADR; no hand sweep.

## Alternatives considered

- **Keep `--status blocked` and teach bead-rs to display it.** Rejected as the primary fix: display would remove invisibility but not permanence; the flag still has no expiry and no next rung. bead-rs *should* surface `manual_blocked` (tracked separately) so that a human-set block is visible, but NEEDLE will not set it.
- **Use bead-rs `deferred` base status for quarantine.** Rejected: `deferred` is the documented meaning of "deliberately postponed by a person" (Phase 13.4) and is what 19.4's over-budget and 19.7's stale triage use for reversible parking. Overloading it would make an automatic backoff indistinguishable from an operator's decision.
- **Auto-split instead of quarantine at the threshold.** Still rejected, as in ADR-012: Mitosis already had its turn at `split_after_failures`; splitting a bead that failed five times manufactures five more.
- **Never escalate to a human; loop rung 4 forever.** Rejected: a bead the plan genuinely does not cover must be visible as such, and Principle 8 says the fix is plan text, which is a human act. The point is to reach that rung rarely and with a written reason, not never.

## Consequences

- `bead list --ready` and `needle status` agree on what is claimable; a quarantined bead is greppable by label in every tool.
- A transient cause clears itself inside a working day; a persistent one reaches an analysis dispatch within ~14 hours of wall clock and a human only with a written case.
- Failure counts are preserved across rounds, which depends on fixing the `reset_failure_count` ordering bug (needle-b39fe1b6) first.
- The fleet-wide "beads at the human rung" count becomes a first-class health metric with a target of zero.
