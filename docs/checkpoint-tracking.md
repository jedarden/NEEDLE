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
1. Flushes the checkpoint to ensure current state
2. Extracts the active root paths from `current.json` and `previous.json`
3. Stages the pointer files and both active root objects
4. Removes superseded objects that are tracked but no longer referenced
5. Commits the changes atomically

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

## Verification

To verify checkpoint integrity:

```bash
bead doctor
```

A healthy checkpoint should not report missing root objects.

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
