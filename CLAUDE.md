# NEEDLE Project Conventions

## Overview

NEEDLE (Navigates Every Enqueued Deliverable, Logs Effort) is a Rust bead worker
binary. It automates bead processing by running the `bf` CLI (bead-forge) to
select, claim, dispatch an AI agent, and handle outcomes.

## MSRV

Minimum Supported Rust Version: **1.75** (2023-12-28).

Pinned in `rust-toolchain.toml`. Do not add dependencies that require a newer
Rust edition without updating MSRV and rust-toolchain.toml.

## Module Dependency Graph

```
cli
 └─ worker
     ├─ strand ─ bead_store ─ types
     ├─ claim  ─ bead_store, telemetry, types
     ├─ prompt ─ config, types
     ├─ dispatch ─ config, telemetry, types
     ├─ outcome  ─ bead_store, config, telemetry, types
     ├─ health   ─ config, telemetry, types
     ├─ bead_store ─ types
     ├─ telemetry  ─ types
     └─ config     ─ types
```

Leaf modules (no internal deps): `types`, `config`, `telemetry`, `bead_store`, `health`.

## Code Style

- No `unwrap()` or `expect()` in non-test code — use `?` with `anyhow`.
- All public functions return `Result<T>`.
- Telemetry must be emitted at every state transition and outcome.
- Match arms must be exhaustive — no catch-all `_` on outcome enums.
- Run `cargo clippy --all-targets -- -D warnings` before committing.
- Run `cargo fmt` before committing.

## Testing

- Unit tests live in `#[cfg(test)]` modules in each source file.
- Integration tests live in `tests/`.
- Do not use `tokio_test::block_on` — use `#[tokio::test]`.
- Test the public interface, not internals.

### Test Isolation Policy

**CRITICAL:** Any integration test that spawns the compiled `needle` binary as a real subprocess via `Command::new(CARGO_BIN_EXE_needle)` MUST isolate both `$HOME` and the Explore strand's scan root.

The Explore strand (enabled by default via `ExploreConfig::default_enabled()`) scans `workspace_root` (defaulting to `$HOME`) for bead workspaces. Without isolation, a test's spawned binary will leak into the real user environment and scan real repos, contaminating both the test and production bead stores.

**📖 Comprehensive isolation patterns documentation:** See `docs/testing-isolation-patterns.md` for detailed coverage of all 4 isolation patterns, decision trees, anti-patterns, and code examples. This policy is the summary; the full documentation includes when to use each pattern and common mistakes.

**Required isolation for subprocess tests:**

```rust
// Always set HOME to the test's tempdir
cmd.env("HOME", temp_dir.path())

// Optionally, disable Explore entirely via config if the test doesn't need it
// (prefer HOME isolation — it's more realistic and catches more bugs)
```

**Required isolation for in-process tests:**

Setting `HOME` only affects child processes. A test that builds a `Worker`
**in-process** (e.g. via a `test_config()` helper) never spawns one, so it
inherits `ExploreConfig::default()` — `workspaces: []` (auto-discover) with
`workspace_root` from `default_workspace_root()` -> `dirs_or_home("")`, the
real home directory. Pin the scan root explicitly:

```rust
config.strands.explore.workspace_root = temp_home.to_path_buf();
config.strands.explore.workspaces = Vec::new();
```

**Rationale:** This policy exists due to the 2026-07-20 contamination incident, where a non-isolated test created ~284 phantom beads across ~22 repos under fixture worker identifiers. See ADR-006 for full postmortem.

The in-process clause was added after 2026-08-05, when `test_config()` in
`tests/integration_tests.rs` — which isolated `workspace.default` and
`workspace.home` but not `strands.explore` — let an orphaned local
`integration_tests` binary roam into bead-forge's live store, mutate beads to
`in_progress` under assignee `echo-test-test-worker`, and truncate
`.beads/issues.jsonl` to 0 bytes (2302 beads, recovered from git). The
subprocess clause above was already in force and did not cover this shape.

**Do not run `cargo test` locally.** Tests run on iad-ci automatically when you push to `main`. A GitHub webhook triggers the `needle-ci` WorkflowTemplate on iad-ci.

After pushing, poll for the triggered workflow and wait for it to complete:

