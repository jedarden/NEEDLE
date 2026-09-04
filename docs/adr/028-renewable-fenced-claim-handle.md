# ADR-028: Carry a renewable fenced claim handle through every attempt

**Status:** Accepted — 2026-09-04
**Deciders:** operator (jedarden), NEEDLE maintainers
**Tracking:** plan revision 25; `needle-cd169aa6`, `needle-8d14d0d1`,
`needle-7e56d009`; bead-rs `beadrs-8c343a7c`

## Context

NEEDLE serializes the instant of claim, but a dispatched agent can outlive the
claim state that authorized it. A release, timeout, lease expiry, recovery
pass, or reassignment can make another worker eligible while the original
agent process continues editing the shared checkout. Expected revision checks
at one lifecycle call do not prove that the process still owns the attempt.

On 2026-09-04 two live worker processes were observed executing prompts for the
same bead and revision in the shared bead-rs checkout. During that overlap the
bead moved through different assignees and then back to open/unassigned while
the processes remained alive. Resource declarations and dependency edges did
not help because both processes believed they were executing the same unit of
work.

ADR-015 correctly rejects per-worker Git worktrees: they duplicate resources
and do not prevent duplicate work. Its operational one-worker-per-repository
guidance is useful as a fallback, but it is not a correctness proof for roaming
workers, lease expiry, or a stale process that resumes after reassignment.

## Decision

A successful claim returns a typed `ClaimHandle` that is the capability to
execute and later resolve one attempt. The handle binds at least:

- workspace/store identity, bead ID, and claimed revision;
- worker/actor identity and globally unique attempt ID;
- claim epoch or monotonically increasing fencing token;
- lease expiry and renewal state when the backend supports leased claims; and
- the negotiated backend capability document/hash.

NEEDLE carries this handle through prompt construction, dispatch, gate
execution, telemetry, and resolution. Dispatch is forbidden without a current
handle. Every claimant mutation and atomic attempt resolution supplies the
expected revision and fencing credential. A stale handle cannot close, release,
update, attach evidence, or publish an outcome.

Lease renewal runs independently of the agent process. If renewal fails, the
lease expires, ownership changes, or an authoritative reread no longer matches
the handle, NEEDLE cancels the local attempt and records an ownership-lost
observation. It does not apply a semantic result from that process. Recovery
may reassign only an expired, compare-and-reap claim and must produce the next
claim epoch.

For a backend that cannot provide the required fencing semantics, NEEDLE uses
an explicit compatibility mode that enforces one executor per workspace and
does not permit concurrent roaming dispatch into that workspace. Capability
absence is never silently treated as equivalent protection.

The shared-checkout and no-worktree decisions remain. Dependencies and resource
keys prevent different beads with declared overlap from running together; the
claim handle prevents two attempts for the same bead epoch from both retaining
authority.

## Consequences

### Positive

- A late or resurrected process is structurally unable to publish stale work.
- Worker registry state can distinguish a live process from an authoritative
  executing attempt.
- Lease-aware Mend and attempt resolution share one ownership definition.
- Duplicate-dispatch incidents become replayable contract failures rather than
  ambiguous Git collisions.

### Negative

- Renewal and cancellation become part of the critical execution path.
- Older backends lose concurrent roaming throughput unless and until they
  expose equivalent fencing.
- Adapters and tests must carry an opaque handle without leaking it into
  prompts, logs, or arbitrary agent-controlled text.
- A network or backend outage can conservatively cancel useful local work; its
  diff must be captured as non-authoritative recovery evidence before release.

## Alternatives considered

- **Rely on atomic claim alone:** rejected. Atomic selection prevents two
  simultaneous winners but does not fence a prior winner after release or
  expiry.
- **Use process/PID liveness as ownership:** rejected. A live process can be
  stale and a dead process can leave an unexpired claim; the bead store is the
  authority.
- **Use resource keys or file dependencies only:** rejected. They address
  different beads with known overlap, not duplicate attempts for one bead.
- **Create a Git worktree per worker:** rejected by ADR-015 and still does not
  prevent duplicate execution or stale lifecycle mutation.

## Implementation and verification

1. bead-rs `beadrs-8c343a7c` makes each claim epoch fenced and requires its
   credential on claimant mutations.
2. NEEDLE `needle-cd169aa6` introduces the renewable `ClaimHandle` and carries
   it through the complete worker attempt.
3. NEEDLE `needle-8d14d0d1` replaces age-only Mend release with lease-aware
   compare-and-reap.
4. NEEDLE `needle-7e56d009` replays the duplicate-worker incident end to end:
   one claim epoch may reach dispatch and resolution; a stale process cannot
   mutate after reassignment.
5. Capability-gated old/new backend conformance and canary deployment precede
   fleet activation. The installed development binary alone is not release
   evidence.

## Related

- [ADR-015: Concurrent same-repo worker isolation](015-concurrent-same-repo-worker-isolation.md)
- [ADR-024: Attempt, evidence, and resolution are the unit of work](024-attempt-evidence-resolution-is-the-unit-of-work.md)
- [ADR-025: Independent reconciling controllers](025-independent-reconciling-controllers-over-idle-waterfall.md)
- bead-rs ADR-011: atomic idempotent attempt resolution

## Supersedes

ADR-028 supersedes ADR-015 only where ADR-015 treats one-worker-per-repository
operational discipline as sufficient claim-lifetime protection. It retains
ADR-015's rejection of per-worker worktrees and its dependency guidance for
different beads that touch the same resources.
