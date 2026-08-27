# Unified Definition of Done Pattern

## Problem

The same question — "is this work acceptable?" — was answered in four disconnected places that drifted apart:

1. **Pre-commit hook** — Bypassable, and bypassed: 23 commits landed with `--no-verify` while failing
2. **CI verify step** — Async and advisory; red for 6+ consecutive runs without anyone noticing
3. **NEEDLE gate** — Only configured in 5 of 71 workspaces; checks only that a commit exists
4. **Agent itself** — Whatever the model decides "done" means

Consequence: An agent can satisfy (4), be accepted by NEEDLE because (3) is unset, land because (1) was skipped, and only later contradict (2) — which nobody reads.

## Solution

**One declared command per repo** — the definition of done — invoked identically by the pre-commit hook, CI verify step, and NEEDLE validation gate. This ensures a single source of truth for "is this work acceptable?" and makes drift between surfaces impossible by construction.

## Design Principles

### 1. Split by Cost, Not Tool

- **Fast lane** (seconds, run locally under cgroup): `fmt`, `clippy`, `check`, `go vet`, `go test -short`
- **Slow lane** (tests): Full test suite, integration tests with Docker

The agent-facing gate runs the fast lane; CI runs both lanes.

**Rationale:** `cargo test` is intercepted on ex44/lab and submitted to iad-ci when the tree is clean, so it is NOT fast feedback. The fast lane must be truly local.

### 2. Aggregate, Never Abort on First Failure

Under `set -e`, an agent fixes fmt, gets re-dispatched, discovers clippy, gets re-dispatched again — one wasted cycle per check. Collect all failures into one report so a dispatch learns everything at once.

**Implementation:**

```bash
declare -a FAILURES=()

run_check() {
  local name="$1"
  shift
  CHECKS+=("$name")

  if "$@" >/dev/null 2>&1; then
    echo "✓ $name passed"
  else
    local exit_code=$?
    echo "✗ $name failed (exit code: $exit_code)"
    FAILURES+=("$name: exit code $exit_code")
  fi
  return 0  # Keep running
}

# Summary at end
if [[ ${#FAILURES[@]} -gt 0 ]]; then
  echo "Failed checks:"
  for failure in "${FAILURES[@]}"; do
    echo "  - $failure"
  done
  exit 1
fi
```

### 3. Do NOT Gate on Failing Checks

Turning on a blocking gate before existing debt is cleaned converts a formatting problem into a fleet-wide work stoppage via failure-count quarantine. **Sequence: clean the debt, then wire the gate.**

**Technical debt tracking:**
- `needle-3653fee9`: NEEDLE has 52 fmt diffs dirty across 7 committed files
- `commitgr-44a76623`: commitgraph go test is failing

### 4. Count Bypasses, Don't Just Allow Them

An invisible bypass is indistinguishable from no gate. Record when the declared command is skipped.

**Implementation:**

```bash
# In pre-commit hook
if ! scripts/definition-of-done.sh --fast --count-bypass; then
  echo "To bypass (not recommended), use: git commit --no-verify"
  echo "This will be recorded in .beads/bypasses.jsonl"
  exit 1
fi

# In definition-of-done.sh
BYPASS_LOG="${REPO_ROOT}/.beads/bypasses.jsonl"
if [[ "$COUNT_BYPASS" == "true" ]]; then
  mkdir -p "$(dirname "$BYPASS_LOG")"
  echo "{\"timestamp\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"lane\":\"$LANE\",\"pwd\":\"$(pwd -P)\"}" >> "$BYPASS_LOG"
fi
```

### 5. Emit a Gate Marker

NEEDLE's validation gate handler needs to detect which verification system a repo is using. Emit a marker:

```bash
echo "NEEDLE_VERIFICATION_GATE: definition-of-done"
```

## Script Structure

