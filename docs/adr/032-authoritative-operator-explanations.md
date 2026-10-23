# ADR-032: Operator explanations project authoritative state without mutating it

**Status:** Accepted — 2026-10-23; implementation pending
**Deciders:** operator request to implement the outcome-reliability roadmap
**Tracking:** plan revision 37, section 4.13; implementation owners in that section

## Context

Readiness, claims, attempts, verification, delivery, recovery and learning have
separate authoritative records. Operators currently have to correlate these to
answer why an outcome has not finished. Empty or stale projections cannot prove
that a queue is empty, an attempt is complete, or human action is necessary.
ADR-024 and ADR-028 already define completion and ownership; O1–O3 already own
the learning snapshot. A new dashboard or independent lifecycle store would
introduce another inconsistent authority.

## Decision

Provide a versioned, bounded, read-only explanation for one explicit workspace
and bead, exposed as `needle explain <bead-id> --workspace <path> [--json]`.
Reuse backend readiness diagnostics through BeadStore capabilities, existing
attempt/resolution evidence, worker registry, recovery plans and O1 when
available. Do not shell out around the backend resolver or infer authoritative
state from logs alone. Unsupported sources are explicit unknowns.

The projection includes source identities and observation times, freshness,
current ownership, required and observed acceptance evidence, delivery state,
blocking dependencies, retry eligibility, and the next permitted action.
Implementation-present, verification-pending, delivery-pending and complete
are presentation states derived from evidence, not new bead lifecycle states.
Only accepted evidence plus confirmed backend resolution establishes complete;
provisional ledger rows never do. ADR-031 external-turn outcomes retain their
own identity and are not relabeled as bead closures.

Every source read has a timeout inside an overall deadline. Partial results
identify missing sources and remain useful. Human and JSON renderings consume
the same sanitized snapshot and stable reason codes. A machine-readable
completeness field distinguishes complete explanations from partial ones;
exit codes distinguish a successful read, a degraded read and invalid input.
No read renews a lease, repairs state, runs a gate or files a bead.

A decision request carries a stable identity, exact question, recommendation,
consequences and the missing authority. Existing answered decisions are reused
only within their recorded scope. Diagnostic remediation is advice, never
implicit permission to execute. Status and doctor may consume this projection;
O1/O2 keep ownership of their existing learning-specific behavior.

## Alternatives and consequences

A second database or dashboard is rejected: the projection is rebuildable.
Inferring success from exit status is rejected by ADR-024. Missing sources
reduce completeness instead of blocking the whole command. Operators gain one
explanation, but consumers must preserve freshness and unknown states.

## Acceptance and rollout

Fixtures cover a completed outcome, pending verification/delivery, stale claim,
held proposal, unchanged retry, source timeout, missing evidence and redaction.
Repeated reads leave authoritative stores byte-for-byte unchanged. A process
fixture proves the total deadline and JSON/human parity. Use existing declared
test harnesses. Ship the read-only surface before changing status consumers;
no fleet configuration or human decision workflow is activated by this ADR.
