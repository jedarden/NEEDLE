# Checkpoint Cleanup Strategy

## Problem

The checkpoint system (`bead sync flush-only`) operates in `mode=monolithic`, meaning every generation is a full snapshot. With each flush, a new generation object is created and older objects become superseded. Without cleanup, these accumulate unbounded (historically reached 86 files/206MB).

**Current state:** Only `current.json` and `previous.json` name active objects. All other objects in `deleted_paths` are superseded but remain on disk and in git.

## Chosen Strategy: Git-Based Cleanup

We use **Option 1: Git-based cleanup** with the following rationale:

### Why Git-Based Cleanup?

1. **History preservation:** Superseded objects remain in git history for recovery
2. **Working tree hygiene:** Active commits keep only active objects on disk
3. **Idempotent:** Safe to run multiple times
4. **No data loss:** Can recover any object from git history
5. **Simple automation:** One script call during checkpoint commits

### Why NOT other options?

**Option 2 (Workspace-local cleanup only):**
- ❌ Loses recovery capability from git history
- ❌ Requires external backup strategy
- ❌ Violates git's role as the source of truth

**Option 3 (Hybrid/Keep last N):**
- ❌ Unnecessary complexity - git history already preserves everything
- ❌ Arbitrary retention limit doesn't map to actual needs
- ❌ More complex to implement and maintain

## Implementation

The cleanup is integrated into `scripts/checkpoint-publish.sh`, which is used
by the standalone commit wrapper:

1. **Extract active objects:** Read `active_root.path` from `current.json` and `previous.json`
2. **Find superseded objects:** List all tracked objects in `.beads/checkpoint/objects/`
3. **Remove superseded:** Remove stale object files from the working tree and
   stage tracked deletions with the active roots
4. **Commit atomically:** Update the index in one exact-path `git add` and
   commit only those checkpoint paths

This happens automatically as part of every checkpoint commit - no separate cleanup step needed.

`commit-checkpoint.sh` debounces standalone checkpoint commits (see
"Batching (Debounce)" in `docs/checkpoint-tracking.md`). Superseded objects
accumulate between publications and are dropped together with the next
validated current/previous roots.

## Recovery

If a superseded object is needed for recovery:

```bash
# Find the commit that last had the object
git log --all --full-history -- '.beads/checkpoint/objects/<object-filename>.jsonl'

# Checkout that commit to restore it
git checkout <commit-sha> -- .beads/checkpoint/objects/<object-filename>.jsonl
```

The `forensic.jsonl` monolith also serves as a fallback for full-store recovery.

## Verification

After cleanup, only two objects should exist in `.beads/checkpoint/objects/`:

```bash
# Should show exactly 2 files
ls .beads/checkpoint/objects/

# Should show exactly 2 files tracked by git
git ls-files '.beads/checkpoint/objects/'

# Both lists should match (no untracked, no deleted)
git status .beads/checkpoint/objects/
```

## Related

- `docs/checkpoint-tracking.md` - Overall checkpoint design
- `scripts/commit-checkpoint.sh` - Implementation with automatic cleanup
