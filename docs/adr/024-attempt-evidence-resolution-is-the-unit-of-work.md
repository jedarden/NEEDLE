# ADR-024: Attempt, Evidence, and Resolution Are the Unit of Factory Work

**Status:** Accepted — 2026-08-31

**Decision-makers:** NEEDLE maintainers and software-factory operators

## Context

NEEDLE currently correlates most execution and telemetry by bead ID and treats
the agent process exit code as the first outcome classification. A bead may be
dispatched repeatedly, however, and process exit 0 proves only that the agent
process exited normally. It does not prove that the requested artifact exists,
that verification passed, or that the bead reached the intended durable state.

This ambiguity has allowed multiple attempts to be counted as multiple pieces
of completed work, completion telemetry to precede authoritative closure, and
reflection to reinforce observations without knowing which attempt produced
them. It also makes retries and external side effects difficult to make
idempotent.

## Decision

NEEDLE will model every dispatch as an immutable, uniquely identified
**Attempt**. An attempt consumes a **ContextManifest**, produces an
**ExecutionTrace** and an **EvidenceBundle**, and ends in exactly one durable
**Resolution**.

The semantic relationship is:

```text
Bead -> Attempt -> ContextManifest -> ExecutionTrace
                              \----> EvidenceBundle -> Resolution
```

The agent process exit is an observation on the attempt, not its resolution.
`bead.completed` may be emitted only after the configured verifier accepts the
evidence and the bead store confirms the bead is closed. Retrying or releasing
an attempt never increments a durable-completion metric.

Every attempt must carry:

- a globally unique `attempt_id` generated before claim or dispatch;
- bead ID, workspace identity, starting bead revision, assignee, and any lease
  fencing token;
- worker, adapter, harness, model, prompt-template, policy, configuration, and
  memory-manifest identities or hashes;
- start/end timestamps and observable process result;
- evidence references and external-effect idempotency receipts;
- one semantic outcome class and one verified resulting bead state.

Resolution classes distinguish at minimum verified success, bead-scoped
failure, infrastructure failure, interruption/cancellation, stale ownership,
and unresolved/indeterminate. Unknown or incomplete evidence fails quiet: the
bead is safely released or quarantined, never credited as complete.

## Consequences

### Benefits

- Completion, retry, and learning metrics refer to durable facts.
- Repeated dispatches of one bead remain distinct and deduplicable.
- Evidence and policy provenance can be reproduced and audited.
- Infrastructure failures stop poisoning bead-quality and learning signals.
- An atomic bead-rs attempt-resolution operation can later close the remaining
  crash window without changing the NEEDLE domain model.

### Costs and risks

- Telemetry, trace, worker registry, outcome handling, statistics, and tests
  need coordinated schema migration.
- Existing events lack attempt IDs and must be treated as legacy observations,
  not silently synthesized into authoritative attempts.
- During migration, consumers must tolerate both legacy and versioned events.

## Implementation

1. Add versioned Attempt, ContextManifest, EvidenceBundle, and Resolution
   types without changing existing lifecycle behavior.
2. Generate one attempt ID before each claim/dispatch and propagate it through
   prompts, gates, traces, telemetry, and outcome handling.
3. Introduce a pure resolution reducer and a side-effect applier.
4. Emit resolution only after re-reading authoritative bead state.
5. Replace action counters with unique-attempt and verified-resolution metrics.
6. Integrate bead-rs's portable atomic resolution contract when its advertised
   capability is available; retain a reconciled legacy sequence otherwise.

Implementation is complete only when crash-boundary and replay tests prove
that one attempt produces at most one durable resolution and completion event.

## Post-Pluck reconciliation audit

The legacy post-Pluck graph was audited against this decision on 2026-09-19.
The graph remains the compatibility path for existing lifecycle behavior; no
resolver decision, ownership fence, release safety rule, split behavior, or
operator event is removed merely to make the types look more like ADR-024.

