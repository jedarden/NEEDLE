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
    # Show the tail here while retaining the named failure for the summary.
    echo "Failure details for $name (last 20 lines):"
    echo "$output" | tail -n 20
    # Keep running so every check reports its result. The summary below returns
    # the aggregate status after all requested checks have run.
    return 0
  fi
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
  run_check "cargo test --lib" timeout 300 cargo test --lib

  # Core integration coverage. Keep each target separately named so CI reports
  # which strand phase failed, and bound every target to fit the verify-step
  # deadline while still allowing the shared debug build to complete.
  run_check "cargo test --test integration_tests" timeout 300 cargo test --test integration_tests
  run_check "cargo test --test p2_integration_tests" timeout 300 cargo test --test p2_integration_tests
  run_check "cargo test --test p3_integration_tests" timeout 300 cargo test --test p3_integration_tests
  run_check "cargo test --test real_br_integration_tests" timeout 300 cargo test --test real_br_integration_tests
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
