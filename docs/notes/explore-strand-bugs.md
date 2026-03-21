# Explore Strand Bugs

Extracted from git log and bead history in NEEDLE-deprecated.

## Overview

The explore strand (strand 3 in the priority waterfall) searches for beads
in other workspaces by traversing the filesystem. It runs in two phases:
Phase 1 (down) searches child directories, Phase 2 (up) walks parent
directories and their siblings. Several bugs made explore a source of
performance problems and incorrect behavior.

## Bug 1: Unbounded Find (nd-60z92o)

**What happened:** Explore's Phase 1 used `find` with no depth limit to
search for `.beads/` directories. On a server with many directories (e.g.,
`/home/coding` with dozens of projects, node_modules, .git, etc.), this
triggered a filesystem scan across the entire home directory tree.

Phase 2's upward walk had no bound either -- it could walk all the way to
`/` looking for sibling workspaces.

**Impact:** With 40+ idle workers running explore simultaneously, the
CPU load hit 35+ on a 20-core machine. The `find` command alone consumed
most of the server's I/O capacity.

**Fix (commit 5637288):** Bounded Phase 1 `find` depth and Phase 2 upward
walk using `explore.max_depth` (default: 3) and `explore.max_upward_depth`
(default: 3) configuration.

## Bug 2: Dotfile Directories Included (commit 5a66ea3)

**What happened:** Explore searched dotfile directories (`.git`, `.npm`,
`.cache`, `.local`, `.nvm`, etc.) for `.beads/` folders. These directories
are often deeply nested and contain millions of files.

**Impact:** Massive I/O overhead from scanning irrelevant directories.
The `.git/objects` directory alone could contain hundreds of thousands of
files.

**Fix:** Added `-not -path '*/\.*'` exclusion to the find command.

## Bug 3: Workers Leaving Home Workspace (nd-08covc)

**What happened:** When a worker's home workspace had beads in `in_progress`
status (being worked by other workers), explore treated the workspace as
"done" and moved the worker to a different workspace. When the worker
returned, it found its home workspace's beads still in progress and left
again, bouncing between workspaces without doing useful work.

**Root cause:** Explore checked for "claimable" beads (open + unclaimed) but
did not consider that `in_progress` beads indicated active work in the
workspace. A workspace with only `in_progress` beads is not "done" -- it is
"in progress."

**Fix (commit 9d2a6b3):** Skip exploring when the home workspace has
`in_progress` beads. The worker stays in its home workspace and waits for
work to become available (either new beads are created or in-progress beads
complete and unlock dependent beads).

## Bug 4: Explore as Accidental Load Generator

**What happened (from memory: feedback_needle_operations.md):** The explore
strand's workspace discovery (`find /home/coding -name .beads`) was
identified as CPU-expensive. With 40+ idle workers running it simultaneously,
it drove server load to 35+ on 20 cores.

**Operational lesson:** The fleet was capped at ~20 workers, and workers
were staggered on launch (`sleep 1-2` between each) to avoid thundering herd
on startup where all workers hit explore simultaneously.

## Lessons for the Rewrite

### 1. Filesystem traversal must be bounded

Any `find` or directory walk needs explicit depth limits, exclusion patterns,
and timeouts. The default should be conservative (shallow search) with
opt-in expansion.

### 2. Cache workspace discovery results

Once the system knows where workspaces are, it should cache the results
rather than re-scanning the filesystem every iteration. Invalidation can be
triggered by file events (inotify) or periodic refresh.

### 3. Configure workspace list explicitly

Rather than auto-discovering workspaces by scanning the filesystem, the
rewrite should support explicit workspace configuration. Auto-discovery can
be a supplement, not the primary mechanism.

### 4. Workers should stay in their assigned workspace

The workspace-switching behavior added complexity (restart engine from
strand 1, cap at 5 restarts) and caused bugs (bouncing between workspaces,
leaving work undone). Consider a simpler model where workers are assigned to
a workspace and stay there, with a separate mechanism for redistributing
workers to busy workspaces.

### 5. Stagger concurrent operations

Multiple workers running the same expensive operation (filesystem scan)
simultaneously multiplies the cost without benefit. Use jitter, per-worker
offsets, or a shared scan result to avoid redundant work.

## Source Evidence

- Commit `5637288`: bound Phase 1 find depth and Phase 2 upward walk
- Commit `5a66ea3`: exclude dotfile directories from workspace discovery
- Commit `9d2a6b3`: skip exploring when home workspace has in_progress beads
- Memory: feedback_needle_operations.md -- fleet ops lessons (40+ workers, load 35+)
- Bead `nd-60z92o`: explore triggers environment-wide find
- Bead `nd-08covc`: fix worker loop after completion