```bash
# Record push time, then poll for the triggered workflow
PUSH_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ)
git push origin main

# Wait up to 2 min for the workflow to appear, then poll until done
WF=""
for i in $(seq 24); do
  sleep 5
  WF=$(kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
    get workflows -n argo-workflows \
    --sort-by=.metadata.creationTimestamp \
    -o jsonpath='{range .items[*]}{.metadata.name} {.metadata.creationTimestamp}{"\n"}{end}' \
    2>/dev/null | awk -v t="$PUSH_TIME" '$2 >= t && /^needle-ci-/ {print $1}' | tail -1)
  [[ -n "$WF" ]] && break
done
echo "Workflow: $WF"

# Poll until complete
for i in $(seq 60); do
  PHASE=$(kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
    get workflow "$WF" -n argo-workflows -o jsonpath='{.status.phase}' 2>/dev/null)
  echo "[$i] $WF phase=$PHASE"
  if [[ "$PHASE" == "Succeeded" || "$PHASE" == "Failed" || "$PHASE" == "Error" ]]; then break; fi
  sleep 30
done

# On failure, stream the pod log (pods are deleted on completion — act fast)
if [[ "$PHASE" != "Succeeded" ]]; then
  POD=$(kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
    get pods -n argo-workflows -l workflows.argoproj.io/workflow="$WF" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)
  kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
    logs -n argo-workflows "$POD" -c main 2>/dev/null \
    || echo "Pod already deleted — check Argo UI at https://argo-ci.ardenone.com (logs kept 2h on failure)"
fi
```

If CI fails, add the log output as a note to the bead and do **not** close it. Fix the issue and push again.

## Commit Convention

```
feat(needle-XYZ): short description
fix(needle-XYZ): short description
test(needle-XYZ): short description
```

## Bead Workflow

Beads are managed with the `bf` CLI (bead-forge). `br` is a deprecated alias
that survives only as a shim on some hosts — never invoke it, and never emit it
from a prompt template or doc. Each bead's body contains deliverables and
acceptance criteria. Close beads with:

```bash
bf close BEAD_ID --reason "Summary of what was done"
```

Note `--reason`, not `--body`: `--body` is not a valid flag and the close will
fail.

### Reopening Beads

When a closed bead needs to be revisited, use `bead reopen`:

```bash
bead reopen BEAD_ID
```

**Critical behavior:** `bead reopen` **clears the assignee**, making the bead
immediately claimable by any worker. This is intentional design (see ADR-018)
to prevent silent starvation where a reopened bead with a stale assignee would
become permanently unclaimable.

The reopen contract:
- Transitions status from `closed` to `open`
- Clears `assignee` (making the bead claimable)
- Clears `closed_at` and `close_reason`
- Clears `manual_blocked` flag
- Preserves full audit trail (previous assignee remains visible in history)

If you need a specific worker to continue a reopened bead, manually reassign
it after reopening with `bead update <id> --assignee <worker>`.

## Capabilities Negotiation

NEEDLE uses the `bead capabilities --profile native-v1` command to verify backend
compatibility at workspace-open time. This is **mandatory** for bead-rs backends.

**Required contract:** The command must return JSON with:
- `implementation: "bead-rs"` — backend identity verification
- `atomic_claim: true` — asserts atomic claim operations (critical for multi-worker safety)
- `statuses: ["open", "in_progress", "deferred", "closed"]` — all four required
- `schemas: [...]` — all three required URNs (issue, event, field-guide)

**Failure mode:** Missing or invalid capabilities cause workspace open to fail
closed. This is intentional — capability gaps can silently break safety guarantees.

**See:** `docs/capabilities-negotiation.md` for the full contract, validation logic,
and backend capability descriptors.

## Checkpoint Commits

When committing checkpoint changes (anything under `.beads/checkpoint/`), you
MUST commit the active root objects referenced by `current.json` and
`previous.json` alongside the pointer files. Use the provided script:

```bash
./scripts/commit-checkpoint.sh "chore: checkpoint commit with active root objects"
```

This ensures a fresh clone can verify checkpoint integrity. Never manually run
`git add .beads/checkpoint/` — it will either miss the active roots (broken
verification) or accumulate superseded objects (bloat). See
`docs/checkpoint-tracking.md` for details.