```bash
#!/usr/bin/env bash
# Unified Definition of Done for <repo>
#
# This script is the single source of truth for "is this work acceptable?"
# It is invoked identically by:
#   - Pre-commit hook (fast lane only, with --count-bypass)
#   - CI verify step (both fast and slow lanes)
#   - NEEDLE validation gate (fast lane only)
#
# Lanes:
#   - Fast: <describe fast checks - seconds, run locally under cgroup>
#   - Slow: <describe slow checks - tests, integration>
#
# Behavior: Aggregates all failures rather than aborting on first.
#
# Usage:
#   scripts/definition-of-done.sh [--fast|--slow|--all] [--count-bypass]

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Default to fast lane
LANE="fast"
COUNT_BYPASS=false

# Parse arguments
while [[ $# -gt 0 ]]; do
  case $1 in
    --fast|--slow|--all) LANE="${1#--}"; shift ;;
    --count-bypass) COUNT_BYPASS=true; shift ;;
    *) echo "Error: Unknown argument: $1" >&2; exit 1 ;;
  esac
done

# Bypass counting
BYPASS_LOG="${REPO_ROOT}/.beads/bypasses.jsonl"
if [[ "$COUNT_BYPASS" == "true" ]]; then
  mkdir -p "$(dirname "$BYPASS_LOG")"
  echo "{\"timestamp\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"lane\":\"$LANE\",\"pwd\":\"$(pwd -P)\"}" >> "$BYPASS_LOG"
fi

# Failure tracking
declare -a FAILURES=()
declare -a CHECKS=()

# Helper to run a check and record failure
run_check() {
  local name="$1"
  shift
  CHECKS+=("$name")

  echo "Running: $name..."

  if output=$("$@" 2>&1); then
    echo "✓ $name passed"
    return 0
  else
    local exit_code=$?
    echo "✗ $name failed (exit code: $exit_code)"
    FAILURES+=("$name: exit code $exit_code")
    echo "Failure details for $name (last 100 lines):"
    echo "$output" | tail -n 100
    return 0
  fi
}

# Emit a marker for the NEEDLE verification gate handler
echo "NEEDLE_VERIFICATION_GATE: definition-of-done"

# Fast lane checks
if [[ "$LANE" == "fast" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Fast Lane Checks ==="
  run_check "check 1" command1 args
  run_check "check 2" command2 args
fi

# Slow lane checks
if [[ "$LANE" == "slow" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Slow Lane Checks ==="
  run_check "test suite" timeout 300 command args
fi

# Summary report
echo ""
echo "=== Definition of Done Summary ==="
echo "Lane: $LANE"
echo "Checks run: ${#CHECKS[@]}"
echo "Failures: ${#FAILURES[@]}"

if [[ ${#FAILURES[@]} -gt 0 ]]; then
  echo ""
  echo "Failed checks:"
  for failure in "${FAILURES[@]}"; do
    echo "  - $failure"
  done
  echo ""
  echo "❌ Definition of NOT done"
  exit 1
else
  echo "✓ Definition of Done"
  exit 0
fi
```

## Integration Points

### 1. Pre-commit Hook (`.githooks/pre-commit`)

```bash
#!/usr/bin/env bash
# Pre-commit hook: Definition of Done (fast lane) + checkpoint verification

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

echo "=== Running Definition of Done (fast lane) ==="

if ! scripts/definition-of-done.sh --fast --count-bypass; then
  echo ""
  echo "❌ Pre-commit check failed"
  echo ""
  echo "Your changes do not meet the Definition of Done."
  echo "Please fix the issues above before committing."
  echo ""
  echo "To bypass (not recommended), use: git commit --no-verify"
  echo "This will be recorded in .beads/bypasses.jsonl"
  exit 1
fi

# Optional: checkpoint verification for NEEDLE
echo "=== Running checkpoint verification ==="
"$repo_root/scripts/checkpoint-publish.sh" verify-index
```

### 2. NEEDLE Gate (`.needle.yaml`)

```yaml
gates:
  - type: command
    commands:
      - scripts/definition-of-done.sh --fast
```

