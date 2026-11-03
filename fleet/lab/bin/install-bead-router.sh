#!/usr/bin/env bash
# Install bead-route (fleet/lab/bin/bead) as ~/.local/bin/bead on this host.
#
# The router must sit first on PATH as `bead`, and every real bead call goes
# through it, so this installer never leaves a moment without a working
# `bead`:
#   1. validate the routing table (JSON, then the router's own validator)
#   2. find the real bead binary: a non-router ~/.local/bin/bead (a fresh host,
#      or a bead upgrade that clobbered the router) is copied to
#      ~/.local/lib/bead-route/bead-real, keeping a timestamped backup of a
#      different previous copy; otherwise the existing bead-real; otherwise
#      ~/.cargo/bin/bead; otherwise fail
#   3. write ~/.config/bead-route/host.json and routes.json (temp + mv)
#   4. install the router to ~/.local/bin/bead (temp in the same dir + mv -f,
#      an atomic rename over the old file)
#   5. verify `bead --version` and `bead __bead-route-role__ $HOME`; on
#      failure restore the previous ~/.local/bin/bead and configuration
#
# Re-running is idempotent. Run it again after any bead upgrade that writes
# ~/.local/bin/bead (wrapper-drift reports that clobber as DRIFT).
#
# Usage: install-bead-router.sh [--dry-run] [ROUTER_FILE [ROUTES_FILE]]
#   defaults: <this dir>/bead and <this dir>/../bead-routes.json
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MARKER="bead-route-router-marker"

DRY_RUN=0
POSITIONAL=()
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d; s/^# \{0,1\}//; p}' "$0"; exit 0 ;;
        -*) echo "unknown argument: $arg" >&2; exit 2 ;;
        *) POSITIONAL+=("$arg") ;;
    esac
