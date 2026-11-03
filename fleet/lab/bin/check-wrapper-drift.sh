#!/usr/bin/env bash
# Wrapper drift check: is the deployed cargo wrapper still the tracked copy?
#
# The deployed ~/.local/bin/cargo and ~/.local/bin/cargo-remote live outside
# any package manager and are writable by every agent on the host; the
# 2026-08-12 codinghome clobber of cargo-remote went unnoticed for five
# weeks precisely because nothing compared the deployed bytes against the
# tracked upstream. This check runs that comparison on a timer
# (wrapper-drift.timer in this directory) so a recurrence surfaces in hours.
#
# The reference copy is resolved from origin/main after a fetch, so a stale
# checkout cannot hide upstream movement; if the fetch fails it falls back
# to this checkout's working tree and says so. The checker is executed
# straight from the checkout by its unit (no installed copy), so the check
# itself cannot drift.
#
# When ~/.config/bead-route/host.json exists (bead-route installed by
# fleet/lab/bin/install-bead-router.sh), the same comparison also covers the
# router (~/.local/bin/bead vs fleet/lab/bin/bead) and its routing table
# (~/.config/bead-route/routes.json vs fleet/lab/bead-routes.json), and
# host.json's real_bead must name an executable that is not the router. A bead
# upgrade that writes ~/.local/bin/bead clobbers the router and surfaces here
# as DRIFT; rerun the installer.
#
# Exit codes:
#   0  every deployed wrapper matches the tracked copy
#   1  DRIFT — a deployed wrapper is missing, non-executable, or diverged
#      (the alert; the unit is left failed so the journal and any
#      unit-failure monitor surface it)
#   2  PROBE — no tracked copy reachable (fetch failed AND the checkout
#      lacks the path); the detector itself is blind and must fail loudly
#
# Usage: check-wrapper-drift.sh [--quiet] [--self-test]
set -uo pipefail
# Deliberately no -e: every failure path here is branched on explicitly
# (fetch failure falls back, comparison failure is the finding).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
TRACKED_DIR="fleet/lab/bin"
INSTALL_DIR="$HOME/.local/bin"
WRAPPERS=(cargo cargo-remote)
BEAD_ROUTE_CONF="$HOME/.config/bead-route"
BEAD_ROUTE_MARKER="bead-route-router-marker"
FETCH_OK=0

QUIET=0
SELF_TEST=0
# Script-scope, not main-local: the EXIT trap below must see it after main
# returns, and a `local` is gone by then (unbound under set -u).
TMP_REF=""
trap '[[ -n "$TMP_REF" ]] && rm -f "$TMP_REF"' EXIT
for arg in "$@"; do
    case "$arg" in
        --quiet) QUIET=1 ;;
        --self-test) SELF_TEST=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# compare_one <name> <installed> <reference> <source> [require_exec]
# Prints one DRIFT or OK line for a deployed/tracked pair. <name> is the
# repo-relative tracked path. require_exec (default 1) is 0 for data files.
# Returns 0 in sync, 1 drift.
compare_one() {
    local name="$1" installed="$2" reference="$3" refsrc="$4" require_exec="${5:-1}"
    if [[ ! -f "$installed" ]]; then
        printf 'wrapper-drift: DRIFT %s missing (tracked: %s via %s)\n' \
            "$installed" "$name" "$refsrc"
        return 1
    fi
    if [[ "$require_exec" == 1 && ! -x "$installed" ]]; then
        printf 'wrapper-drift: DRIFT %s is not executable\n' "$installed"
        return 1
    fi
    if ! cmp -s "$reference" "$installed"; then
        printf 'wrapper-drift: DRIFT %s differs from tracked copy (via %s)\n' \
            "$installed" "$refsrc"
        # Bound the evidence: the unit result is read in the journal, and a
        # clobbered wrapper can differ by thousands of lines.
        diff -u "$installed" "$reference" 2>/dev/null \
            | sed -n '1,60s/^/wrapper-drift:   /p'
        return 1
    fi
    [[ "$QUIET" == 1 ]] || \
        printf 'wrapper-drift: OK %s (via %s)\n' "$installed" "$refsrc"
    return 0
}

