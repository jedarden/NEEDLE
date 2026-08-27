# Definition of Done Adoption Guide

This guide explains how to adopt the unified Definition of Done pattern in a new repository.

## Quick Start

1. Copy the template for your language (see templates below)
2. Customize the fast/slow lane checks for your repo
3. Configure the pre-commit hook
4. (Optional) Configure NEEDLE gate
5. (Optional) Update CI to use the unified pattern

## Step-by-Step Adoption

### Step 1: Create `scripts/definition-of-done.sh`

Copy one of the language-specific templates below and customize it for your repository.

**Key sections to customize:**
- Fast lane checks (seconds, run locally under cgroup)
- Slow lane checks (tests, integration tests)
- Timeout values appropriate for your test suite
- Any repo-specific build commands

### Step 2: Configure Pre-commit Hook

Create or update `.githooks/pre-commit`:

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

echo "✅ Pre-commit checks passed"
```

Make it executable:
```bash
chmod +x .githooks/pre-commit
git config core.hooksPath .githooks
```

### Step 3: Verify It Works

Test the script directly:
```bash
# Test fast lane
./scripts/definition-of-done.sh --fast

# Test slow lane
./scripts/definition-of-done.sh --slow

# Test both lanes
./scripts/definition-of-done.sh --all
```

### Step 4: (Optional) Configure NEEDLE Gate

Add to `.needle.yaml`:

```yaml
gates:
  - type: command
    commands:
      - scripts/definition-of-done.sh --fast
```

**⚠️ IMPORTANT:** Only enable the NEEDLE gate AFTER the fast lane is reliably green. Otherwise, you'll create a fleet-wide work stoppage via failure-count quarantine.

### Step 5: (Optional) Update CI

Update your CI WorkflowTemplate's verify step to use the unified command:

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

### Step 6: Clean Existing Debt (Before Making Gates Mandatory)

Before making the pre-commit hook or NEEDLE gate mandatory:
1. Fix all formatting issues
2. Fix all test failures
3. Verify CI is green

**Only then** should you make the gate a blocker (remove `--no-verify` bypass instructions).

## Language-Specific Templates

### Rust Template

```bash
#!/usr/bin/env bash
# Unified Definition of Done for <REPO_NAME>
#
# This script is the single source of truth for "is this work acceptable?"
# It is invoked identically by:
#   - Pre-commit hook (fast lane only, with --count-bypass)
#   - CI verify step (both fast and slow lanes)
#   - NEEDLE validation gate (fast lane only)
#
# Lanes:
#   - Fast: fmt, clippy, check (seconds, run locally under cgroup)
#   - Slow: unit and integration tests
#
# Behavior: Aggregates all failures rather than aborting on first.
# Returns non-zero if ANY check fails, with all failures reported.
#
# Usage:
#   scripts/definition-of-done.sh [--fast|--slow|--all] [--count-bypass]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

LANE="fast"
COUNT_BYPASS=false

while [[ $# -gt 0 ]]; do
  case $1 in
    --fast) LANE="fast"; shift ;;
    --slow) LANE="slow"; shift ;;
    --all) LANE="all"; shift ;;
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

# Helper to reap orphaned processes after a check completes
reap_orphans() {
  local log="$1" target pid fd1
  [[ -d /proc ]] || return 0
  target="$(readlink -f "$log" 2>/dev/null)" || return 0
  [[ -n "$target" ]] || return 0

  for p in /proc/[0-9]*; do
    pid="${p#/proc/}"
    [[ "$pid" == "$$" ]] && continue
    fd1="$(readlink -f "$p/fd/1" 2>/dev/null)" || continue
    if [[ "$fd1" == "$target" ]]; then
      kill -9 "$pid" 2>/dev/null || true
      echo "  reaped orphaned pid $pid (still writing to this check's output)"
    fi
  done
}

# Helper to run a check and record failure
run_check() {
  local name="$1"
  shift
  CHECKS+=("$name")

  echo "Running: $name..."

  local log exit_code=0
  log="$(mktemp "${TMPDIR:-/tmp}/dod-check-XXXXXX.log")"

  "$@" >"$log" 2>&1 || exit_code=$?

  reap_orphans "$log"

  if [[ $exit_code -eq 0 ]]; then
    echo "✓ $name passed"
  else
    echo "✗ $name failed (exit code: $exit_code)"
    FAILURES+=("$name: exit code $exit_code")
    echo "Failure details for $name (last 100 lines):"
    tail -n 100 "$log" || true
  fi

  rm -f "$log"
  return 0
}

