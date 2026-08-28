# ADR-020: Verification Gates Judge Committed State

## Status

Accepted (2026-08-28)

## Context

NEEDLE's verification gates run after an agent exits successfully (exit code 0) to verify work before accepting bead closure. Prior to this change, gates ran directly in the shared workspace checkout, which could contain uncommitted changes.

This created a subtle failure mode: a gate could pass against uncommitted files, but the same command would fail on a fresh clone or another worker checking out the same commit. This violated the principle that verification should judge durable, committed artifacts.

### Prior Behavior and Risks

1. **Shared checkout contamination**: Multiple workers sharing a workspace could see each other's uncommitted changes
2. **False positives**: Gates passing locally but failing in CI or on other machines
3. **Uncommitted dependencies**: Code referencing symbols/functions only present in uncommitted files
4. **No-worktree policy violation**: ADR-015 explicitly rejected per-worker worktrees, so we needed a different approach to isolation

### The ARMOR Incident (Real Example)

ARMOR experienced a "commit-storm" where a worker repeatedly committed trivial doc files to satisfy a bare "must have a commit" rule, each triggering paired CI version-bump commits. The underlying issue was verification that accepted uncommitted state as sufficient, incentivizing workers to game the system rather than shipping real work.

## Decision

**Verification gates MUST judge committed git state, not the working tree.**

### Implementation

1. **Clean extraction**: Before running gates, NEEDLE extracts HEAD using `git archive HEAD | tar -x -C <tmp>` into a per-dispatch temp directory
2. **Execution modes**: `GateConfig::Command` gains a `run_in` field:
   - `clean` (default): Run in the extracted committed state
   - `workspace`: Run in the shared checkout (for gates that must see uncommitted state)
3. **Lifecycle**: Temp directories are removed on success, retained on failure for diagnosis
4. **Shipped-work check**: Already operates on git commits only (no extraction needed)

### Configuration Example

```yaml
gates:
  - type: command
    commands:
      - cargo test
      - cargo clippy -- -D warnings
    run_in: clean  # Default, can be omitted

  - type: command
    commands:
      - make build-check
    run_in: workspace  # For gates that must see build cache
```

## Rationale

### Why Committed State

1. **Reproducibility**: Committed state is the only durable artifact that replicates across environments
2. **CI parity**: Gates should pass/fail the same in NEEDLE as they would in CI
3. **Shared checkout safety**: Multiple workers can share a workspace without interfering with each other's verification
4. **Fresh clone simulation**: Clean extraction approximates what a fresh clone would see

### Why Not Worktrees (ADR-015 Revisited)

ADR-015 rejected per-worker git worktrees for:
- Disk and build-cache explosion
- Merge-back complexity
- Bead-level serialization being the real fix

Clean extraction addresses the same problem without worktree overhead:
- Lightweight temp directories (deleted on success)
- No git management overhead
- Simple, clear lifecycle

### The `run_in` Escape Hatch

Some gates genuinely need to see uncommitted state:
- Build-cache validation (cache files are never committed)
- Local environment checks
- Integration tests that need running services

The `run_in: workspace` mode preserves this capability while making clean execution the explicit default.

## Consequences

### Positive

1. **Reliable verification**: Gates now judge what actually gets committed and pushed
2. **No false positives**: A passing gate means the code would work on any machine
3. **Shared checkout safe**: Multiple workers can use the same workspace without verification interference
4. **Clear failure diagnosis**: Failed gates retain their extraction for inspection

### Negative

1. **Overhead**: Each verification requires git archive + tar extraction (~100-500ms depending on repo size)
2. **Temporary space**: Failed gates leave behind temp directories until manual cleanup
3. **Configuration complexity**: Users must understand `run_in` modes (though `clean` is the sensible default)

### Mitigations

- Extraction happens in parallel with other worker tasks (async I/O)
- Temp directories use worker scratch (`NEEDLE_SCRATCH` env) rather than shared spaces
- Clear documentation and sensible defaults reduce configuration burden

## Implementation Details

### Extraction Process

```bash
git archive HEAD | tar -x -C /tmp/needle-clean-<bead-id>
```

The extraction directory is named with the bead ID for easy identification during failure diagnosis.

### Gate Execution

```rust
let execution_dir = if run_in == RunIn::Clean {
    Some(extract_committed_state(workspace, &bead.id).await?)
} else {
    None
};

let run_dir = execution_dir.as_ref().unwrap_or(workspace);
// Run commands in run_dir
```

### Failure Handling

When a clean-extraction gate fails that would have passed in workspace mode:
1. The extraction is retained (not deleted)
2. The bead gets label `uncommitted-dependency` (planned enhancement)
3. The reopen reason includes the workspace diff for context

### Shipped-Work Check

This check already operates on git commits only:
- `git rev-parse HEAD` (committed HEAD)
- `git diff <pre_sha> <head>` (comparing commits)
- `git merge-base --is-ancestor <head> @{u}` (checking ancestry)

No extraction needed—it's already commit-aware.

## Alternatives Considered

### 1. Worktrees (Rejected)

See ADR-015 for full analysis. TL;DR: Worktrees add overhead without solving the real problem (bead-level serialization).

### 2. Sticky Temporary Worktrees (Rejected)

Similar to worktrees but reused across dispatches. Rejected because:
- State drift between worktree and origin
- Complex lifecycle management
- Still requires disk and build-cache overhead

### 3. Commit-Based Verification Only (Rejected)

Run gates only after commits are pushed. Rejected because:
- Delays feedback loop (must wait for push)
- Doesn't catch "would fail on commit" cases early
- Requires every agent to push for verification

### 4. Do Nothing (Rejected)

Continue running gates in workspace. Rejected because:
- False positives persist
- Shared checkout contamination
- Violates reproducibility principle

## Future Enhancements

### Uncommitted-Dependency Detection

When a clean-extraction gate fails but workspace mode passes:
1. Compute `git diff HEAD` to identify uncommitted changes
2. Add label `uncommitted-dependency` with the diff summary
3. Include workspace diff in reopen reason

This makes the shared-checkout failure mode (which ADR-015 accepts) detectable and actionable.

### Configurable Extraction Methods

Support alternatives to `git archive`:
- `git clone --no-local <repo> <temp>` (slower but more robust)
- `rsync` with exclusions (for non-git workspaces)

### Shipped-Work Check in Clean Mode

While currently unnecessary (it's already commit-aware), the shipped-work check could optionally run in clean extraction for consistency with other gates.

## References

- ADR-015: Concurrent Same-Repo Worker Isolation (no-worktrees policy)
- ADR-006: Bead Lifecycle Reliability (predispatch snapshots)
- GitHub issue jedarden/NEEDLE#9: Configurable stderr cap for gates
- ARMOR commit-storm incident (docs/notes on ARMOR's commit-storm)