| Legacy surface | ADR-024 mapping and current boundary |
| --- | --- |
| `ResolveResponse`, `ResolveDecision`, and `resolve_strict` | A bounded resolver proposal. `Complete`, `Retry`, `Blocked`, and `Split` are requested lifecycle actions, not semantic `AttemptOutcome` values and not authoritative `Resolution` records. Strict parsing and the safe fallback remain required compatibility behavior. |
| `ResolveContext` | An adapter from dispatch observations (bead, exit, output, duration, interruption, and optional evidence) into the resolver prompt. It is not yet a durable, versioned `ContextManifest`; its fields must not be treated as authoritative provenance. |
| `resolve::evidence::EvidenceBundle` and its seven sections | The current bounded, sanitized, read-only implementation of the ADR evidence concept: acceptance criteria, dispatch output, Git state, commits/diff, validation, failure history, and trace tail. It is captured for resolution and telemetry, but is not yet a versioned durable record with a manifest identity. |
| `DecisionExecutor`, `AppliedDecision`, and `ReleaseCause` | The legacy Resolution applier. Ownership checks, validation gates, safe release, split compensation, and authoritative rereads protect the requested action. The result still describes the applied lifecycle action rather than a complete immutable `Resolution`. |
| `AttemptResolvedFields` and `attempt.resolved` | A provisional attempt ledger observation. It carries the attempt, bead, worker, adapter/model, prompt, gate, outcome, action, exit, commit, cost, and terminal fields. `exit_code` is explicitly observational. Current rows remain `provisional=true`, with no confirmed state or context-manifest hash, so they are not the ADR’s final Resolution. |
| `bead_store::AttemptResolution` and `ResolveReceipt` | A capability-gated bead-rs adapter and replay receipt for the future atomic contract. Unsupported backends retain the reconciled legacy sequence; these types do not themselves perform the requested lifecycle action. |
| `ResolutionApplied`, `ResolutionFailed`, and `ResolveEvaluated` | Legacy post-Pluck operator telemetry for the controller/applier path. They remain queryable and are intentionally preserved, but consumers must not use them as proof of an ADR-024 Resolution without the authoritative reread/confirmation fields. |
| `BeadAction` and `run_post_dispatch_resolution` | `BeadAction` is the normal dispatch postcondition guard: every outcome selects a lifecycle action before mutation. `run_post_dispatch_resolution` is the exceptional post-Pluck reconciliation controller, invoked once after the agent is inactive and the claim fence still holds. Neither is the final ADR reducer. |
| `attempt_history::AttemptRecord` | Bounded, derived context for a later attempt. It is useful for prompts and failure history but is not an authoritative attempt or resolution ledger. |

The implementation audit sorts the existing work as follows:

- Shipped: `needle-b5793c98` (post-Pluck action graph), `needle-aee9e898`
  (bounded evidence), `needle-3a6e1ecd` (strict decision schema),
  `needle-6d057322` (post-Pluck invocation and ownership fence), and
  `needle-eba55bfc` (telemetry and lifecycle coverage). Their executor and
  postcondition blockers, including `needle-dc21a48b`, `needle-07174dcf`,
  `needle-2eff276a`, `needle-6d76f548`, `needle-97397df2`,
  `needle-62e7d8c1`, `needle-4a99daf3`, `needle-7ef79a8f`, and
  `needle-d69a4af0`, are closed with their own acceptance evidence.
- Compatibility by design: the legacy decision vocabulary, executor result,
  resolution-applied/failed telemetry, and backend capability fallback remain
  necessary while the versioned ADR types are introduced. `needle-13ad0633`
  is related fleet self-healing work, not a blocker for this reconciliation.
- Remaining/conflicting: `needle-3386daef` is not closed. The orphaned
  exit-zero/no-shipped-work path safely releases the bead, but its handler
  still reports `Outcome::Success` and the provisional ledger still records
  `verified_success`; that conflicts with ADR-024's requirement that success
  requires accepted evidence and confirmed closure. Its old orphan test has
  not been inverted to assert the semantic failure. The boot-recovery source
  work from `needle-6d76f548` is shipped, while its parked integration fixture
  still calls a non-existent `Worker::run_cycle()` and must be rewritten
  against the actual worker entry point before it can serve as a regression.
- Not yet implemented by this graph: versioned durable `Attempt`,
  `ContextManifest`, `ExecutionTrace`, and `Resolution` records; attempt-ID
  generation before claim; confirmed-state emission after the authoritative
  reread; and crash-boundary/replay proof of at-most-one durable resolution.

Until that migration lands, consumers must treat provisional attempt rows and
the legacy post-Pluck events as observations or requested-action telemetry.
Only a future confirmed, authoritative resolution record may grant completion
credit. The existing graph continues to own safe lifecycle mutation and all
of its current behavior while that boundary is migrated.

## Related

- [Current software-factory plan](../plan/plan.md)
- [ADR-006: Bead lifecycle reliability](006-bead-lifecycle-reliability.md)
- [ADR-020: Verification gates judge committed state](020-verification-gates-judge-committed-state.md)
- [ADR-023: Gate execution errors are infrastructure](023-gate-execution-errors-are-infrastructure-not-bead-failures.md)
- bead-rs ADR-011: Atomic idempotent attempt resolution
