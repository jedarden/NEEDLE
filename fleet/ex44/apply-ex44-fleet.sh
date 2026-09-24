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
ROAM_HOME="$NEEDLE_HOST_HOME/.needle/roam-only"
ROAM_HOME_CONFIG="$SRC_DIR/roam-home.yaml"
FLEET_POLICY="$SRC_DIR/fleet-policy.env"
MANAGED_ADAPTERS_DIR="$SRC_DIR/adapters"

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
configured_worker_cap=$(sed -n 's/^NEEDLE_WORKER__MAX_WORKERS=//p' "$FLEET_POLICY")
[[ "$configured_worker_cap" =~ ^[1-9][0-9]*$ ]] || {
    echo "fleet-policy.env must define a positive NEEDLE_WORKER__MAX_WORKERS" >&2
    exit 1
}
[[ "$worker_count" -eq "$configured_worker_cap" ]] || {
    echo "workers.tsv has $worker_count workers but the fleet cap is $configured_worker_cap" >&2
    exit 1
}
[[ "$(manifest_ids | sort -u | wc -l)" -eq "$worker_count" ]] || {
    echo "workers.tsv contains duplicate identifiers" >&2
    exit 1
}

if manifest_rows | awk -F'\t' -v home="$ROAM_HOME" '$2 == home { found=1 } END { exit !found }'; then
    [[ -r "$ROAM_HOME_CONFIG" ]] || {
        echo "roam-only home config is not readable: $ROAM_HOME_CONFIG" >&2
        exit 1
    }
    run mkdir -p "$ROAM_HOME"
    if ! cmp -s "$ROAM_HOME_CONFIG" "$ROAM_HOME/.needle.yaml" 2>/dev/null; then
        echo "- installing $ROAM_HOME/.needle.yaml"
        if [[ -f "$ROAM_HOME/.needle.yaml" && "$DRY_RUN" != 1 ]]; then
            cp "$ROAM_HOME/.needle.yaml" "$ROAM_HOME/.needle.yaml.bak-$(date -u +%Y%m%dT%H%M%SZ)"
        fi
        run install -m 644 "$ROAM_HOME_CONFIG" "$ROAM_HOME/.needle.yaml"
    else
        echo "- roam-only home config already current"
    fi
fi

