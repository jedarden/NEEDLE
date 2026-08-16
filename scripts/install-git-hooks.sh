#!/usr/bin/env bash
# Enable the repository's tracked Git hooks for checkpoint publication checks.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || { printf 'install-git-hooks: not inside a Git worktree\n' >&2; exit 1; }
cd "$repo_root"
git config --local core.hooksPath .githooks
printf 'install-git-hooks: enabled .githooks for %s\n' "$repo_root"
