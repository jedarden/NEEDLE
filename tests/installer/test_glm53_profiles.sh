#!/usr/bin/env bash
# Hermetic tests for the managed GLM-5.3-Flash profile installer.
#
# This suite only copies checked-in YAML into a temporary directory. It does
# not invoke needle, Claude Code, a proxy, curl, or any credential-bearing
# environment, so it is safe to run offline.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INSTALLER="$ROOT/scripts/install-needle-adapters.sh"
SOURCE="$ROOT/fleet/ex44/adapters"
NAMES=(
    claude-code-glm-5.3-flash
    claude-code-glm-5.3-flash-1m
    claude-code-glm-5.3-flash-low
    claude-code-glm-5.3-flash-high
)

TMP_ROOT="$(mktemp -d -t needle-glm53-installer-XXXXXX)"
trap 'rm -rf "$TMP_ROOT"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

for name in "${NAMES[@]}"; do
    [[ -f "$SOURCE/$name.yaml" ]] || fail "missing managed profile: $name"
    grep -Fq 'ANTHROPIC_AUTH_TOKEN=${NEEDLE_ZAI_AUTH_TOKEN:-proxy-handles-auth}' \
        "$SOURCE/$name.yaml" || fail "$name contains a non-externalized auth value"
done

echo "TEST: GLM-5.3-Flash installer dry-run is side-effect free"
DRY_RUN_DIR="$TMP_ROOT/dry-run/adapters"
dry_run_output="$(bash "$INSTALLER" --adapters-dir "$DRY_RUN_DIR" --dry-run)"
grep -Fq 'done: 4 installed, 0 already current' <<<"$dry_run_output" || \
    fail "dry-run did not report all four profiles: $dry_run_output"
[[ ! -e "$DRY_RUN_DIR" ]] || fail "dry-run created $DRY_RUN_DIR"
echo "  ✓ dry-run reports four profiles and writes nothing"

echo "TEST: GLM-5.3-Flash installer installs all profiles and is idempotent"
DEST="$TMP_ROOT/adapters"
first_output="$(bash "$INSTALLER" --adapters-dir "$DEST")"
grep -Fq 'done: 4 installed, 0 already current' <<<"$first_output" || \
    fail "first install did not install four profiles: $first_output"
for name in "${NAMES[@]}"; do
    cmp -s "$SOURCE/$name.yaml" "$DEST/$name.yaml" || \
        fail "installed profile differs from source: $name"
done
[[ -z "$(find "$DEST" -maxdepth 1 -name '*.bak-*' -print -quit)" ]] || \
    fail "first install unexpectedly created a backup"

second_output="$(bash "$INSTALLER" --adapters-dir "$DEST")"
grep -Fq 'done: 0 installed, 4 already current' <<<"$second_output" || \
    fail "second install was not idempotent: $second_output"
echo "  ✓ all four profiles installed and the second run is a no-op"

echo "TEST: GLM-5.3-Flash installer preserves a changed target"
TARGET="$DEST/claude-code-glm-5.3-flash.yaml"
printf '%s\n' 'previous local profile fixture; not a credential' > "$TARGET"
replacement_output="$(bash "$INSTALLER" --adapters-dir "$DEST" --names claude-code-glm-5.3-flash)"
backup="$(find "$DEST" -maxdepth 1 -type f \
    -name 'claude-code-glm-5.3-flash.yaml.bak-*' -print -quit)"
[[ -n "$backup" ]] || fail "changed target did not create a backup"
grep -Fq 'previous local profile fixture; not a credential' "$backup" || \
    fail "backup did not preserve the previous target"
cmp -s "$SOURCE/claude-code-glm-5.3-flash.yaml" "$TARGET" || \
    fail "changed target was not replaced with the managed source"
grep -Fq 'done: 1 installed, 0 already current' <<<"$replacement_output" || \
    fail "replacement did not report one install: $replacement_output"
echo "  ✓ changed target was backed up before replacement"

echo "GLM-5.3-Flash profile installer tests passed"
