# Checkpoint generation archive

`bead sync flush-only` publishes a new immutable generation object under
`.beads/checkpoint/objects/` on every flush and lists the superseded ones in
`current.json`'s `deleted_paths` — but nothing actually removes them, and
`bead sync` has no prune command. They accumulate unbounded.

They are not redundant. Measured 2026-08-16: of 90 objects on disk, only 25
existed anywhere in git history. Flushes vastly outnumber commits, so only the
generations that happened to coincide with a commit were ever captured by a
committed `forensic.jsonl`. The other 65 (156 MB) existed on exactly one disk,
untracked and unbacked-up.

Committing them raw is not an option: 221 MB, growing ~2.4 MB per flush. That
is the failure mode that cost commitgraph 817 MB of history and a 10.5h mirror
outage.

The snapshots are near-identical, so long-range compression collapses them:

| method                   | result              |
|--------------------------|---------------------|
| `zstd -3`                | 216 MB -> 27.7 MB   |
| `zstd -19 --long=27`     | 221 MB -> 0.42 MB   |

At ~420 KB the whole generation history fits in git, which gives it two-tier
durability for free via the Forgejo primary and the GitHub push mirror.

## Recreating / extracting

```bash
# extract (needs --long=27 to match how it was written)
zstd -d --long=27 -c .beads/archive/checkpoint-generations-YYYYMMDD.tar.zst | tar -xf - -C /some/where

# re-archive after generations accumulate again
tar -cf - .beads/checkpoint/objects/ \
  | zstd -19 --long=27 -o .beads/archive/checkpoint-generations-$(date -u +%Y%m%d).tar.zst
```

## Rule

**Archive and verify the round-trip before pruning `objects/`.** Every object
in the archive must sha256-compare byte-identical against its original before
anything is deleted. The live `active_root` objects named by `current.json` and
`previous.json` must never be pruned — they are tracked in git and the
checkpoint cannot be verified without them (see needle-ea87796e).
