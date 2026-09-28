#!/usr/bin/env bash
# Keep the Forgejo source-of-truth and GitHub artifact-mirror contract explicit.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${NEEDLE_RELEASE_AUTHORITY_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
README="$REPO_ROOT/README.md"
LLMS="$REPO_ROOT/llms.txt"
INSTALLER="$REPO_ROOT/install.sh"

failures=0

require_text() {
    local file="$1"
    local text="$2"
    local description="$3"

    if [[ ! -f "$file" ]]; then
        printf 'release authority check: missing %s\n' "$file" >&2
        failures=$((failures + 1))
    elif ! grep -Fq -- "$text" "$file"; then
        printf 'release authority check: missing %s in %s\n' \
            "$description" "${file#"$REPO_ROOT/"}" >&2
        failures=$((failures + 1))
    fi
}

# README: source operations are Forgejo-first, while the status badge and
# release/install links deliberately target the public GitHub mirror.
require_text "$README" \
    'https://git.ardenone.com/jedarden/NEEDLE' \
    'canonical Forgejo repository URL'
require_text "$README" \
    'GitHub is a read-only mirror' \
    'read-only mirror statement'
require_text "$README" \
    'https://img.shields.io/github/checks-status/jedarden/NEEDLE/main' \
    'GitHub mirror CI-status badge'
require_text "$README" \
    'https://img.shields.io/github/v/release/jedarden/NEEDLE' \
    'GitHub release-version badge'
require_text "$README" \
    'https://github.com/jedarden/NEEDLE/releases/latest' \
    'GitHub latest-release URL'
require_text "$README" \
    'https://github.com/jedarden/NEEDLE/releases/latest/download/install.sh' \
    'GitHub release installer URL'
require_text "$README" \
    'cargo install --git https://git.ardenone.com/jedarden/NEEDLE' \
    'canonical Forgejo source-install command'
require_text "$README" \
    'release lane pushes the version tag to Forgejo' \
    'tag-before-artifact release flow'
require_text "$README" \
    'never push there directly' \
    'no-direct-push mirror rule'

# Keep the machine-facing installer aligned with the same flow. Its GitHub
# API/download endpoints are intentional artifact lookups, not source remotes.
require_text "$INSTALLER" \
    'Source of truth: https://git.ardenone.com/jedarden/NEEDLE' \
    'installer source-of-truth comment'
require_text "$INSTALLER" \
    'Public release/artifact mirror: https://github.com/jedarden/NEEDLE' \
    'installer artifact-mirror comment'
require_text "$INSTALLER" \
    'GITHUB_API="https://api.github.com/repos/$REPO/releases/latest"' \
    'GitHub release API endpoint'
require_text "$INSTALLER" \
    'https://github.com/${REPO}/releases/download/' \
    'GitHub release download endpoint'
require_text "$INSTALLER" \
    'cargo install --git https://git.ardenone.com/jedarden/NEEDLE' \
    'canonical source-install fallback'

# Agent-readable onboarding must not silently turn the artifact host into the
# source-of-truth host.
require_text "$LLMS" \
    'Forgejo (`git.ardenone.com/jedarden/NEEDLE`) is the source of truth' \
    'agent-readable source authority statement'
require_text "$LLMS" \
    'public release artifacts and checksums are published on GitHub' \
    'agent-readable artifact flow'
require_text "$LLMS" \
    'do not add GitHub as a push remote' \
    'agent-readable no-dual-push rule'

if [[ "$failures" -ne 0 ]]; then
    printf 'release authority check: %d failure(s)\n' "$failures" >&2
    exit 1
fi

echo 'release authority check: Forgejo source and GitHub artifact flow are documented'
