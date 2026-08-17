# Definition of Done: Unified Verification System

## Overview

This document describes the unified "definition of done" system for NEEDLE and related repositories. It provides a single source of truth for "is this work acceptable?" that is invoked identically by:

- **Pre-commit hook** (fast lane only)
- **CI verify step** (both fast and slow lanes)
- **NEEDLE validation gate** (fast lane only)

## The Single Command

Every repo using this system declares its definition of done in one place:

```bash
scripts/definition-of-done.sh [--fast|--slow|--all] [--count-bypass]
```

### Lanes

The definition of done is split by **cost**, not by tool:

**Fast lane** (seconds, runs locally under cgroup):
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo check`

**Slow lane** (tests, submitted to iad-ci when tree is clean):
- `cargo test --lib`
- `cargo test --test integration_tests` (or representative sample)

### Invocation Points

| Invocation Point | Lane | Bypass Counting | Behavior |
|-----------------|------|-----------------|----------|
| Pre-commit hook | `--fast` | `--count-bypass` | Blocks commit on failure |
| CI verify step | `--all` | No | Aggregates failures, reports all |
| NEEDLE gate | `--fast` | No | Reopens bead on failure |

### Key Behavior

**Aggregates failures, does NOT abort on first:**

Under `set -e`, an agent fixes fmt, gets re-dispatched, discovers clippy, gets re-dispatched again — one wasted cycle per check. The unified command collects ALL failures into one report so a dispatch learns everything at once.

**Bypasses are recorded:**

Every `--no-verify` commit (pre-commit bypass) is logged to `.beads/bypasses.jsonl` with timestamp, lane, and working directory. An invisible bypass is indistinguishable from no gate.

## Existing Debt: Sequencing Required

The rollout of this system must wait for existing quality debt to be resolved. A blocking gate cannot be enabled on a repo that currently fails it — that converts a formatting problem into a fleet-wide work stoppage via failure-count quarantine.

### NEEDLE Debt

**Bead:** `needle-3653fee9` (52 fmt diffs dirty across 7 committed files)

The definition-of-done fast lane will fail on NEEDLE's current main branch until these formatting issues are resolved.

**Resolution sequence:**
1. Fix all formatting issues (`cargo fmt`)
2. Commit the fixes
3. Enable the unified pre-commit hook
4. Wire the NEEDLE gate to `definition-of-done.sh --fast`

### commitgraph Debt

**Bead:** `commitgr-44a76623` (go test failing)

The definition-of-done equivalent for Go repos will include `go test ./...`, which currently fails.

**Resolution sequence:**
1. Fix the failing test(s)
2. Commit the fixes
3. Enable the unified pre-commit hook
4. Wire the NEEDLE gate to the Go equivalent

## Rollout Plan

### Phase 1: Infrastructure (This work)

- [x] Create `scripts/definition-of-done.sh` with fast/slow lane separation
- [x] Update pre-commit hook to invoke fast lane with bypass counting
- [x] Update NEEDLE's own `.needle.yaml` gate to use fast lane
- [ ] Update CI workflow template to invoke both lanes
- [ ] Document sequencing requirements

### Phase 2: Debt Resolution

- [ ] Resolve `needle-3653fee9` (fmt issues)
- [ ] Resolve `commitgr-44a76623` (go test failures)

### Phase 3: Activation

Once debt is resolved:
- [ ] Enable pre-commit hook (remove `--no-verify` from existing workflow)
- [ ] Confirm CI green with new verify step
- [ ] Confirm NEEDLE gate runs successfully on closed beads

### Phase 4: Extension to Other Repos

After proving the system on NEEDLE:
- [ ] Implement Go equivalent for commitgraph
- [ ] Implement equivalent for other repos in the fleet
- [ ] Update CI templates for each language/toolchain

## Related Work

- **`needle-3386daef`**: Fixes WHERE acceptance authority lives (verification, not exit code)
- **This bead (`needle-d1b2ee0d`)**: Fixes WHAT is verified and ensures every surface asks the same question

Neither subsumes the other; both are required for a complete solution.

## Migration Notes

### From `verify-shipped-commit.sh`

The old gate only checked `cargo check --all-targets`. The new fast lane adds:
- `cargo fmt --check` (formatting)
- `cargo clippy --all-targets -- -D warnings` (linting)

This means commits that would have passed the old gate will now fail if they have formatting or linting issues. This is intentional — the old gate was too permissive.

### From Legacy `verification:` Config

The legacy `verification:` key in `.needle.yaml` is deprecated in favor of `gates:`. The new system uses `gates:` with a single command that is the definition of done.

## Verification

After rollout, verify the system is working:

```bash
# Test the command directly
./scripts/definition-of-done.sh --fast
./scripts/definition-of-done.sh --slow
./scripts/definition-of-done.sh --all

# Test pre-commit hook
git commit --allow-empty -m "test pre-commit"  # Should fail if fast lane fails
git commit --allow-empty --no-verify -m "bypass test"  # Check bypass log

# Test NEEDLE gate (via bead closure)
bf close <test-bead> --reason "test gate"
# Check that gate ran and accepted/rejected the commit
```

## Bypass Analysis

Monitor `.beads/bypasses.jsonl` to understand:
- How often quality gates are being bypassed
- Which workers/users are bypassing
- Whether bypasses correlate with subsequent failures

```bash
# Count bypasses
wc -l .beads/bypasses.jsonl

# Analyze bypass patterns
jq -r '.lane' .beads/bypasses.jsonl | sort | uniq -c
jq -r '.pwd' .beads/bypasses.jsonl | sort | uniq -c
```
