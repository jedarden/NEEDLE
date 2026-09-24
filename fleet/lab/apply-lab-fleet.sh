#!/usr/bin/env bash
# Converge the lab bare-metal NEEDLE fleet to the policy tracked in this
# directory (needle-3d5c65d8, 2026-09-10).
#
# Installs:
#   needle.slice            -> ~/.config/systemd/user/needle.slice
#   needle-worker@.service  -> ~/.config/systemd/user/needle-worker@.service
#   workers.tsv instances   -> ~/.config/needle/workers/<identifier>.env
# and enables exactly the manifest instances. Any other needle-worker@<id>
# instance that is still enabled gets MASKED (not merely disabled): the
# 2026-09-09 right-size retired 12+ instances of the old 15-unit fleet, and a
# disabled unit can still be started by hand or by a stale tool loop — mask
# makes retired instances unstartable. With --install-cargo-wrapper, also
# installs bin/cargo and bin/cargo-remote -> ~/.local/bin/ (shared wrappers;
# opt-in because the operator and non-fleet agents use them too) and
# enables the wrapper-drift timer (fleet/lab/wrapper-drift.{service,timer}),
# which fails whenever a deployed wrapper stops matching the tracked copy —
# the 2026-08-12 codinghome cargo-remote clobber hid for five weeks because
# nothing made that comparison. --wrappers-only does just that wrapper +
# drift-watch half and skips the fleet convergence entirely; that is the
# codinghome form, since this manifest must not converge there.
#
# Never touched:
#   ~/.config/needle/worker-common.env  (credentials; must already exist)
#   ~/.config/systemd/user/user.control/  (systemd set-property drop-ins)
#   existing needle.slice.d/ drop-ins     (set the same values as the fragment)
#
# Running workers are NOT restarted: `systemctl --user daemon-reload` only,
# so a converge never interrupts in-flight attempts. Restart an instance
# explicitly when you want it to pick up unit changes:
#   systemctl --user restart needle-worker@<identifier>
#
# Usage: apply-lab-fleet.sh [--dry-run] [--install-cargo-wrapper] [--wrappers-only]
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SYSTEMD_DIR="$HOME/.config/systemd/user"
WORKERS_DIR="$HOME/.config/needle/workers"
MANIFEST="$SRC_DIR/workers.tsv"

DRY_RUN=0
INSTALL_CARGO_WRAPPER=0
WRAPPERS_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        --install-cargo-wrapper) INSTALL_CARGO_WRAPPER=1 ;;
        --wrappers-only) WRAPPERS_ONLY=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

run() {
    if [[ "$DRY_RUN" == 1 ]]; then
        echo "  [dry-run] $*"
    else
        "$@"
    fi
}

install_cargo_wrappers() {
    for wrapper in cargo cargo-remote; do
        if ! cmp -s "$SRC_DIR/bin/$wrapper" "$HOME/.local/bin/$wrapper" 2>/dev/null; then
            echo "- installing ~/.local/bin/$wrapper (backing up previous)"
            if [[ "$DRY_RUN" != 1 ]]; then
                [[ -f "$HOME/.local/bin/$wrapper" ]] && \
                    cp "$HOME/.local/bin/$wrapper" "$HOME/.local/bin/$wrapper.bak-$(date -u +%Y%m%dT%H%M%SZ)"
                install -m 755 "$SRC_DIR/bin/$wrapper" "$HOME/.local/bin/$wrapper"
            else
                echo "  [dry-run] backup + install ~/.local/bin/$wrapper"
            fi
        else
            echo "- ~/.local/bin/$wrapper already current"
        fi
    done
    # The drift check runs straight from this checkout, so the checker itself
    # cannot drift; it fetches origin/main for its reference, so a stale
    # checkout cannot hide upstream movement either (fleet/lab/bin/
    # check-wrapper-drift.sh for the exit-code contract).
    for unit in wrapper-drift.service wrapper-drift.timer; do
        if ! cmp -s "$SRC_DIR/$unit" "$SYSTEMD_DIR/$unit" 2>/dev/null; then
            echo "- installing $unit"
            run install -m 644 "$SRC_DIR/$unit" "$SYSTEMD_DIR/$unit"
        else
            echo "- $unit already current"
        fi
    done
    if [[ "$DRY_RUN" == 1 ]]; then
        echo "  [dry-run] systemctl --user enable --now wrapper-drift.timer"
    else
        systemctl --user enable --now wrapper-drift.timer
    fi
}

