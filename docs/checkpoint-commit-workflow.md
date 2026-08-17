# Checkpoint Commit Workflow

## Problem

`.beads/checkpoint/current.json` and `previous.json` are pointer files that reference specific generation objects under `objects/`. If these pointers are committed without their referenced objects, a fresh clone will fail verification with:

```
WARN checkpoint_freshness: Integrity error: Root object file missing
```

## Solution

Every checkpoint commit must include **both** the pointer files AND the active root objects they reference.

## Proper Workflow

### 1. Flush the checkpoint

```bash
bead sync flush-only
```

This updates:
- `.beads/checkpoint/current.json` (latest generation)
- `.beads/checkpoint/previous.json` (previous generation)  
- `.beads/checkpoint/forensic.jsonl` (compatibility view)

And creates a new generation object in `objects/gen-*.jsonl`.

### 2. Commit the checkpoint

**Use the provided script** to dynamically resolve and stage the active root objects:

```bash
./scripts/commit-checkpoint.sh
git commit -m "chore: checkpoint flush"
```

The script:
- Reads `current.json` and `previous.json` to extract `active_root.path`
- Stages the pointer files (current.json, previous.json, forensic.jsonl)
- Stages ONLY the two active root objects (the ones referenced)
- Does NOT stage superseded objects (those listed in `deleted_paths`)

### 3. Push

```bash
git push origin main
```

## What Gets Tracked

**Tracked in git:**
- `.beads/checkpoint/current.json` (pointer to latest generation)
- `.beads/checkpoint/previous.json` (pointer to previous generation)
- `.beads/checkpoint/forensic.jsonl` (monolithic view for compatibility)
- The TWO active root objects currently referenced by the pointers above

**NOT tracked (intentionally untracked):**
- All superseded generation objects (those in `deleted_paths` of the pointers)
- These accumulate locally but are never added to git

## Why This Works

1. **Verification:** A fresh clone has both the pointer AND the object it references, so `bead doctor` verification succeeds.

2. **Minimal history:** We only track the two active root objects (~3MB each), not the entire 206MB history of 86 superseded generations.

3. **Git deduplication:** `forensic.jsonl` is byte-identical to the current active root object, so git stores it as a single blob with no additional space cost.

4. **Dynamic resolution:** The script reads the pointers at commit time, so it works even as generation IDs change on every flush.

## Common Mistakes

### ❌ `git add .beads/checkpoint/`

This stages EVERY file in the directory, including 80+ superseded objects that should never be tracked. Result: 206MB added to git history.

### ❌ `git add .beads/checkpoint/*.json`

This only stages the pointer files without the objects they reference. Result: Fresh clone fails verification.

### ❌ Static `.gitignore` with `gen-*.jsonl`

This would ignore ALL generation objects, including the active roots we need to track. Result: Active roots can't be committed even intentionally.

## Recovery from Bad State

If you've committed pointers without objects:

1. Find the missing object from the pointer:
   ```bash
   jq -r '.active_root.path' .beads/checkpoint/current.json
   ```

2. If the object exists locally (it should), add it:
   ```bash
   git add .beads/checkpoint/objects/gen-*.jsonl
   git commit --amend --no-edit
   ```

3. If the object is missing, restore from the monolith:
   ```bash
   bead init
   bead sync import-only --input .beads/checkpoint/forensic.jsonl \
     --restore-into-empty --actor <you>
   ```

## Verification

Test that a fresh clone works:

```bash
# In a temp directory
git clone --depth 1 <this-repo-url> test-clone
cd test-clone
bead doctor
# Should pass without "Root object file missing" error
```
