#!/usr/bin/env bash
# Verification gate for NEEDLE bead closure.
# Builds HEAD in an isolated detached worktree to catch compilation errors
# that would prevent CI from running, ensuring the shipped commit works.
#
# This must pass before a bead can be closed — premature closures that break
# the build will be automatically reopened and released by OutcomeHandler.

set -euo pipefail

# Emit a marker for the verification gate handler
echo "NEEDLE_VERIFICATION_GATE: verify-shipped-commit"

# Get the repository root
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Create a temporary directory for the worktree
WORKTREE_BASE="${REPO_ROOT}/.needle-verify-temp"
WORKTREE_DIR="${WORKTREE_BASE}/verify-shipped-commit-$$"

# Ensure cleanup on exit (both success and failure)
cleanup() {
    local exit_code=$?
    if [[ -d "$WORKTREE_DIR" ]]; then
        echo "Cleaning up worktree: $WORKTREE_DIR"
        git worktree remove "$WORKTREE_DIR" --force 2>/dev/null || true
    fi
    # Also clean up the base directory if it's empty
    if [[ -d "$WORKTREE_BASE" && -z "$(ls -A "$WORKTREE_BASE" 2>/dev/null)" ]]; then
        rmdir "$WORKTREE_BASE" 2>/dev/null || true
    fi
    exit $exit_code
}
trap cleanup EXIT

# Create a detached worktree at HEAD
echo "Creating detached worktree at HEAD..."
git worktree add --detach "$WORKTREE_DIR" HEAD

# Use a shared target directory for incremental builds
# This keeps subsequent gate runs fast
export CARGO_TARGET_DIR="${REPO_ROOT}/target"

# Run the build check
echo "Verifying shipped commit builds..."
cd "$WORKTREE_DIR"

# Check all targets (lib, bins, tests, examples)
# Use --all-targets to catch test compilation failures like needle-04df9025
if cargo check --all-targets 2>&1; then
    echo "✓ Shipped commit builds successfully"
    exit 0
else
    exit_code=$?
    echo "✗ Shipped commit does not build (exit code: $exit_code)"
    echo "Bead will be reopened and released"
    exit 1
fi