# tracked_copy <relpath> <outfile>
# Writes the authoritative tracked copy of the repo-relative <relpath> to
# <outfile> and prints the reference source used. origin/main is used when
# main's fetch succeeded (FETCH_OK); returns 1 when nothing is reachable.
tracked_copy() {
    local rel="$1" out="$2"
    if [[ "$FETCH_OK" == 1 ]] \
       && git -C "$REPO_DIR" show "origin/main:$rel" > "$out" 2>/dev/null; then
        printf 'origin/main'
        return 0
    fi
    if [[ -f "$REPO_DIR/$rel" ]]; then
        cp "$REPO_DIR/$rel" "$out"
        printf 'checkout tree'
        return 0
    fi
    return 1
}

# check_real_bead <host.json> <router>
# bead-route's host.json must name an executable real bead that is not the
# router itself (else every routed call would recurse or fail with 127).
# Prints one DRIFT or OK line. Returns 0 sound, 1 drift.
check_real_bead() {
    local host_json="$1" router="$2" real
    real="$(python3 -c 'import json, sys; v = json.load(open(sys.argv[1])).get("real_bead"); print(v if isinstance(v, str) else "")' "$host_json" 2>/dev/null)" || real=""
    if [[ -z "$real" ]]; then
        printf 'wrapper-drift: DRIFT %s has no readable real_bead\n' "$host_json"
        return 1
    fi
    if [[ ! -f "$real" || ! -x "$real" ]]; then
        printf 'wrapper-drift: DRIFT bead-route real_bead %s is missing or not executable\n' "$real"
        return 1
    fi
    if [[ -e "$router" && "$(realpath "$real")" == "$(realpath "$router")" ]] \
       || head -c 4096 "$real" | grep -aqF "$BEAD_ROUTE_MARKER"; then
        printf 'wrapper-drift: DRIFT bead-route real_bead %s is the router itself\n' "$real"
        return 1
    fi
    [[ "$QUIET" == 1 ]] || \
        printf 'wrapper-drift: OK bead-route real_bead %s\n' "$real"
    return 0
}

# self_test: drive compare_one over fabricated pairs; touches nothing live.
self_test() {
    local rc=0 out st t
    t="$(mktemp -d)"
    local repo="$t/$TRACKED_DIR" home="$t/home/.local/bin"
    mkdir -p "$repo" "$home"
    for w in "${WRAPPERS[@]}"; do
        printf '#!/bin/sh\necho v1\n' > "$repo/$w"
        printf '#!/bin/sh\necho v1\n' > "$home/$w"
        chmod 755 "$repo/$w" "$home/$w"
    done

    if [[ "$REPO_DIR/$TRACKED_DIR" != "$SCRIPT_DIR" ]]; then
        echo "self-test: REPO_DIR resolves wrong: $REPO_DIR (script at $SCRIPT_DIR)" >&2
        rc=1
    fi

    out=$(compare_one cargo "$home/cargo" "$repo/cargo" "self-test"); st=$?
    if [[ $st -ne 0 ]]; then
        echo "self-test: identical pair reported drift (exit $st): $out" >&2; rc=1
    fi

    printf '#!/bin/sh\necho clobbered\n' > "$home/cargo"
    out=$(compare_one cargo "$home/cargo" "$repo/cargo" "self-test"); st=$?
    if [[ $st -ne 1 ]] || ! grep -q 'DRIFT' <<<"$out"; then
        echo "self-test: clobbered copy not flagged (exit $st): $out" >&2; rc=1
    fi

    rm -f "$home/cargo-remote"
    out=$(compare_one cargo-remote "$home/cargo-remote" "$repo/cargo-remote" "self-test"); st=$?
    if [[ $st -ne 1 ]] || ! grep -q 'missing' <<<"$out"; then
        echo "self-test: missing wrapper not flagged (exit $st): $out" >&2; rc=1
    fi

    printf '#!/bin/sh\necho v1\n' > "$home/cargo-remote"; chmod 644 "$home/cargo-remote"
    out=$(compare_one cargo-remote "$home/cargo-remote" "$repo/cargo-remote" "self-test"); st=$?
    if [[ $st -ne 1 ]] || ! grep -q 'not executable' <<<"$out"; then
        echo "self-test: non-executable wrapper not flagged (exit $st): $out" >&2; rc=1
    fi

    # bead-route: the routing table is data (no exec bit required) but still
    # compared byte-for-byte.
    printf '{"version": 1}\n' > "$repo/routes.json"
    printf '{"version": 1}\n' > "$t/routes.json"; chmod 644 "$t/routes.json"
    out=$(compare_one routes.json "$t/routes.json" "$repo/routes.json" "self-test" 0); st=$?
    if [[ $st -ne 0 ]]; then
        echo "self-test: identical non-executable routes.json reported drift (exit $st): $out" >&2; rc=1
    fi
    printf '{"version": 2}\n' > "$t/routes.json"
    out=$(compare_one routes.json "$t/routes.json" "$repo/routes.json" "self-test" 0); st=$?
    if [[ $st -ne 1 ]] || ! grep -q 'DRIFT' <<<"$out"; then
        echo "self-test: diverged routes.json not flagged (exit $st): $out" >&2; rc=1
    fi

    # bead-route: real_bead must be an executable that is not the router.
    local router="$home/bead" real="$t/bead-real"
    printf '#!/usr/bin/env -S python3 -I\n# %s\n' "$BEAD_ROUTE_MARKER" > "$router"
    printf '#!/bin/sh\necho bead 0.0.0\n' > "$real"
    chmod 755 "$router" "$real"
    printf '{"real_bead": "%s"}\n' "$real" > "$t/host.json"
    out=$(check_real_bead "$t/host.json" "$router"); st=$?
    if [[ $st -ne 0 ]]; then
        echo "self-test: sound real_bead reported drift (exit $st): $out" >&2; rc=1
    fi
    printf '{"real_bead": "%s"}\n' "$router" > "$t/host.json"
    out=$(check_real_bead "$t/host.json" "$router"); st=$?
    if [[ $st -ne 1 ]] || ! grep -q 'router itself' <<<"$out"; then
        echo "self-test: real_bead naming the router not flagged (exit $st): $out" >&2; rc=1
    fi
    printf '{"real_bead": "%s/absent"}\n' "$t" > "$t/host.json"
    out=$(check_real_bead "$t/host.json" "$router"); st=$?
    if [[ $st -ne 1 ]] || ! grep -q 'missing or not executable' <<<"$out"; then
        echo "self-test: missing real_bead not flagged (exit $st): $out" >&2; rc=1
    fi

    rm -rf "$t"
    if [[ $rc -eq 0 ]]; then
        echo "wrapper-drift: self-test passed"
    fi
    return "$rc"
}

