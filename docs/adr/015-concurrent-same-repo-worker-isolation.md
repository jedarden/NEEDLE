# ADR-015: Concurrent Same-Repo Worker Isolation

**Status:** Accepted — 2026-08-15
**Deciders:** operator (jedarden)
**Tracking:** plan.md §6.9; CLAUDE.md "NEEDLE Fleet Dispatch — no worktrees"

## Context

NEEDLE's current design gives every worker assigned to a repository the **same working directory**. The `bead.workspace` path is passed verbatim to dispatched agents via `{workspace}` template rendering — no per-worker suffix, clone, or `git worktree` derivation exists anywhere in the codebase.

The per-workspace claim `flock` (introduced in plan.md §6.1) guards only the CLAIMING step's `br update --claim` call. Once two workers are each dispatched into the same workspace, their agents can run concurrent `git add`, `commit`, `reset --hard`, and `checkout` operations in that single shared tree for the full duration of both dispatches, with nothing serializing them beyond whatever the agents themselves happen to do.

**Confirmed in code:**
- `src/dispatch/mod.rs:709` — `workspace` path rendered directly into agent command, no per-worker derivation
- `src/commit_hook.rs:28-31` — per-workspace advisory lock exists only for `inject_bead_id_trailer`, protecting NEEDLE's post-hoc amend, not the agent's work
- CLAUDE.md line "NEEDLE gives every worker assigned to a repo the *same* working directory" — already documented as current behavior

**Historical incident (2026-08-09):**
Bead `cg-l0v0kc` produced two byte-identical commits (`aadfdb3`, `c876058`) at the same second, both labeled `fix(cg-l0v0kc)`, from two concurrent workers (`alpha`, `luna`). The exact mechanism (claim race vs. reopen-and-reclaim window) was not fully root-caused, but the failure mode is clear: **duplicate claim on the same unit of work, not a file collision**. Worktree isolation would not have prevented this — the real problem is bead-level overlap, not filesystem contention.

## Decision

**Reject full per-worktree isolation.** Accept shared working directories as a deliberate design constraint and enforce it operationally through:
1. **Fleet dispatch discipline** — default to one worker per repository for build-heavy workspaces (Rust, Go, cargo build, compilation)
2. **Bead authoring guidelines** — require beads that touch the same file/function to have explicit blocking dependencies, serializing overlapping work at the bead level rather than trusting workers to notice
3. **Documentation** — CLAUDE.md already carries the prohibition ("Never create per-worker git worktrees"); this ADR normative-izes it

## Alternatives Considered

### Alternative 1: Full per-worker worktree isolation

**Approach:** `git worktree add <workspace>/.needle-worktrees/<worker-id> <branch>` per worker, dispatch agent into isolated worktree, then merge/rebase back to shared branch on completion.

**Rejected because:**

1. **Complex merge-back strategy unspecified:**
   - Rebase risks reordering commits and breaking causality
   - Fast-forward only works if workers always stay ahead of origin (false for concurrent workers)
   - Explicit merge commits create a bifurcated history that `bead_commit_index` and other tooling don't expect
   - No consensus on which approach is correct, and each has edge cases

2. **Trailer injection breakage:**
   - `commit_hook`'s `inject_bead_id_trailer` amends HEAD to add `Bead-Id:` trailers
   - In a worktree, HEAD is not the shared branch — the trailer lands on the worktree branch
   - Merging back either duplicates trailers (merge commit) or loses them (rebase/squash)
   - HOOP's `bead_commit_index` integration expects trailers on the shared branch's mainline

3. **Disk and build-cache explosion:**
   - Rust/Go worktrees duplicate `target/` and build artifacts per worker
   - Real-world impact: on EX44 with 10 workers and 30 repos, `~/.needle-worktrees/` would consume ~300 GB for workspace clones plus build caches
   - No mechanism to share build cache across worktrees without circular symlinks or shared OUT_DIR hacks