while IFS=$'\t' read -r id workspace agent delay explore; do
    [[ "$id" =~ ^[a-z0-9][a-z0-9-]*$ ]] || {
        echo "invalid worker identifier: $id" >&2
        exit 1
    }
    [[ -d "$workspace" || ( "$DRY_RUN" == 1 && "$workspace" == "$ROAM_HOME" ) ]] || {
        echo "$id workspace does not exist: $workspace" >&2
        exit 1
    }
    [[ -f "$MANAGED_ADAPTERS_DIR/$agent.yaml" || -f "$NEEDLE_CONFIG_DIR/adapters/$agent.yaml" ]] || {
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

mkdir -p "$SYSTEMD_DIR" "$WORKERS_DIR" "$NEEDLE_CONFIG_DIR/adapters" "$NEEDLE_HOST_HOME/.local/bin"

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
install_if_changed 644 "$SRC_DIR/needle-zai-governor.service" "$SYSTEMD_DIR/needle-zai-governor.service"
install_if_changed 644 "$SRC_DIR/needle-zai-governor.timer" "$SYSTEMD_DIR/needle-zai-governor.timer"
install_if_changed 644 "$SRC_DIR/needle-factory-audit.service" "$SYSTEMD_DIR/needle-factory-audit.service"
install_if_changed 644 "$SRC_DIR/needle-factory-audit.timer" "$SYSTEMD_DIR/needle-factory-audit.timer"
install_if_changed 644 "$SRC_DIR/needle-improve.service" "$SYSTEMD_DIR/needle-improve.service"
install_if_changed 644 "$SRC_DIR/needle-improve.timer" "$SYSTEMD_DIR/needle-improve.timer"
install_if_changed 644 "$SRC_DIR/needle-release-upgrade.service" "$SYSTEMD_DIR/needle-release-upgrade.service"
install_if_changed 644 "$SRC_DIR/needle-release-upgrade.timer" "$SYSTEMD_DIR/needle-release-upgrade.timer"
install_if_changed 755 "$SRC_DIR/needle-zai-governor" "$NEEDLE_HOST_HOME/.local/bin/needle-zai-governor"
# Both systemd ExecStart and test.sh invoke the helper directly, so it must
# land executable; its behavior suite is test.sh's --self-test run.
install_if_changed 755 "$SRC_DIR/needle-release-upgrade" "$NEEDLE_HOST_HOME/.local/bin/needle-release-upgrade"
install_if_changed 644 "$SRC_DIR/fleet-policy.env" "$NEEDLE_CONFIG_DIR/fleet-policy.env"
install_if_changed 644 "$SRC_DIR/backlog-policy.env" "$NEEDLE_CONFIG_DIR/backlog-policy.env"
for adapter in "$MANAGED_ADAPTERS_DIR"/*.yaml; do
    install_if_changed 644 "$adapter" "$NEEDLE_CONFIG_DIR/adapters/$(basename "$adapter")"
done

while IFS=$'\t' read -r id workspace agent delay explore; do
    target="$WORKERS_DIR/$id.env"
    expected=$(printf 'NEEDLE_WS=%s\nNEEDLE_AGENT=%s\nNEEDLE_START_DELAY=%s\nNEEDLE_STRANDS__EXPLORE__ENABLED=%s' \
        "$workspace" "$agent" "$delay" "$explore")
    if [[ "$agent" == codex-* ]]; then
        # Codex capacity must remain Codex capacity. The fleet-wide evidence
        # router intentionally explores GLM variants for Z.ai workers, but a
        # Codex worker must not be sampled back onto that provider pool.
        expected+=$'\nNEEDLE_AGENT__EVIDENCE_ROUTING__ENABLED=false'
    fi
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

if ! systemctl --user is-enabled -q needle-zai-governor.timer 2>/dev/null; then
    run systemctl --user enable needle-zai-governor.timer
fi
if [[ "$START_NEW" == 1 ]] && ! systemctl --user is-active -q needle-zai-governor.timer; then
    run systemctl --user start --no-block needle-zai-governor.timer
fi

# One factory-health audit per host per day (needle-a0d1eb19): never one per
# worker, and it repairs nothing. It reports, files at most three deduplicated
# beads per run, and writes an escalation brief when the learning loop stalls.
if ! systemctl --user is-enabled -q needle-factory-audit.timer 2>/dev/null; then
    run systemctl --user enable needle-factory-audit.timer
fi
if [[ "$START_NEW" == 1 ]] && ! systemctl --user is-active -q needle-factory-audit.timer; then
    run systemctl --user start --no-block needle-factory-audit.timer
fi

# ADR-029 (needle-7c064803): `needle improve` is a one-shot CLI with no
# background strand of its own, so it needs the same external-scheduler
# treatment as the factory audit. Shadow mode (improvements.admission.shadow)
# stays the config default of true here -- this unit only makes the loop
# generate and journal proposals against the live ledger; admitting any of
# them is a separate, human-reviewed config change per plan.md section 4.10.
if ! systemctl --user is-enabled -q needle-improve.timer 2>/dev/null; then
    run systemctl --user enable needle-improve.timer
fi
if [[ "$START_NEW" == 1 ]] && ! systemctl --user is-active -q needle-improve.timer; then
    run systemctl --user start --no-block needle-improve.timer
fi

# Standalone systemd workers do not run `needle supervise`, so its release
# poller is absent on ex44. This host-level timer reuses `needle upgrade`'s
# canary-gated :testing -> :stable path without starting a second worker fleet.
if ! systemctl --user is-enabled -q needle-release-upgrade.timer 2>/dev/null; then
    run systemctl --user enable needle-release-upgrade.timer
fi
if [[ "$START_NEW" == 1 ]] && ! systemctl --user is-active -q needle-release-upgrade.timer; then
    run systemctl --user start --no-block needle-release-upgrade.timer
fi

echo "- policy converged; no running worker was restarted"
