#!/usr/bin/env bash
# Converge codinghome/ex44 to the source-controlled NEEDLE fleet policy.
# Running workers are never restarted. --start-new starts only inactive desired
# instances, with --no-block so their configured launch stagger remains async.
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NEEDLE_HOST_HOME="${NEEDLE_HOST_HOME:-$HOME}"
SYSTEMD_DIR="$NEEDLE_HOST_HOME/.config/systemd/user"
NEEDLE_CONFIG_DIR="$NEEDLE_HOST_HOME/.config/needle"
WORKERS_DIR="$NEEDLE_CONFIG_DIR/workers"
MANIFEST="$SRC_DIR/workers.tsv"
REQUIRED_EXPLORE="$SRC_DIR/required-explore-workspaces.txt"
GLOBAL_CONFIG="$NEEDLE_CONFIG_DIR/config.yaml"

DRY_RUN=0
START_NEW=0
RETIRE_STRAYS=0
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        --start-new) START_NEW=1 ;;
        --retire-strays) RETIRE_STRAYS=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

run() {
    if [[ "$DRY_RUN" == 1 ]]; then
        printf '  [dry-run]'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

manifest_rows() {
    awk -F'\t' '$1 !~ /^#/ && NF == 5 { print }' "$MANIFEST"
}

manifest_ids() {
    manifest_rows | cut -f1
}

worker_count=$(manifest_ids | wc -l)
[[ "$worker_count" -eq 21 ]] || {
    echo "workers.tsv must contain exactly 21 workers; found $worker_count" >&2
    exit 1
}
[[ "$(manifest_ids | sort -u | wc -l)" -eq "$worker_count" ]] || {
    echo "workers.tsv contains duplicate identifiers" >&2
    exit 1
}

[[ -r "$GLOBAL_CONFIG" ]] || {
    echo "global NEEDLE config is not readable: $GLOBAL_CONFIG" >&2
    exit 1
}

while IFS=$'\t' read -r id workspace agent delay explore; do
    [[ "$id" =~ ^[a-z0-9][a-z0-9-]*$ ]] || {
        echo "invalid worker identifier: $id" >&2
        exit 1
    }
    [[ -d "$workspace" ]] || {
        echo "$id workspace does not exist: $workspace" >&2
        exit 1
    }
    [[ -f "$NEEDLE_CONFIG_DIR/adapters/$agent.yaml" ]] || {
        echo "$id adapter does not exist: $agent" >&2
        exit 1
    }
    [[ "$delay" =~ ^[0-9]+$ ]] || {
        echo "$id has invalid start delay: $delay" >&2
        exit 1
    }
    [[ "$explore" == true || "$explore" == false ]] || {
        echo "$id has invalid explore flag: $explore" >&2
        exit 1
    }
done < <(manifest_rows)

# The host deliberately uses an explicit Explore list so roaming workers do
# not enter retired/scratch repositories. Merge newly-required workspaces into
# that list without reformatting or replacing the rest of the operator config.
while IFS= read -r workspace; do
    [[ -n "$workspace" && "$workspace" != \#* ]] || continue
    [[ -d "$workspace" ]] || {
        echo "required Explore workspace does not exist: $workspace" >&2
        exit 1
    }
    if grep -Eq "^[[:space:]]+-[[:space:]]+$workspace[[:space:]]*$" "$GLOBAL_CONFIG"; then
        echo "- Explore workspace already reachable: $workspace"
        continue
    fi
    echo "- adding Explore workspace to $GLOBAL_CONFIG: $workspace"
    if [[ "$DRY_RUN" != 1 ]]; then
        cp "$GLOBAL_CONFIG" "$GLOBAL_CONFIG.bak-$(date -u +%Y%m%dT%H%M%SZ)"
        tmp_config=$(mktemp "$NEEDLE_CONFIG_DIR/config.yaml.XXXXXX")
        awk -v workspace="$workspace" '
            /^    workspace_root:/ && !inserted {
                print "      - " workspace
                inserted=1
            }
            { print }
            END { if (!inserted) exit 1 }
        ' "$GLOBAL_CONFIG" >"$tmp_config"
        chmod --reference="$GLOBAL_CONFIG" "$tmp_config"
        mv "$tmp_config" "$GLOBAL_CONFIG"
    fi
done <"$REQUIRED_EXPLORE"

mkdir -p "$SYSTEMD_DIR" "$WORKERS_DIR"

install_if_changed() {
    local mode=$1 source=$2 target=$3
    if cmp -s "$source" "$target" 2>/dev/null; then
        echo "- $(basename "$target") already current"
        return
    fi
    echo "- installing $target"
    if [[ -f "$target" && "$DRY_RUN" != 1 ]]; then
        cp "$target" "$target.bak-$(date -u +%Y%m%dT%H%M%SZ)"
    fi
    run install -m "$mode" "$source" "$target"
}

install_if_changed 644 "$SRC_DIR/needle.slice" "$SYSTEMD_DIR/needle.slice"
install_if_changed 644 "$SRC_DIR/needle-worker@.service" "$SYSTEMD_DIR/needle-worker@.service"
install_if_changed 644 "$SRC_DIR/needle-backlog-slo.service" "$SYSTEMD_DIR/needle-backlog-slo.service"
install_if_changed 644 "$SRC_DIR/needle-backlog-slo.timer" "$SYSTEMD_DIR/needle-backlog-slo.timer"
install_if_changed 644 "$SRC_DIR/fleet-policy.env" "$NEEDLE_CONFIG_DIR/fleet-policy.env"
install_if_changed 644 "$SRC_DIR/backlog-policy.env" "$NEEDLE_CONFIG_DIR/backlog-policy.env"

while IFS=$'\t' read -r id workspace agent delay explore; do
    target="$WORKERS_DIR/$id.env"
    expected=$(printf 'NEEDLE_WS=%s\nNEEDLE_AGENT=%s\nNEEDLE_START_DELAY=%s\nNEEDLE_STRANDS__EXPLORE__ENABLED=%s' \
        "$workspace" "$agent" "$delay" "$explore")
    if [[ -f "$target" && "$(<"$target")" == "$expected" ]]; then
        echo "- $id.env already current"
        continue
    fi
    echo "- rendering $target"
    if [[ -f "$target" && "$DRY_RUN" != 1 ]]; then
        cp "$target" "$target.bak-$(date -u +%Y%m%dT%H%M%SZ)"
    fi
    if [[ "$DRY_RUN" != 1 ]]; then
        printf '%s\n' "$expected" >"$target"
        chmod 644 "$target"
    fi
done < <(manifest_rows)

run systemctl --user daemon-reload

while IFS= read -r id; do
    unit="needle-worker@$id.service"
    if [[ "$(systemctl --user is-enabled "$unit" 2>/dev/null || true)" == masked ]]; then
        echo "- unmasking desired worker $unit"
        run systemctl --user unmask "$unit"
    fi
    if ! systemctl --user is-enabled -q "$unit" 2>/dev/null; then
        echo "- enabling $unit"
        run systemctl --user enable "$unit"
    fi
    if [[ "$START_NEW" == 1 ]] && ! systemctl --user is-active -q "$unit"; then
        echo "- starting inactive desired worker $unit"
        run systemctl --user start --no-block "$unit"
    fi
done < <(manifest_ids)

if [[ "$RETIRE_STRAYS" == 1 ]]; then
    while IFS= read -r unit; do
        [[ -n "$unit" ]] || continue
        id=${unit#needle-worker@}
        id=${id%.service}
        if ! manifest_ids | grep -Fxq "$id"; then
            if [[ "$(systemctl --user is-enabled "$unit" 2>/dev/null || true)" == masked ]]; then
                continue
            fi
            echo "- retiring non-manifest unit $unit without stopping its current process"
            run systemctl --user disable "$unit"
            run systemctl --user mask "$unit"
        fi
    done < <(systemctl --user list-unit-files 'needle-worker@*.service' --no-legend 2>/dev/null | awk '$2 != "indirect" {print $1}')
fi

if ! systemctl --user is-enabled -q needle-backlog-slo.timer 2>/dev/null; then
    run systemctl --user enable needle-backlog-slo.timer
fi
if [[ "$START_NEW" == 1 ]] && ! systemctl --user is-active -q needle-backlog-slo.timer; then
    run systemctl --user start --no-block needle-backlog-slo.timer
fi

echo "- policy converged; no running worker was restarted"