done
if (( ${#POSITIONAL[@]} > 2 )); then
    echo "usage: install-bead-router.sh [--dry-run] [ROUTER_FILE [ROUTES_FILE]]" >&2
    exit 2
fi
ROUTER_SRC="${POSITIONAL[0]:-$SCRIPT_DIR/bead}"
ROUTES_SRC="${POSITIONAL[1]:-$LAB_DIR/bead-routes.json}"

BIN_DIR="$HOME/.local/bin"
LIB_DIR="$HOME/.local/lib/bead-route"
CONF_DIR="$HOME/.config/bead-route"
TARGET="$BIN_DIR/bead"
REAL_COPY="$LIB_DIR/bead-real"
CARGO_BEAD="$HOME/.cargo/bin/bead"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"

say() { printf 'install-bead-router: %s\n' "$*"; }
die() { printf 'install-bead-router: ERROR: %s\n' "$*" >&2; exit 1; }
dry() { printf 'install-bead-router:   [dry-run] %s\n' "$*"; }

# is_router <file>: the tracked router, or any Python script carrying the
# bead-route marker (an older router version).
is_router() {
    local f="$1"
    [[ -f "$f" ]] || return 1
    cmp -s "$f" "$ROUTER_SRC" && return 0
    [[ "$(head -c 2 "$f" 2>/dev/null | od -An -c | tr -d ' ')" == '#!' ]] || return 1
    head -c 4096 "$f" | grep -aq python && head -c 4096 "$f" | grep -aqF "$MARKER"
}

# is_real_bead <file>: executable and answers --version as bead.
is_real_bead() {
    local f="$1" out
    [[ -f "$f" && -x "$f" ]] || return 1
    is_router "$f" && return 1
    out="$(cd / && "$f" --version 2>/dev/null)" || return 1
    [[ "$out" == bead\ * ]]
}

# write_atomic <dest> <mode>: stdin -> temp beside dest -> mv -f.
write_atomic() {
    local dest="$1" mode="$2" tmp
    tmp="$(mktemp "$(dirname "$dest")/.$(basename "$dest").tmp.XXXXXX")"
    cat > "$tmp"
    chmod "$mode" "$tmp"
    mv -f "$tmp" "$dest"
}

say "router: $ROUTER_SRC"
say "routes: $ROUTES_SRC"
[[ "$DRY_RUN" == 1 ]] && say "dry run: nothing will be changed"

# --- 1. validate -------------------------------------------------------------
[[ -f "$ROUTER_SRC" ]] || die "router file not found: $ROUTER_SRC"
[[ -f "$ROUTES_SRC" ]] || die "routes file not found: $ROUTES_SRC"
# Absolute from here on: the checks below run from `cd /`, where a relative
# argument (e.g. `install-bead-router.sh bin/bead bead-routes.json`) is lost.
ROUTER_SRC="$(cd "$(dirname "$ROUTER_SRC")" && pwd)/$(basename "$ROUTER_SRC")"
ROUTES_SRC="$(cd "$(dirname "$ROUTES_SRC")" && pwd)/$(basename "$ROUTES_SRC")"
grep -aqF "$MARKER" "$ROUTER_SRC" || die "$ROUTER_SRC does not carry the bead-route marker"
python3 -c 'import json, sys; json.load(open(sys.argv[1]))' "$ROUTES_SRC" \
    || die "routes file is not valid JSON: $ROUTES_SRC"
(cd / && BEAD_ROUTE_ROUTES="$ROUTES_SRC" python3 -I "$ROUTER_SRC" __bead-route-role__ / >/dev/null) \
    || die "the router rejects the routing table $ROUTES_SRC"
say "routing table valid"

# --- 2. real bead ---------------------------------------------------------------
REAL=""
if [[ -e "$TARGET" || -L "$TARGET" ]] && ! is_router "$TARGET"; then
    is_real_bead "$TARGET" \
        || die "$TARGET is neither the router nor a working bead binary; refusing to replace it"
    if [[ -f "$REAL_COPY" ]] && cmp -s "$TARGET" "$REAL_COPY"; then
        say "real bead already preserved at $REAL_COPY"
    else
        if [[ -e "$REAL_COPY" ]]; then
            if [[ "$DRY_RUN" == 1 ]]; then
                dry "back up $REAL_COPY -> $REAL_COPY.bak-$STAMP"
            else
                cp -p "$REAL_COPY" "$REAL_COPY.bak-$STAMP"
                say "backed up previous real bead to $REAL_COPY.bak-$STAMP"
            fi
        fi
        if [[ "$DRY_RUN" == 1 ]]; then
            dry "preserve $TARGET as $REAL_COPY"
        else
            mkdir -p "$LIB_DIR"
            tmp="$(mktemp "$LIB_DIR/.bead-real.tmp.XXXXXX")"
            cp "$TARGET" "$tmp"
            chmod 755 "$tmp"
            mv -f "$tmp" "$REAL_COPY"
            say "preserved $TARGET as $REAL_COPY"
        fi
    fi
    REAL="$REAL_COPY"
elif is_real_bead "$REAL_COPY"; then
    REAL="$REAL_COPY"
elif is_real_bead "$CARGO_BEAD"; then
    REAL="$CARGO_BEAD"
else
    die "no real bead binary found (checked $TARGET, $REAL_COPY, $CARGO_BEAD)"
fi
say "real bead: $REAL"

if [[ "$DRY_RUN" == 1 ]]; then
    dry "write $CONF_DIR/host.json {\"real_bead\": \"$REAL\"}"
    if cmp -s "$ROUTES_SRC" "$CONF_DIR/routes.json" 2>/dev/null; then
        say "$CONF_DIR/routes.json already current"
    else
        dry "write $CONF_DIR/routes.json from $ROUTES_SRC"
    fi
    if cmp -s "$ROUTER_SRC" "$TARGET" 2>/dev/null; then
        say "$TARGET already current"
    else
        dry "install $ROUTER_SRC -> $TARGET (atomic rename)"
    fi
    dry "verify $TARGET --version and $TARGET __bead-route-role__ \$HOME"
    exit 0
fi

# --- 3. configuration -----------------------------------------------------------
mkdir -p "$CONF_DIR" "$BIN_DIR"
SAVE_DIR="$(mktemp -d "$CONF_DIR/.install-save.XXXXXX")"
PREV_TARGET=""
cleanup() { rm -rf "$SAVE_DIR"; [[ -n "$PREV_TARGET" ]] && rm -f "$PREV_TARGET"; return 0; }
trap cleanup EXIT
for f in host.json routes.json; do
    [[ -f "$CONF_DIR/$f" ]] && cp -p "$CONF_DIR/$f" "$SAVE_DIR/$f"
done
python3 -c 'import json, sys; print(json.dumps({"real_bead": sys.argv[1]}))' "$REAL" \
    | write_atomic "$CONF_DIR/host.json" 644
write_atomic "$CONF_DIR/routes.json" 644 < "$ROUTES_SRC"
say "wrote $CONF_DIR/host.json and $CONF_DIR/routes.json"

# --- 4. router --------------------------------------------------------------------
if [[ -e "$TARGET" || -L "$TARGET" ]]; then
    PREV_TARGET="$BIN_DIR/.bead.prev.$$"
    ln "$TARGET" "$PREV_TARGET" 2>/dev/null || cp -pP "$TARGET" "$PREV_TARGET"
fi
tmp="$(mktemp "$BIN_DIR/.bead.new.XXXXXX")"
cp "$ROUTER_SRC" "$tmp"
chmod 755 "$tmp"
mv -f "$tmp" "$TARGET"
say "installed router at $TARGET"

# --- 5. verify ----------------------------------------------------------------------
verify() {
    local out
    out="$(cd / && "$TARGET" --version 2>&1)" || { say "verify: --version failed: $out"; return 1; }
    [[ "$out" == bead\ * ]] || { say "verify: unexpected --version output: $out"; return 1; }
    "$TARGET" __bead-route-role__ "$HOME" >/dev/null || { say "verify: role query failed"; return 1; }
}
if ! verify; then
    if [[ -n "$PREV_TARGET" ]]; then
        mv -f "$PREV_TARGET" "$TARGET"
        PREV_TARGET=""
    else
        rm -f "$TARGET"
    fi
    for f in host.json routes.json; do
        if [[ -f "$SAVE_DIR/$f" ]]; then
            mv -f "$SAVE_DIR/$f" "$CONF_DIR/$f"
        else
            rm -f "$CONF_DIR/$f"
        fi
    done
    die "verification failed; restored the previous $TARGET and configuration"
fi
say "verified: $(cd / && "$TARGET" --version 2>&1)"
say "done"
