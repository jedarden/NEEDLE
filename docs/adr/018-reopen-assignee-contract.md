# ADR-018: Bead Reopen Assignee Contract

## Status

Accepted (2026-08-24, implemented 2026-08-28)

## Context

The `bead reopen` command transitions a closed issue back to open status. A key design question is what should happen to the assignee field when a bead is reopened.

### Historical Problem

Prior to 2026-08-24, bead-rs's `reopen_issue_impl` retained the assignee when reopening. This created a **silent failure mode**:

1. A bead is closed (status=closed, assignee=some-worker)
2. The bead is reopened (status=open, assignee=some-worker)
3. NEEDLE workers skip beads with non-null assignees when claiming from the ready frontier
4. If `some-worker` is no longer working on that bead, it becomes **permanently unclaimable**
5. The bead appears healthy (status=open) but never gets picked up

This manifested as a fleet-wide issue on 2026-08-16:
- **583 beads** across **47 workspaces** had stale assignees
- Ten workspaces were fully starved (zero claimable beads)
- Fleet-wide ready count: 342 → 576 after cleanup

### NEEDLE-Specific Considerations

NEEDLE's worker behavior differs from typical work queue systems:

- **Ephemeral workers**: Workers run with `--count 1` and relaunch under the same name
- **Worker name ≠ active dispatch**: A worker name being "alive" doesn't mean it's working on the original bead
- **Mend cleanup limitation**: The mend strand's reap logic skips beads whose assignee is in `live_worker_ids`, but this excludes too many beads because worker names persist across relaunches

### Industry Patterns

Research into common work queue systems:

**GitHub Issues**: Reopening retains the assignee, but assignees are typically humans who remain explicitly assigned across long timeframes.

**Jira**: Reopening retains the assignee, but similar to GitHub, assignees are persistent human actors, not ephemeral processes.

**Linear**: Similar to Jira - assignee is preserved on reopen.

**Pattern**: Human-worked systems preserve assignees on reopen because the assignee represents ongoing responsibility, not process identity.

**NEEDLE's difference**: Bead assignees represent **process identity** (which worker instance claimed it), not **responsibility** (who should work on it). When a process exits or times out, that identity becomes stale.

## Decision

**`bead reopen` MUST clear the assignee.**

### Rationale

1. **Prevents silent starvation**: A reopened bead without an assignee is immediately visible to the ready frontier and claimable by any worker.

2. **Matches NEEDLE's process identity model**: Bead assignees represent transient process identities, not persistent responsibility. Clearing on reopen acknowledges the original worker process is no longer working on it.

3. **Avoids complex staleness detection**: We don't need to track whether an assignee is "stale" - the act of reopening signals the bead needs fresh attention.

4. **Symmetric with closure**: Closing a bead preserves its assignee for historical record, but reopening acknowledges that work is restarting from a clean state.

5. **No loss of information**: The bead's full history (including previous assignees) is preserved in the audit trail. Clearing the assignee doesn't erase who worked on it previously.

### Implementation

The `bead reopen` command:

```bash
bead reopen <id>
```

Effects:
- Sets `base_status` to `open`
- Clears `closed_at` and `close_reason`
- Clears `manual_blocked` flag
- **Clears `assignee`** (makes issue claimable)
- Advances `updated_at`
- Increments `revision`
- Appends `reopened` audit event

This is the only valid way to transition from `closed` to `open`. Generic `update` cannot do this.

### Alternative Considered

**Option: Retain assignee with warning**

We considered retaining the assignee and emitting a warning when a bead is both `open` and `assigned`. However:

- **False positives**: Workers run with `--count 1` and relaunch under the same name, so "assignee is alive" doesn't mean "assignee is still working on this bead"
- **Doesn't solve starvation**: A warned-but-unclaimable bead is still unclaimable
- **Complex staleness heuristics**: We'd need to track "last active dispatch time" per worker and expire stale assignments, adding significant complexity

The clean contract (reopen clears assignee) is simpler and correct for NEEDLE's process identity model.

## Consequences

### Positive

1. **No silent starvation**: Reopened beads are immediately claimable
2. **Simplified semantics**: No need for staleness detection or warning systems
3. **Clear mental model**: Reopen = "fresh start" for assignment
4. **Matches process identity**: Assignee represents process, not responsibility

### Neutral

1. **Manual reassignment needed**: If a human explicitly wants a specific worker to continue a reopened bead, they must manually reassign it after reopen
2. **Audit trail preserved**: Previous assignee is still visible in the bead's audit history

### Negative

1. **Loss of continuity**: In rare cases where the same worker process is still running and should continue the bead, the assignment is lost and must be re-established manually (mitigated by checking bead status before closing)

## Related

- ADR-006: Bead Lifecycle Reliability (test contamination incident)
- Bead needle-44e7e5cd: "Mend never clears a stale assignee whose worker identity is still alive"
- Bead needle-07a5ab00: "bead-rs reopen leaves stale assignee"
- Fleet-wide cleanup: 2026-08-16, 583 beads across 47 workspaces
