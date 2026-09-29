# Checkpoint Commit Workflow

`current.json` and `previous.json` point to immutable generation objects. Every
checkpoint publication must carry both pointers, the forensic view, both
validated roots, and stale tracked-object removals together.

For a standalone checkpoint commit, flush and use the cadence-bounded helper:

```bash
bead sync flush-only
./scripts/commit-checkpoint.sh "chore(beads): checkpoint"
```

The helper coalesces standalone publications for 900 seconds by default, holds
the bead-rs checkpoint publish lock, validates root hashes before pruning, and
commits only checkpoint paths. A coalesced flush remains durable in the
working tree for the next publication. `--force` requests an immediate commit.

For checkpoint data that should ride with an intentional source commit, stage
it through the shared publisher, then stage the source paths precisely:

```bash
bead sync flush-only
./scripts/checkpoint-publish.sh stage
git add path/to/the/actual/source/changes
git commit -m "feat: actual source change"
```

The pre-commit hook verifies that staged pointer changes have both root blobs
in the index with matching hashes. Do not use `git add -A` for checkpoint
publication; it can stage unrelated files or superseded generation objects.

For recovery from a missing root, use the verified forensic view only after
confirming the workspace backend and following the recovery procedure in
`docs/checkpoint-tracking.md`.
