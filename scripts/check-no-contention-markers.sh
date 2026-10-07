#!/usr/bin/env bash
# Phase 20 (needle-9e778fad): file-contention markers under .needle/locks/ are
# checkout-local runtime state and must never be tracked or staged in this
# repository. Inside the pre-commit hook, `git diff --cached` reads the index
# being committed, so a force-added marker is caught before it lands.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "not a git checkout (clean extraction); file-contention marker check skipped"
  exit 0
fi
found="$( { git ls-files -- .needle/locks; git diff --cached --name-only -- .needle/locks; } | sort -u)"
if [ -n "$found" ]; then
  echo "ERROR: file-contention markers are tracked or staged:" >&2
  printf '  %s\n' $found >&2
  echo "Unstage with: git rm --cached -r -- .needle/locks" >&2
  exit 1
fi
echo "no file-contention markers tracked or staged"