main() {
    local rc=0 drifted=0 ok=0 rel installed exec_bit refsrc entry
    # Each entry: <repo-relative tracked path>|<deployed path>|<require exec>
    local items=()
    for w in "${WRAPPERS[@]}"; do
        items+=("$TRACKED_DIR/$w|$INSTALL_DIR/$w|1")
    done
    if [[ -f "$BEAD_ROUTE_CONF/host.json" ]]; then
        items+=("$TRACKED_DIR/bead|$INSTALL_DIR/bead|1")
        items+=("fleet/lab/bead-routes.json|$BEAD_ROUTE_CONF/routes.json|0")
    fi
    TMP_REF="$(mktemp)"
    git -C "$REPO_DIR" fetch --quiet origin main 2>/dev/null && FETCH_OK=1
    for entry in "${items[@]}"; do
        IFS='|' read -r rel installed exec_bit <<<"$entry"
        if ! refsrc="$(tracked_copy "$rel" "$TMP_REF")"; then
            printf 'wrapper-drift: PROBE no tracked copy of %s reachable (fetch failed and checkout lacks it)\n' \
                "$rel" >&2
            rc=2
            continue
        fi
        if compare_one "$rel" "$installed" "$TMP_REF" "$refsrc" "$exec_bit"; then
            ok=$((ok + 1))
        else
            drifted=$((drifted + 1))
            [[ $rc -eq 0 ]] && rc=1
        fi
    done
    if [[ -f "$BEAD_ROUTE_CONF/host.json" ]]; then
        if check_real_bead "$BEAD_ROUTE_CONF/host.json" "$INSTALL_DIR/bead"; then
            ok=$((ok + 1))
        else
            drifted=$((drifted + 1))
            [[ $rc -eq 0 ]] && rc=1
        fi
    fi
    if [[ $rc -eq 0 ]]; then
        printf 'wrapper-drift: all %d deployed items match the tracked copies\n' "$ok"
    else
        printf 'wrapper-drift: summary: %d drifted, %d ok\n' "$drifted" "$ok"
    fi
    return "$rc"
}

if [[ "$SELF_TEST" == 1 ]]; then
    self_test
else
    main
fi
