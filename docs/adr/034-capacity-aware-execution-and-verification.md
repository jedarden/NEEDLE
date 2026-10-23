# ADR-034: Execution and verification consume separate bounded capacity

**Status:** Accepted — 2026-10-23; implementation pending
**Deciders:** operator request to implement the outcome-reliability roadmap
**Tracking:** plan revision 37, section 4.13; N-T33 remains the launch-state owner

## Context

CPU/RAM launch checks, provider limits and controller budgets exist, but agent
execution and verification compete for different constrained resources.
Launching more workers can increase build-lock and verification waiting time
without increasing accepted outcomes. Admission-blocked workers must retain
N-T33's resident, observable behavior rather than restart or claim work early.

## Decision

Extend the existing admission machinery with separate agent-execution and
verification capacity classes. Compose host/cgroup headroom, disk headroom,
provider limits, workspace constraints and existing controller budgets. Retain
bead-rs authority over claims and native resource keys; capacity permits are
local scheduling state, not distributed task locks or claim credentials.

Verification permits are acquired before an expensive gate starts. They have
explicit owner/attempt identity, bounded acquisition, cancellation and release
on completion. Crash recovery requires confirmed owner death or an existing fenced
controller takeover, plus proof the occupied gate process tree has stopped
before freeing its slot. A surviving gate retains its occupied permit under
recovery ownership until completion or confirmed termination; worker death,
takeover and elapsed time alone cannot free capacity. Across
processes on one host use the existing shared state/coordination mechanism,
not one independent in-process semaphore per worker. A waiting attempt keeps
its required claim renewal and is reported as waiting for capacity, not as a
failed task. No permit bypasses Cargo's repository target lock or creates an
extra target directory. Exact-input receipts can eliminate a repeat execution
only under the existing receipt trust rules.

The supervisor uses a versioned pressure snapshot with observation age,
configured limits and binding reason. Sustained verification backlog or disk
pressure bounds new agent launches; unknown required capacity fails admission
with a reason. Use hysteresis and capped jittered rechecks to avoid oscillation.
Do not kill productive attempts merely to reach a lower target. Preserve
bounded capacity for reconciliation/claim renewal so backpressure cannot block
its own recovery. Fair ordering prevents one repository monopolizing gate
slots; report wait duration and saturation by resource class.

No automatic budget increase, provider substitution, remote CI mutation or
live worker-count change is authorized. CI queue data is advisory unless its
freshness and capability contract is configured; absence is explicitly unknown.
New scheduling controls ship disabled or in observation mode with validated
bounds. Activation uses the existing release/canary and operator authority.

## Alternatives and consequences

A single worker-count limit hides the bottleneck. Unbounded verification
parallelism and per-worker target directories are rejected. Local permits do
not solve cross-host coordination; existing remote CI and provider authorities
continue to govern their capacity. Extra queueing is acceptable when it reduces
contention and improves accepted outcomes per elapsed hour and per dollar.

## Acceptance and rollout

Use deterministic clock/pressure fixtures plus isolated process contracts for
cross-process permit contention, cancellation, crash reclaim and no live-owner
reclaim. Kill a worker while its gate survives: the occupied slot must remain
unavailable until that gate process tree stops. Prove no claim/launch while blocked, bounded retries, fair progress,
restoration after pressure clears and no false completion/failure attribution.
Record baseline and candidate verification wait, throughput, cost, retries and
regressions before recommending activation. N-T33, controller leases and budgets
remain prerequisites; their unresolved holds must not be bypassed.
