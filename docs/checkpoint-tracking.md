# Checkpoint Tracking

## Problem

The checkpoint system (`bead sync flush-only`) creates generation objects under `.beads/checkpoint/objects/` that reference each other in a chain. The pointer files (`current.json`, `previous.json`, `forensic.jsonl`) name an `active_root` object, but if that object is not committed to git, a fresh clone will fail pointer verification.

## Solution

Every commit that publishes `.beads/checkpoint/` must include:
1. The pointer files: `current.json`, `previous.json`, `forensic.jsonl`
2. The objects referenced by both `current.json.active_root.path` and `previous.json.active_root.path`

This ensures the checkpoint is self-verifying after a fresh clone.

## Implementation

Use `scripts/commit-checkpoint.sh` to commit checkpoint changes:

```bash
./scripts/commit-checkpoint.sh "chore: checkpoint commit with active root objects"
```

The script:
1. Reads the checkpoint state already published by `bead sync flush-only`
2. Extracts the active root paths from `current.json` and `previous.json`
3. Stages the pointer files and both active root objects
4. Stages every superseded tracked object as deleted, including objects the
   flush already removed from the worktree
5. Verifies that Git pairs each new root with a superseded deletion as a
   rename whenever superseded history exists
6. Commits the changes atomically

## Superseded Objects

Objects listed in `deleted_paths` of the checkpoint manifests are superseded and should not remain tracked. The commit script automatically removes these from git when they are no longer referenced by any active root.

**Strategy:** Git-based cleanup (see `docs/checkpoint-cleanup-strategy.md` for full rationale).

**Implementation:** As part of each checkpoint commit, `scripts/commit-checkpoint.sh` calls `scripts/cleanup-superseded-checkpoint-objects.sh`, which:

1. Extracts the two active objects from `current.json.active_root.path` and `previous.json.active_root.path`
2. Lists all tracked objects in `.beads/checkpoint/objects/`
3. Removes any tracked object not in the active set using `git rm`
4. Commits the removals atomically with the new checkpoint

This ensures:
- Only two active objects exist in the working tree
- Superseded objects remain in git history for recovery
- The cleanup is automatic and idempotent

The rename shape is part of the checkpoint contract, not cosmetic diff
formatting. A monolithic root added without its superseded deletion makes Git
and Forgejo treat the entire snapshot as newly introduced text, needlessly
rescanning tens of thousands of unchanged lines. The commit helper therefore
checks `git diff --cached -M` and refuses an unpaired root addition when a
tracked superseded object exists. A first-ever checkpoint, which has no
historical object to pair, remains a valid addition.

## Verification

To verify checkpoint integrity:

```bash
bead doctor
```

A healthy checkpoint should not report missing root objects.

## CI Integration

The needle-ci workflow automatically commits checkpoint changes after running tests. This ensures that checkpoint modifications made during test execution are properly committed with their active root objects.

**Workflow behavior:**
1. Tests run via `definition-of-done.sh --all` (which may flush checkpoints)
2. If checkpoint files were modified, the workflow uses `scripts/commit-checkpoint.sh` to commit them with proper active root objects
3. The commit is pushed back to the repository

This automation prevents the "missing active root object" error that can occur when checkpoint changes are committed manually without including the referenced objects.

## Fresh Clone Recovery

If a fresh clone has an unrecoverable checkpoint (missing root objects), the monolith view (`forensic.jsonl`) can still be used for recovery:

```bash
bead init
bead sync import-only --input .beads/checkpoint/forensic.jsonl \
  --restore-into-empty --actor <you>
```

However, this is a fallback - the committed checkpoint with active roots should always verify successfully.

## Related

- `docs/checkpoint-cleanup-strategy.md` - Superseded object cleanup strategy
- bead-rs plan.md section 7 (checkpoint design)
- ADR-006 (bead store architecture)
- Commit `ffbef35` in bead-rs (similar fix)
