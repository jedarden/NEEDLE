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

# compare_one <name> <installed> <reference> <source>
# Prints one DRIFT or OK line for a wrapper pair. Returns 0 in sync, 1 drift.
compare_one() {
    local name="$1" installed="$2" reference="$3" refsrc="$4"
    if [[ ! -f "$installed" ]]; then
        printf 'wrapper-drift: DRIFT %s missing (tracked: %s/%s via %s)\n' \
            "$installed" "$TRACKED_DIR" "$name" "$refsrc"
        return 1
    fi
    if [[ ! -x "$installed" ]]; then
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

# tracked_copy <name> <outfile>
# Writes the authoritative tracked copy of $TRACKED_DIR/<name> to <outfile>
# and prints the reference source used. Returns 1 when nothing is reachable.
tracked_copy() {
    local name="$1" out="$2"
    if git -C "$REPO_DIR" fetch --quiet origin main 2>/dev/null \
       && git -C "$REPO_DIR" show "origin/main:$TRACKED_DIR/$name" > "$out" 2>/dev/null; then
        printf 'origin/main'
        return 0
    fi
    if [[ -f "$REPO_DIR/$TRACKED_DIR/$name" ]]; then
        cp "$REPO_DIR/$TRACKED_DIR/$name" "$out"
        printf 'checkout tree'
        return 0
    fi
    return 1
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

    rm -rf "$t"
    if [[ $rc -eq 0 ]]; then
        echo "wrapper-drift: self-test passed"
    fi
    return "$rc"
}

main() {
    local rc=0 drifted=0 ok=0 w installed refsrc
    TMP_REF="$(mktemp)"
    for w in "${WRAPPERS[@]}"; do
        installed="$INSTALL_DIR/$w"
        if ! refsrc="$(tracked_copy "$w" "$TMP_REF")"; then
            printf 'wrapper-drift: PROBE no tracked copy of %s reachable (fetch failed and checkout lacks %s/%s)\n' \
                "$w" "$TRACKED_DIR" "$w" >&2
            rc=2
            continue
        fi
        if compare_one "$w" "$installed" "$TMP_REF" "$refsrc"; then
            ok=$((ok + 1))
        else
            drifted=$((drifted + 1))
            [[ $rc -eq 0 ]] && rc=1
        fi
    done
    if [[ $rc -eq 0 ]]; then
        printf 'wrapper-drift: all %d deployed wrappers match the tracked copies\n' "${#WRAPPERS[@]}"
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
