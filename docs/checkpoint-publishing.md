# Publishing bead-rs checkpoints

`bead sync flush-only` writes a new immutable generation and advances the
checkpoint pointers. The generation ID changes on every flush, so a normal
`git add .beads/checkpoint/current.json` is incomplete: it can commit a pointer
without the object that pointer verifies.

After flushing, stage the checkpoint through the repository helper:

```bash
bead sync flush-only
./scripts/checkpoint-publish.sh stage
git add path/to/the/actual/source/changes
git commit -m "chore(beads): checkpoint"
```

Or let the helper stage the checkpoint and perform the commit:

```bash
./scripts/checkpoint-publish.sh commit -m "chore(beads): checkpoint"
```

The helper reads `active_root.path` and `active_root.sha256` from both
`current.json` and `previous.json`, verifies the files, and stages the resolved
paths together with the pointers and `forensic.jsonl`. It removes every other
`objects/gen-*.jsonl` file from the working tree. The current and previous
roots are the only generations retained locally; older generations remain
recoverable from Git history when they were committed.

The tracked pre-commit hook rejects a staged pointer change unless both
pointer-declared roots are present in the final Git index and match their
declared hashes. Enable it once per clone:

```bash
./scripts/install-git-hooks.sh
```

Do not use `git add -A` for checkpoint publication. It can capture unrelated
machine-local files and superseded multi-megabyte generation objects.
