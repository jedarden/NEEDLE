# Publishing bead-rs checkpoints

`bead sync flush-only` writes a durable JSONL generation and advances the
checkpoint pointers. The pointer files are only usable from a fresh clone when
the objects named by both `current.json` and `previous.json` are committed
with them.

For a standalone checkpoint publication, use the publisher:

```bash
bead sync flush-only
./scripts/commit-checkpoint.sh "chore(beads): checkpoint"
```

The same operation is available as `./scripts/checkpoint-publish.sh commit -m
"chore(beads): checkpoint"`. It validates both pointer schemas and root
hashes, holds bead-rs's checkpoint publish lock, stages the pointers, forensic
view, both roots, and stale tracked-object deletions in one Git index update,
then commits only those paths. Other staged and unstaged edits stay in place.

Standalone publications are coalesced for 900 seconds by default. Set
`NEEDLE_CHECKPOINT_COMMIT_DEBOUNCE_SECONDS` to change the interval. A skipped
publication exits successfully and leaves the durable flush in the working
tree for a later run. Use `--force` only when an immediate standalone
publication is necessary.

The commit helper also accepts `--workspace PATH` so one centrally installed
publisher can safely publish checkpoints from several repositories.

To include checkpoint state in an intentional source commit, stage it through
the helper and stage the source paths precisely:

```bash
bead sync flush-only
./scripts/checkpoint-publish.sh stage
git add path/to/the/actual/source/changes
git commit -m "feat: actual source change"
```

The tracked pre-commit hook rejects staged pointer changes unless both
pointer-declared roots are present in the final Git index and match their
SHA-256 hashes. Enable it once per clone:

```bash
./scripts/install-git-hooks.sh
```

Do not use `git add -A` for checkpoint publication. It can capture unrelated
machine-local files and superseded generation objects.