### 3. CI Verify Step (WorkflowTemplate)

```yaml
spec:
  templates:
  - name: verify
    container:
      args:
      - |
        set -ex
        git clone --depth 1 --branch main "https://git.ardenone.com/jedarden/<repo>.git" /workspace
        cd /workspace

        echo "=== Running Definition of Done (all lanes) ==="
        ./scripts/definition-of-done.sh --all

        echo "Verify passed"
```

## Language-Specific Fast Lanes

### Rust (NEEDLE)

```bash
# Fast lane
run_check "cargo fmt --check" cargo fmt -- --check
run_check "cargo clippy" cargo clippy --all-targets -- -D warnings
run_check "cargo check" cargo check

# Slow lane
run_check "cargo test --no-run (build all test targets)" timeout --kill-after=30 1800 cargo test --no-run --lib --test integration_tests
run_check "cargo test --lib" timeout --kill-after=30 900 cargo test --lib
run_check "cargo test --test integration_tests" timeout --kill-after=30 900 cargo test --test integration_tests
```

**Key considerations:**
- Use `timeout --kill-after=30` to prevent hung tests from blocking CI forever
- Build all test targets FIRST with `cargo test --no-run` to restore timeout caps to test execution bounds (not compile+run)
- Install pinned bead-rs CLI in CI if needed by P2/P3 strand fixtures

### Go (commitgraph)

```bash
# Fast lane
run_check "go vet ./..." go vet ./...
run_check "go test ./... (short)" go test ./... -short

# Slow lane
run_check "go test ./... (full)" timeout 300 go test ./...
```

**Key considerations:**
- Use `-short` flag in fast lane to skip integration tests that require Docker
- Full test suite in slow lane includes Docker-dependent tests

### Go (SEAM)

```bash
# Fast lane
run_check "gofmt check" gofmt -l $(git ls-files '*.go')
run_check "go vet ./..." go vet ./...
run_check "go test ./... (short)" go test ./... -short

# Slow lane
run_check "go test ./... (full)" timeout 300 go test ./...
```

## Rollout Strategy

### Phase 1: Implement the Pattern

1. Create `scripts/definition-of-done.sh` with language-appropriate checks
2. Create/update `.githooks/pre-commit` to invoke it with `--fast --count-bypass`
3. Document the pattern in `docs/definition-of-done-pattern.md`

### Phase 2: Wire CI

1. Update the workflow template's verify step to run `./scripts/definition-of-done.sh --all`
2. Verify CI passes consistently

### Phase 3: Wire NEEDLE Gate (Optional)

1. Add `gates:` section to `.needle.yaml`:
   ```yaml
   gates:
     - type: command
       commands:
         - scripts/definition-of-done.sh --fast
   ```
2. Only do this AFTER the fast lane is reliably green

### Phase 4: Clean Technical Debt

Before making gates mandatory, clean existing debt:
- Fix all formatting issues (`cargo fmt`, `gofmt -w`)
- Fix all test failures
- Verify CI is green

**Only then** enable the gate as a blocker.

## Current Implementation Status

| Repo   | Script | Pre-commit | CI Verify | NEEDLE Gate | Technical Debt |
|--------|--------|------------|-----------|-------------|-----------------|
| NEEDLE | ✅     | ✅         | ✅        | ✅          | needle-3653fee9 (52 fmt diffs) |
| commitgraph | ✅ | ✅      | ❌ (old separate checks) | ❌ | commitgr-44a76623 (go test failing) |
| SEAM   | ❌     | ❌ (old gofmt-only) | ❌ | ❌ | Unknown |

## Related Work

- `needle-3386daef`: Fixes WHERE acceptance authority lives (verification, not exit code)
- `needle-3653fee9`: Tracks NEEDLE formatting debt
- `commitgr-44a76623`: Tracks commitgraph test debt

This bead fixes WHAT is verified and ensures every surface asks the same question.