4. **Does not solve the actual failure mode:**
   - The `cg-l0v0kc` incident was duplicate claim → duplicate commits for the *same* bead, not concurrent agents editing the *same* file
   - Worktree isolation serializes file access but does not prevent two workers from claiming the same bead simultaneously (that's what the claim flock already does)
   - The real problem is bead-level decomposition: overlapping beads that touch the same function/file should block each other explicitly

5. **Operational fragility:**
   - Worktree cleanup on worker crash leaves orphaned `.git/worktrees/` entries
   - Stale worktrees accumulate; `git worktree prune` must run periodically
   - Workers dying mid-merge leave the branch in a detached/merging state that requires manual recovery

### Alternative 2: Accept constraint + operational enforcement

**Approach:** Document that concurrent workers in the same repo is unsafe, enforce one worker per repo for build-heavy workspaces, and require explicit bead dependencies for overlapping work.

**Accepted because:**

1. **Matches actual operational practice:**
   - Fleet already runs with one worker per repo for Rust workspaces (`commitgraph`, `needletail`, `kalshi-*`, etc.)
   - Multi-worker deployments target repos with light/I/O-bound work (documentation, config-only changes)
   - The failure mode (concurrent builds in shared `target/`) already exists and is mitigated by dispatch discipline, not code changes

2. **Solves the right problem at the right layer:**
   - Overlapping beads touching the same code should have explicit dependencies — this is a bead-authoring concern, not a filesystem concern
   - Workers already have `bf ready` ignore dependencies; sloppy decomposition is the only thing standing between "ready" and "actually safe to claim right now"
   - A worktree doesn't prevent two workers from both "helpfully" fixing the same bug independently; it only serializes their `git commit` calls

3. **No merge/rebase complexity:**
   - All commits land directly on the working branch
   - `commit_hook` trailer injection works as designed
   - `bead_commit_index` and downstream tooling see a linear, unbroken history

4. **Bounded resource usage:**
   - One checkout per repo, not one per worker
   - Shared build cache works as intended
   - No worktree cleanup or orphan-management burden

5. **Extant documentation + precedent:**
   - CLAUDE.md already prohibits worktrees ("Never create per-worker git worktrees")
   - The hard prohibition section is explicitly checked by `PreToolUse` hooks — this ADR brings code behavior in line with existing policy

## Consequences

### Positive

- **Simpler operational model:** One worker per repo is the default for build-heavy workspaces; exceptions require explicit justification
- **No merge-back complexity:** Commits land directly on the working branch; no worktree cleanup, no divergent history to reconcile
- **Bead-level decomposition discipline:** Overlapping work must have explicit blocking dependencies, surfacing the real design issue rather than hiding it behind filesystem serialization
- **Resource boundedness:** Disk usage and build cache size are predictable (one checkout + shared cache, not N worktrees × M repos)

### Negative

- **No structural guarantee against concurrent file edits:** Two agents dispatched concurrently in the same repo can still race on `git add`/`commit`/`reset` operations
- **Operational discipline required:** Fleet operators must pin one worker per repo for build-heavy workspaces; this is not enforced in code
- **Bead authoring burden:** Beads that touch overlapping code must explicitly declare dependencies; sloppy decomposition creates subtle races

### Migration

No code changes required. This ADR normative-izes existing practice:

1. **Fleet configuration:** Existing `--workspace` assignments already follow one-worker-per-repo for build-heavy workspaces
2. **Bead authoring:** Update CLAUDE.md to add "Give beads that touch the same file/function a real blocking dependency" to the existing work-sharing guidance
3. **Documentation:** CLAUDE.md already carries the prohibition — no change needed beyond cross-referencing this ADR

## Evidence

**Commitgraph incident (2026-08-09):**
- Bead `cg-l0v0kc` → duplicate commits `aadfdb3`, `c876058` at same second from workers `alpha`, `luna`
- Root cause: duplicate claim on same bead, not file contention
- Worktree isolation would not have prevented this (the real problem is bead-level overlap, not concurrent access)

**Current code verification (2026-08-15):**
- `src/dispatch/mod.rs:709` — no per-worker workspace derivation
- `src/commit_hook.rs:28-31` — flock exists only for trailer injection, not agent work
- CLAUDE.md — already prohibits worktrees

**Operational practice:**
- EX44 fleet runs 1 worker per Rust repo (commitgraph, needletail, kalshi-*)
- Multi-worker deployments target light/I/O repos only
- No reported file-contention incidents from concurrent `git add`/`commit` operations

## Related

- Plan.md §6.9 — original open question
- ADR-001 — Explore strand hardening (claim-aware filtering, per-worker scan rotation)
- CLAUDE.md — "NEEDLE Fleet Dispatch — no worktrees"
