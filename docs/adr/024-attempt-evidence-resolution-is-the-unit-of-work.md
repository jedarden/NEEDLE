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

## Related

- [Current software-factory plan](../plan/plan.md)
- [ADR-006: Bead lifecycle reliability](006-bead-lifecycle-reliability.md)
- [ADR-020: Verification gates judge committed state](020-verification-gates-judge-committed-state.md)
- [ADR-023: Gate execution errors are infrastructure](023-gate-execution-errors-are-infrastructure-not-bead-failures.md)
- bead-rs ADR-011: Atomic idempotent attempt resolution

