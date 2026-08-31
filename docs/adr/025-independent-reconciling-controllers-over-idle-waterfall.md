# ADR-025: Independent Reconciling Controllers Replace Idle-Waterfall Coupling

**Status:** Accepted — 2026-08-31

**Decision-makers:** NEEDLE maintainers and software-factory operators

## Context

NEEDLE's strands are currently evaluated as an ordered waterfall. Productive
work selection comes first; maintenance, loop detection, reflection, and
exhaustion handling occur later. A continuously ready queue or a repeatedly
redispatched bead can therefore starve the very mechanisms intended to detect
and correct the condition.

The strand implementations contain useful behavior, but their position in a
single idle path conflates scheduling priority with correctness and health.

## Decision

NEEDLE will evolve the strand runner into independently scheduled,
idempotent reconciling controllers:

| Controller | Responsibility |
| --- | --- |
| Claim scheduler | Select and atomically claim eligible work |
| Executor | Build context and run one identified attempt |
| Resolver | Classify evidence and request a lifecycle transition |
| Lifecycle reconciler | Repair orphaned, stale, or partially applied state |
| Workspace-health controller | Detect gate, storage, provider, and loop conditions |
| Work proposers | Explore, Weave, Unravel, Pulse, and Generation proposals |
| Reflector | Turn selected episodes into evaluated lesson candidates |
| Escalation controller | Apply bounded retry, quarantine, and human handoff policy |

Controllers run on their own event and time triggers, acquire scoped leases,
and reconcile desired state from authoritative observations. No health,
resolution, or reflection controller may depend on the ready queue becoming
empty. At most one controller owns each mutation kind; other controllers emit
recommendations or intents.

Existing strand names may remain as user-facing configuration aliases during
migration, but the waterfall is not the target execution model.

## Consequences

### Benefits

- Poisoned or permanently nonempty queues cannot suppress repair or learning.
- Each controller can be tested for idempotence and crash recovery.
- Scheduling cadence becomes explicit rather than an incidental strand order.
- Useful strand logic can be migrated incrementally instead of rewritten all
  at once.

### Costs and risks

- The worker loop and configuration model require a compatibility layer.
- Concurrent controllers need leases, bounded queues, and mutation ownership.
- Poorly chosen triggers could add load; every controller therefore needs a
  budget and backoff policy.

## Implementation

1. Introduce the controller scheduler alongside the current StrandRunner.
2. Move resolution and lifecycle reconciliation first because they protect
   work truth.
3. Move Splice and gate health next so they run independently of Pluck.
4. Convert work-generating strands to proposal-only controllers with explicit
   backlog budgets.
5. Move Reflect last, after attempt and resolution facts are trustworthy.
6. Remove the waterfall only after parity, idempotence, and starvation tests
   pass under the controller scheduler.

## Related

- [Current software-factory plan](../plan/plan.md)
- [ADR-022: Visible time-bounded quarantine](022-visible-time-bounded-quarantine-and-escalation-ladder.md)
- [ADR-023: Gate execution errors are infrastructure](023-gate-execution-errors-are-infrastructure-not-bead-failures.md)
- [ADR-024: Attempt, evidence, and resolution](024-attempt-evidence-resolution-is-the-unit-of-work.md)

