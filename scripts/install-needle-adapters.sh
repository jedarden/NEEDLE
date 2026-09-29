#!/usr/bin/env bash
# Install the repo-managed agent adapter profiles into the configured
# adapters directory (needle-0810eb14).
#
# Idempotent: a target whose content already matches is left untouched. A
# replaced target gets a timestamped .bak sibling first, so rollback is
# `cp <target>.bak-<ts> <target>` (or reinstall the previous git revision and
# re-run). The full fleet apply (fleet/ex44/apply-ex44-fleet.sh) performs the
# same install_if_changed sync for every managed adapter; this script is the
# standalone path for a single box or a single profile.
#
# Usage:
#   scripts/install-needle-adapters.sh [--adapters-dir DIR] [--source DIR]
#                                      [--names a,b,...] [--dry-run]
#
#   --adapters-dir  destination (default: agent.adapters_dir from .needle.yaml,
#                   falling back to ~/.config/needle/adapters)
#   --source        directory holding the managed YAMLs
#                   (default: <repo>/fleet/ex44/adapters)
#   --names         comma-separated adapter file basenames to install
#                   (default: the four managed GLM-5.3-Flash profiles)
#   --dry-run       report what would change without touching anything
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_NAMES="claude-code-glm-5.3-flash,claude-code-glm-5.3-flash-1m,claude-code-glm-5.3-flash-low,claude-code-glm-5.3-flash-high"

ADAPTERS_DIR=""
SOURCE_DIR="$ROOT/fleet/ex44/adapters"
NAMES="$DEFAULT_NAMES"
DRY_RUN=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --adapters-dir) ADAPTERS_DIR="$2"; shift 2 ;;
        --source) SOURCE_DIR="$2"; shift 2 ;;
        --names) NAMES="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
done

# Resolve the configured adapters directory from the workspace config when
# not given explicitly.
if [[ -z "$ADAPTERS_DIR" ]]; then
    ADAPTERS_DIR="$(awk '/^agent:/{in_agent=1; next}
        in_agent && /^[^ ]/{in_agent=0}
        in_agent && $1 == "adapters_dir:" {sub(/^ +/, ""); print $2; exit}' \
        "$ROOT/.needle.yaml")"
fi
if [[ -z "$ADAPTERS_DIR" ]]; then
    ADAPTERS_DIR="$HOME/.config/needle/adapters"
fi

[[ -d "$SOURCE_DIR" ]] || { echo "source directory not found: $SOURCE_DIR" >&2; exit 1; }
if [[ "$DRY_RUN" != 1 ]]; then
    mkdir -p "$ADAPTERS_DIR"
fi

installed=0
skipped=0
IFS=',' read -ra NAME_LIST <<< "$NAMES"
for name in "${NAME_LIST[@]}"; do
    name="$(echo "$name" | xargs)"
    [[ -n "$name" ]] || continue
    source="$SOURCE_DIR/$name.yaml"
    target="$ADAPTERS_DIR/$name.yaml"
    if [[ ! -f "$source" ]]; then
        echo "!! source adapter not found: $source" >&2
        exit 1
    fi
    # Cheap structural gate: needle's load_adapters fails closed on a YAML
    # that does not even name an adapter, so catch that here rather than at
    # the next worker start.
    grep -q '^name:' "$source" || { echo "!! $source has no name: field" >&2; exit 1; }
    if cmp -s "$source" "$target" 2>/dev/null; then
        echo "- $name already current"
        skipped=$((skipped + 1))
        continue
    fi
    if [[ "$DRY_RUN" == 1 ]]; then
        echo "~ $name would be installed to $target"
        installed=$((installed + 1))
        continue
    fi
    if [[ -f "$target" ]]; then
        backup="$target.bak-$(date -u +%Y%m%dT%H%M%SZ)"
        backup_suffix=0
        while [[ -e "$backup" ]]; do
            backup_suffix=$((backup_suffix + 1))
            backup="$target.bak-$(date -u +%Y%m%dT%H%M%SZ)-$backup_suffix"
        done
        cp "$target" "$backup"
        echo "- previous file kept at $backup"
    fi
    install -m 644 "$source" "$target"
    echo "- installed $target"
    installed=$((installed + 1))
done

echo "done: $installed installed, $skipped already current"
