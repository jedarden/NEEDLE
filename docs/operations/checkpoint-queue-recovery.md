# Checkpoint queue ownership and recovery

NEEDLE checks the native bead-rs checkpoint before selecting work or changing
beads. The check runs in the descriptor store, covering both home and roaming
workspaces. A verified `remote-advanced` checkpoint is reconciled; local
unpublished history is flushed. Dispatch proceeds only after the checkpoint
is aligned and ready. An integrity refusal pauses that workspace, preserves
its bead state, and emits `workspace.sync.paused` through tracing. Successful
revalidation emits `workspace.sync.recovered`. Read-only diagnostics remain
available. A failed completion flush never emits a successful bead completion.

## One claim authority per repository

Multiple workers can share one checkout and its SQLite database. Separate
hosts have separate claim authorities, even when Git checkpoints and workspace
UUIDs match. Configure one queue host in the repository's `.needle.yaml`:

```yaml
queue:
  owner_host: lab
```

Use the exact hostname returned by `hostname`, including its domain if any.
The setting is checked on every guarded operation and applies to roaming
workers too. Missing `queue.owner_host` preserves existing operation; an
explicit empty, malformed, or different owner pauses dispatch before any
backend mutation. This is a deployment policy, not a distributed lease.
Deploy it to every participating host before resuming a multi-host fleet.
Independent hosts must not keep writing the same queue. A future multi-host
deployment needs one common claim service; distinct fork UUIDs alone do not
prevent duplicated work.

## Recover the lab declarative-config incident

1. Stop new dispatch and quiesce every writer of this repository on lab and
   ex44, including watchdogs, interactive bead writers and checkpoint-publishing
   jobs. Preserve in-flight agent output. Do not modify ArgoCD-managed resources.
2. Confirm `.needle.yaml` selects bead-rs. Record the installed binary version
   and hash. Run `bead sync status --format json` and read-only `bead doctor
   --scope backup --json`; inspect their structured fields, not just exit codes.
   Older bead versions returned success for checkpoint integrity warnings.
3. Take consistent SQLite backups using SQLite's backup API, plus copies of
   config, current/previous pointers and every generation object they select.
   Store the evidence with restrictive permissions in a task-specific directory.
   A filesystem copy of a live WAL database alone is not a consistent backup.
4. Inventory both histories by `(origin_store_uuid, origin_event_sequence)`.
   Compare public content, find the common prefix, list local-only issues,
   comments, dependencies, completion reasons and claims. Preserve both original
   conflicting suffixes. In the observed incident the first disagreement was
   event 181, long before the ZCode workers were deployed; some checkpoint-closed
   tasks were still open in lab's live database.
5. Rehearse an explicit recovery in an isolated temporary workspace. Restore
   a verified retained generation using native `bead restore` (consult current
   help), then apply reviewed local-only semantic changes using public bead
   operations with provenance references to the retained evidence. Resolve
   issue-state conflicts individually; timestamp order alone is not proof that
   a repeated closure or reopen was intended. Do not renumber or rewrite the
   original conflicting audit records, delete the production database, or
   claim that the two contradictory event streams were losslessly merged.
6. The current `sync reconcile` only accepts a verified superset. It must refuse
   conflicting identities. `sync fork` preserves historical identities and
   requires a clean checkpoint; it does not repair this already-diverged store.
   If the reviewed recovery cannot account for every live-only change, keep
   writers paused and retain the evidence rather than discarding that work.
7. Publish and verify the recovered checkpoint in the rehearsal, then activate
   the reviewed recovery on the selected queue host while retaining the original
   store backups for rollback. Verify issue states, graph integrity, absence of
   stale claims and that already-completed tasks do not appear in `bead list
   --ready --json`. Flush, commit the exact checkpoint fileset and push to origin.
8. Deploy pinned, verified bead-rs and NEEDLE builds. Build bead-rs through its
   repository's `scripts/build-from-archive.sh`; keep binary hashes and source
   commits with the deployment record. Resume one canary worker and verify its
   claim, real work, closure and aligned checkpoint. Restore the remaining
   workers only after that cycle succeeds without a repeated task or sync error.

The installed fleet also needs the live-claim reaping fixes tracked by
`needle-a7372f9b`, `needle-8d14d0d1` and `needle-47a0afca` before rollout. During
this implementation, cleanup released an active interactive claim and five
running workers simultaneously reported that same bead. Checkpoint alignment
cannot make an incorrect release safe. Reaping must verify current execution
and claim ownership atomically; claim age or an absent fleet registration alone
is insufficient evidence that an interactive owner has stopped working.

Checkpoint transport must be coordinated with writers. bead-rs's
`.beads/operation.lock` protects validation and ordinary mutations; checkpoint
publication uses `.beads/checkpoint/publish.lock`. A transport replacing
checkpoint files must participate in the operation-lock boundary or quiesce
writers. Do not delete or replace runtime lock files. NEEDLE's additional
`.beads/needle-sync.lock` serializes its status/reconcile/operation sequence;
it cannot protect against an arbitrary external Git process ignoring the locks.