# --wrappers-only: for a host that runs the wrappers but not this manifest
# (codinghome). Installs the wrappers + drift units and nothing else — never
# the lab needle-worker convergence below.
if [[ "$WRAPPERS_ONLY" == 1 ]]; then
    echo "== wrapper + drift-watch install only (src: $SRC_DIR)"
    install_cargo_wrappers
    echo "== wrappers current"
    exit 0
fi

manifest_ids() {
    awk -F'\t' '$1 !~ /^#/ && NF >= 5 { print $1 }' "$MANIFEST"
}

echo "== lab fleet converge (src: $SRC_DIR)"

# --- sanity: manifest rows are well-formed and workspaces exist -------------
while IFS=$'\t' read -r id ws agent delay explore; do
    [[ "$id" =~ ^(lab-|#) ]] || { echo "manifest row has bad identifier: $id" >&2; exit 1; }
    [[ -d "$ws" ]] || { echo "manifest row $id: workspace missing on this host: $ws" >&2; exit 1; }
done < <(awk -F'\t' '$1 !~ /^#/ && NF >= 5 { print $1"\t"$2"\t"$3"\t"$4"\t"$5 }' "$MANIFEST")

# --- units -------------------------------------------------------------------
for unit in needle.slice needle-worker@.service; do
    if ! cmp -s "$SRC_DIR/$unit" "$SYSTEMD_DIR/$unit" 2>/dev/null; then
        echo "- installing $unit"
        run install -m 644 "$SRC_DIR/$unit" "$SYSTEMD_DIR/$unit"
    else
        echo "- $unit already current"
    fi
done

# --- per-instance env files --------------------------------------------------
mkdir -p "$WORKERS_DIR"
while IFS=$'\t' read -r id ws agent delay explore; do
    [[ "$id" =~ ^# ]] && continue
    target="$WORKERS_DIR/$id.env"
    content=$(printf 'NEEDLE_WS=%s\nNEEDLE_AGENT=%s\nNEEDLE_START_DELAY=%s\nNEEDLE_STRANDS__EXPLORE__ENABLED=%s\n' \
        "$ws" "$agent" "$delay" "$explore")
    if [[ -f "$target" ]] && [[ "$(cat "$target")" == "$content" ]]; then
        echo "- $id.env already current"
        continue
    fi
    if [[ -f "$target" && "$DRY_RUN" != 1 ]]; then
        # Preserve any operator-local values (recoverability over tidiness).
        cp "$target" "$target.bak-$(date -u +%Y%m%dT%H%M%SZ)"
    fi
    echo "- rendering $id.env"
    if [[ "$DRY_RUN" != 1 ]]; then
        printf '%s\n' "$content" > "$target"
        chmod 644 "$target"
    fi
done < "$MANIFEST"

# --- enable exactly the manifest; disable enabled strays ---------------------
for id in $(manifest_ids); do
    if ! systemctl --user is-enabled -q "needle-worker@$id.service" 2>/dev/null; then
        echo "- enabling needle-worker@$id.service"
        run systemctl --user enable "needle-worker@$id.service"
    else
        echo "- needle-worker@$id.service already enabled"
    fi
done

for link in "$SYSTEMD_DIR/default.target.wants"/needle-worker@*.service; do
    [[ -e "$link" || -L "$link" ]] || continue
    unit="${link##*/}"
    id="${unit#needle-worker@}"; id="${id%.service}"
    if ! grep -qx "$id" < <(manifest_ids); then
        echo "- DISABLING + MASKING stray unit: $unit (not in workers.tsv)"
        # `systemctl list-unit-files needle-worker@*` only reports the template
        # on some systemd versions, not enabled template instances. Enumerate
        # the target wants directly, then remove that enablement before masking.
        run systemctl --user disable "$unit"
        run systemctl --user mask "$unit"
    fi
done

# --- cargo wrappers + drift watch (opt-in: shared with non-fleet users) ------
if [[ "$INSTALL_CARGO_WRAPPER" == 1 ]]; then
    install_cargo_wrappers
fi

# --- reload (never restarts anything) ----------------------------------------
run systemctl --user daemon-reload

echo
echo "== fleet policy applied. Current state:"
systemctl --user list-units 'needle-worker@*' --no-pager --no-legend || true
echo
echo "== budget check (see README.md \"CPU budget accounting\"):"
systemctl --user show needle.slice -p CPUQuotaPerSecUSec -p MemoryMax -p MemoryHigh 2>/dev/null || true
echo
if [[ ! -f "$HOME/.config/needle/worker-common.env" ]]; then
    echo "NOTE: ~/.config/needle/worker-common.env is missing; workers will run without" >&2
    echo "shared fleet credentials (OTLP export auth). It is host-only by design —" >&2
    echo "create it on the host, never in this repo." >&2
fi
