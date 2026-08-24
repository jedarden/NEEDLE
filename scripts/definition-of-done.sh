#!/usr/bin/env bash
# Unified Definition of Done for NEEDLE
#
# This script is the single source of truth for "is this work acceptable?"
# It is invoked identically by:
#   - Pre-commit hook (fast lane only, with --count-bypass)
#   - CI verify step (both fast and slow lanes)
#   - NEEDLE validation gate (fast lane only)
#
# Lanes:
#   - Fast: fmt, clippy, check (seconds, run locally under cgroup)
#   - Slow: unit and core strand integration targets
#
# Behavior: Aggregates all failures rather than aborting on first.
# Returns non-zero if ANY check fails, with all failures reported.
#
# Usage:
#   scripts/definition-of-done.sh [--fast|--slow|--all] [--count-bypass]
#
# Flags:
#   --fast          Run fast lane only (default for NEEDLE gate)
#   --slow          Run slow lane only (tests)
#   --all           Run both lanes (default for CI)
#   --count-bypass   Record invocation to bypass log (for pre-commit hook)

set -euo pipefail

# Script directory for path resolution
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Default to fast lane
LANE="fast"
COUNT_BYPASS=false

# Parse arguments
while [[ $# -gt 0 ]]; do
  case $1 in
    --fast)
      LANE="fast"
      shift
      ;;
    --slow)
      LANE="slow"
      shift
      ;;
    --all)
      LANE="all"
      shift
      ;;
    --count-bypass)
      COUNT_BYPASS=true
      shift
      ;;
    *)
      echo "Error: Unknown argument: $1" >&2
      echo "Usage: $0 [--fast|--slow|--all] [--count-bypass]" >&2
      exit 1
      ;;
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
# Kill anything the finished check left behind. A leftover inherits the check's
# stdout, so a process whose fd 1 still points at this check's log is by
# definition one of its orphans -- which makes this precise: it cannot match an
# unrelated process, another check, or a concurrent CI run (each gets its own
# mktemp log).
reap_orphans() {
  local log="$1" target pid fd1
  [[ -d /proc ]] || return 0          # /proc scan is Linux-only; no-op elsewhere
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

run_check() {
  local name="$1"
  shift
  CHECKS+=("$name")

  echo "Running: $name..."

  # Capture to a FILE, never a command substitution.
  #
  # `output=$(cmd)` reads the pipe until every WRITER closes it, not until the
  # command exits. `timeout N cargo test ...` signals the process group, but a
  # test binary that installs a SIGTERM handler (NEEDLE's worker does, for
  # graceful shutdown) survives it, is reparented to init, and keeps fd 1/2 on
  # that pipe. The read then never returns: the timeout has fired, cargo is
  # gone, and the check still hangs -- so the per-target cap is inert and the
  # step runs until the pod's activeDeadlineSeconds and is SIGKILLed with no
  # output at all. That surfaces as "Pod was active on the node longer than the
  # specified deadline", which reads as a slow suite rather than a hung test.
  #
  # Observed in needle-ci 2026-08-24: cargo test --lib's 900s cap fired, both
  # `timeout` and `cargo` exited, and the orphaned test binary still showed
  # fd 1 -> pipe:[49593974] while idling at 15m CPU and holding 2.6Gi. Roughly
  # 36 consecutive runs over 35h reported only the deadline message.
  #
  # Writing to a file removes the dependency on writers closing: the check
  # returns as soon as the command itself exits.
  local log exit_code=0
  log="$(mktemp "${TMPDIR:-/tmp}/dod-check-XXXXXX.log")"

  "$@" >"$log" 2>&1 || exit_code=$?

  # An orphan idles at ~0% CPU but holds its memory; five leaking test targets
  # would exhaust the verify container's 5Gi on their own.
  reap_orphans "$log"

  if [[ $exit_code -eq 0 ]]; then
    echo "✓ $name passed"
  else
    echo "✗ $name failed (exit code: $exit_code)"
    FAILURES+=("$name: exit code $exit_code")
    # Show the tail here while retaining the named failure for the summary.
    # 100 lines covers cargo test's full alphabetical failures list; a 20-line
    # tail truncated the head of that list, hiding which modules failed.
    echo "Failure details for $name (last 100 lines):"
    tail -n 100 "$log" || true
  fi

  rm -f "$log"
  # Keep running so every check reports its result. The summary below returns
  # the aggregate status after all requested checks have run.
  return 0
}

# Emit a marker for the NEEDLE verification gate handler
echo "NEEDLE_VERIFICATION_GATE: definition-of-done"

# Fast lane checks (seconds, run locally)
if [[ "$LANE" == "fast" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Fast Lane Checks ==="

  # cargo fmt --check
  run_check "cargo fmt --check" cargo fmt -- --check

  # cargo clippy --all-targets -- -D warnings
  run_check "cargo clippy" cargo clippy --all-targets -- -D warnings

  # cargo check
  run_check "cargo check" cargo check
fi

# Slow lane checks (tests)
if [[ "$LANE" == "slow" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Slow Lane Checks ==="

  # cargo test --lib (unit tests)
  run_check "cargo test --lib" timeout --kill-after=30 900 cargo test --lib

  # Core integration coverage. Keep each target separately named so CI reports
  # which strand phase failed, and bound every target to fit the verify-step
  # deadline while still allowing the shared debug build to complete.
  run_check "cargo test --test integration_tests" timeout --kill-after=30 900 cargo test --test integration_tests
  run_check "cargo test --test p2_integration_tests" timeout --kill-after=30 900 cargo test --test p2_integration_tests
  run_check "cargo test --test p3_integration_tests" timeout --kill-after=30 900 cargo test --test p3_integration_tests
  run_check "cargo test --test real_br_integration_tests" timeout --kill-after=30 900 cargo test --test real_br_integration_tests

  # Installer tests (isolated, shell-level regression tests)
  run_check "installer tests" timeout --kill-after=30 60 bash tests/installer/run.sh
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