# Emit a marker for the NEEDLE verification gate handler
echo "NEEDLE_VERIFICATION_GATE: definition-of-done"

# Fast lane checks
if [[ "$LANE" == "fast" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Fast Lane Checks ==="
  run_check "cargo fmt --check" cargo fmt -- --check
  run_check "cargo clippy" cargo clippy --all-targets -- -D warnings
  run_check "cargo check" cargo check
fi

# Slow lane checks
if [[ "$LANE" == "slow" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Slow Lane Checks ==="

  # Build all test targets first
  run_check "cargo test --no-run (build all test targets)" \
    timeout --kill-after=30 1800 cargo test --no-run

  # Run test suites
  run_check "cargo test --lib" timeout --kill-after=30 900 cargo test --lib
  run_check "cargo test --test integration_tests" timeout --kill-after=30 900 cargo test --test integration_tests
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

### Go Template

```bash
#!/usr/bin/env bash
# Unified Definition of Done for <REPO_NAME>
#
# This script is the single source of truth for "is this work acceptable?"
# It is invoked identically by:
#   - Pre-commit hook (fast lane only, with --count-bypass)
#   - CI verify step (both fast and slow lanes)
#   - NEEDLE validation gate (fast lane only)
#
# Lanes:
#   - Fast: gofmt, go vet, go test -short (seconds, run locally)
#   - Slow: full test suite including Docker-dependent tests
#
# Behavior: Aggregates all failures rather than aborting on first.
# Returns non-zero if ANY check fails, with all failures reported.
#
# Usage:
#   scripts/definition-of-done.sh [--fast|--slow|--all] [--count-bypass]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

LANE="fast"
COUNT_BYPASS=false

while [[ $# -gt 0 ]]; do
  case $1 in
    --fast) LANE="fast"; shift ;;
    --slow) LANE="slow"; shift ;;
    --all) LANE="all"; shift ;;
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

  local log exit_code=0
  log="$(mktemp "${TMPDIR:-/tmp}/dod-check-XXXXXX.log")"

  "$@" >"$log" 2>&1 || exit_code=$?

  if [[ $exit_code -eq 0 ]]; then
    echo "✓ $name passed"
  else
    echo "✗ $name failed (exit code: $exit_code)"
    FAILURES+=("$name: exit code $exit_code")
    echo "Failure details for $name (last 100 lines):"
    tail -n 100 "$log" || true
  fi

  rm -f "$log"
  return 0
}

# Emit a marker for the NEEDLE verification gate handler
echo "NEEDLE_VERIFICATION_GATE: definition-of-done"

# Fast lane checks
if [[ "$LANE" == "fast" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Fast Lane Checks ==="

  # Check formatting
  run_check "gofmt check" bash -c \
    'gofmt -l $(git ls-files "*.go") | { grep -q . && exit 1; exit 0; }'

  # Vet and short tests
  run_check "go vet ./..." go vet ./...
  run_check "go test ./... (short)" go test ./... -short
fi

# Slow lane checks
if [[ "$LANE" == "slow" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Slow Lane Checks ==="
  run_check "go test ./... (full)" timeout 300 go test ./...
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

### TypeScript/Node Template

```bash
#!/usr/bin/env bash
# Unified Definition of Done for <REPO_NAME>
#
# This script is the single source of truth for "is this work acceptable?"
# It is invoked identically by:
#   - Pre-commit hook (fast lane only, with --count-bypass)
#   - CI verify step (both fast and slow lanes)
#   - NEEDLE validation gate (fast lane only)
#
# Lanes:
#   - Fast: prettier, eslint, tsc (seconds, run locally)
#   - Slow: jest test suite
#
# Behavior: Aggregates all failures rather than aborting on first.
# Returns non-zero if ANY check fails, with all failures reported.
#
# Usage:
#   scripts/definition-of-done.sh [--fast|--slow|--all] [--count-bypass]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

LANE="fast"
COUNT_BYPASS=false

while [[ $# -gt 0 ]]; do
  case $1 in
    --fast) LANE="fast"; shift ;;
    --slow) LANE="slow"; shift ;;
    --all) LANE="all"; shift ;;
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

  local log exit_code=0
  log="$(mktemp "${TMPDIR:-/tmp}/dod-check-XXXXXX.log")"

  "$@" >"$log" 2>&1 || exit_code=$?

  if [[ $exit_code -eq 0 ]]; then
    echo "✓ $name passed"
  else
    echo "✗ $name failed (exit code: $exit_code)"
    FAILURES+=("$name: exit code $exit_code")
    echo "Failure details for $name (last 100 lines):"
    tail -n 100 "$log" || true
  fi

  rm -f "$log"
  return 0
}

# Emit a marker for the NEEDLE verification gate handler
echo "NEEDLE_VERIFICATION_GATE: definition-of-done"

# Fast lane checks
if [[ "$LANE" == "fast" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Fast Lane Checks ==="

  # Format check
  run_check "prettier check" npx prettier --check .

  # Lint
  run_check "eslint" npx eslint .

  # Type check
  run_check "tsc" npx tsc --noEmit
fi

# Slow lane checks
if [[ "$LANE" == "slow" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Slow Lane Checks ==="
  run_check "jest tests" timeout 300 npm test
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

### Python Template

```bash
#!/usr/bin/env bash
# Unified Definition of Done for <REPO_NAME>
#
# This script is the single source of truth for "is this work acceptable?"
# It is invoked identically by:
#   - Pre-commit hook (fast lane only, with --count-bypass)
#   - CI verify step (both fast and slow lanes)
#   - NEEDLE validation gate (fast lane only)
#
# Lanes:
#   - Fast: black, ruff, mypy (seconds, run locally)
#   - Slow: pytest test suite
#
# Behavior: Aggregates all failures rather than aborting on first.
# Returns non-zero if ANY check fails, with all failures reported.
#
# Usage:
#   scripts/definition-of-done.sh [--fast|--slow|--all] [--count-bypass]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

LANE="fast"
COUNT_BYPASS=false

while [[ $# -gt 0 ]]; do
  case $1 in
    --fast) LANE="fast"; shift ;;
    --slow) LANE="slow"; shift ;;
    --all) LANE="all"; shift ;;
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

  local log exit_code=0
  log="$(mktemp "${TMPDIR:-/tmp}/dod-check-XXXXXX.log")"

  "$@" >"$log" 2>&1 || exit_code=$?

  if [[ $exit_code -eq 0 ]]; then
    echo "✓ $name passed"
  else
    echo "✗ $name failed (exit code: $exit_code)"
    FAILURES+=("$name: exit code $exit_code")
    echo "Failure details for $name (last 100 lines):"
    tail -n 100 "$log" || true
  fi

  rm -f "$log"
  return 0
}

# Emit a marker for the NEEDLE verification gate handler
echo "NEEDLE_VERIFICATION_GATE: definition-of-done"

# Fast lane checks
if [[ "$LANE" == "fast" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Fast Lane Checks ==="

  # Format check
  run_check "black check" black --check .

  # Lint
  run_check "ruff check" ruff check .

  # Type check
  run_check "mypy" mypy .
fi

# Slow lane checks
if [[ "$LANE" == "slow" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Slow Lane Checks ==="
  run_check "pytest tests" timeout 300 pytest
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

## Troubleshooting

### Script exits on first failure

Make sure you're using the `run_check` helper function, which always returns 0 after recording failures. If you run commands directly without `run_check`, `set -e` will abort on the first failure.

### Bypasses not being recorded

Check that:
1. The pre-commit hook is calling the script with `--count-bypass`
2. The `.beads` directory is writable
3. You're not using `git commit --no-verify` (which bypasses the hook entirely)

### Tests timing out

Adjust the timeout values in the slow lane checks. The template uses `timeout --kill-after=30` to send SIGKILL 30 seconds after the initial timeout signal.

### NEEDLE gate not firing

Check that:
1. The script emits `NEEDLE_VERIFICATION_GATE: definition-of-done` on stdout
2. The `.needle.yaml` gates section points to the correct script path
3. The script is executable (`chmod +x scripts/definition-of-done.sh`)

## Rollout Checklist

Before making gates mandatory in your repo:

- [ ] Script created at `scripts/definition-of-done.sh`
- [ ] Script executable (`chmod +x`)
- [ ] Pre-commit hook configured in `.githooks/pre-commit`
- [ ] `git config core.hooksPath .githooks` set
- [ ] Bypass logging working (check `.beads/bypasses.jsonl`)
- [ ] Fast lane checks pass reliably
- [ ] Slow lane checks pass in CI
- [ ] All existing formatting debt cleaned
- [ ] All existing test failures fixed
- [ ] CI WorkflowTemplate updated to use `--all`
- [ ] NEEDLE gate configured (optional, only if fast lane is green)
- [ ] Documentation updated for your repo

## References

- Full pattern documentation: `docs/definition-of-done-pattern.md`
- Reference implementation: `scripts/definition-of-done.sh` (NEEDLE repo)
- Parent bead: needle-d1b2ee0d
