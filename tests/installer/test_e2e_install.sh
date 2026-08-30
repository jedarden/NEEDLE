#!/bin/bash
#
# End-to-end regression tests for install.sh.
#
# Unlike test_install.sh / test_checksum_verification.sh, which assert on
# fixtures and on heredoc copies of the installer's logic, this suite runs
# the REAL install.sh as a subprocess against a mock curl that serves files
# from a local fixture root. No network access, no real user installation:
# every run gets its own HOME and NEEDLE_INSTALL_PATH under a private
# temp directory, and an empty environment (env -i).
#
# Covered behaviors (see bead needle-092b40ea acceptance criteria):
#   - missing/unusable checksum data exits nonzero before moving the binary
#   - valid checksum installs; a mismatch aborts (never skippable)
#   - explicit opt-out installs with a conspicuous warning
#   - version discovery on a >pipe-buffer API payload, no broken-pipe noise
#   - --help documents the security tradeoff

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SH="$SCRIPT_DIR/../../install.sh"

TEST_COUNT=0
PASS_COUNT=0
FAIL_COUNT=0
BASE_DIR=""

# Asset name install.sh will look for, computed with the same mapping the
# installer uses so the suite works on any supported runner arch.
detect_asset_name() {
    local arch os
    case "$(uname -m)" in
        x86_64|amd64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) arch="$(uname -m)" ;;
    esac
    case "$(uname -s)" in
        Linux*) os="unknown-linux-gnu" ;;
        Darwin*) os="apple-darwin" ;;
        *) os="$(uname -s)" ;;
    esac
    echo "needle-${arch}-${os}"
}
ASSET_NAME="$(detect_asset_name)"
BEAD_ASSET_NAME="bead-${ASSET_NAME#needle-}"

setup() {
    # Fresh, unique fixture root + fake curl + isolated home per test.
    BASE_DIR=$(mktemp -d -t needle-e2e-XXXXXX)
    export MOCK_ROOT="$BASE_DIR/root"
    export MOCK_HOME="$BASE_DIR/home"
    mkdir -p "$MOCK_ROOT/files" "$MOCK_HOME" "$BASE_DIR/bin"

    # Fake curl: serves the API document and release "assets" from the
    # fixture root; a missing file behaves like `curl -f` on a 404 (exit 22).
    # The fixture root travels via $MOCK_ROOT, which run_installer passes
    # through the empty environment.
    cat > "$BASE_DIR/bin/curl" <<'MOCKEOF'
#!/bin/bash
# Mock curl for install.sh e2e tests. Serves from $MOCK_ROOT.
out=""
url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -*) shift ;;
        *)  url="$1"; shift ;;
    esac
done
if [[ "$url" == *"/bead-rs/"* ]]; then
    # bead-rs release: separate fixture tree so its checksums.txt does not
    # collide with needle's.
    if [[ "$url" == *"/releases/latest" ]]; then
        file="$MOCK_ROOT/beadrs/api.json"
    else
        file="$MOCK_ROOT/beadrs/files/$(basename "$url")"
    fi
elif [[ "$url" == *"/releases/latest" ]]; then
    file="$MOCK_ROOT/api.json"
else
    file="$MOCK_ROOT/files/$(basename "$url")"
fi
if [[ ! -f "$file" ]]; then
    echo "curl: (22) The requested URL returned error: 404 (mock)" >&2
    exit 22
fi
if [[ -n "$out" ]]; then
    cp "$file" "$out"
else
    cat "$file"
fi
MOCKEOF
    chmod +x "$BASE_DIR/bin/curl"

    # Mock needle binary: must answer --version with exit 0.
    printf '#!/bin/sh\necho "needle 0.1.0-mock"\n' > "$MOCK_ROOT/files/$ASSET_NAME"
    chmod +x "$MOCK_ROOT/files/$ASSET_NAME"

    printf '{"tag_name": "v0.1.0", "assets": [{"name": "%s"}, {"name": "checksums.txt"}]}\n' \
        "$ASSET_NAME" > "$MOCK_ROOT/api.json"

    # bead-rs release fixtures: a mock `bead` binary, its release document and
    # a correct checksums.txt. install.sh bundles bead next to needle.
    mkdir -p "$MOCK_ROOT/beadrs/files"
    printf '#!/bin/sh\necho "bead 0.2.2-mock"\n' > "$MOCK_ROOT/beadrs/files/$BEAD_ASSET_NAME"
    chmod +x "$MOCK_ROOT/beadrs/files/$BEAD_ASSET_NAME"
    printf '{"tag_name": "v0.2.2", "assets": [{"name": "%s"}, {"name": "checksums.txt"}]}\n' \
        "$BEAD_ASSET_NAME" > "$MOCK_ROOT/beadrs/api.json"
    write_bead_checksums correct

    LAST_RC=""
    LAST_OUT="$BASE_DIR/out.log"
    LAST_ERR="$BASE_DIR/err.log"
    : > "$LAST_OUT"
    : > "$LAST_ERR"
}

teardown() {
    [[ -n "$BASE_DIR" && -d "$BASE_DIR" ]] && rm -rf "$BASE_DIR"
    BASE_DIR=""
}

write_checksums() {
    # write_checksums <hash-or-special>
    #   special "correct"  -> hash of the actual mock binary
    #   special "absent"   -> checksums.txt listing only other assets
    #   special "missing"  -> no checksums.txt at all (404)
    local what="$1"
    if [[ "$what" == "missing" ]]; then
        rm -f "$MOCK_ROOT/files/checksums.txt"
        return
    fi
    local hash
    if [[ "$what" == "correct" ]]; then
        hash=$(sha256sum "$MOCK_ROOT/files/$ASSET_NAME" | awk '{print $1}')
    else
        hash="$what"
    fi
    if [[ "$what" == "absent" ]]; then
        printf 'somehash  other-asset\nanotherhash  another-asset-2\n' > "$MOCK_ROOT/files/checksums.txt"
    else
        printf '%s  %s\notherhash  some-other-asset\n' "$hash" "$ASSET_NAME" > "$MOCK_ROOT/files/checksums.txt"
    fi
}

# write_bead_checksums <correct|hash>
write_bead_checksums() {
    local hash="$1"
    if [[ "$hash" == "correct" ]]; then
        hash=$(sha256sum "$MOCK_ROOT/beadrs/files/$BEAD_ASSET_NAME" | awk '{print $1}')
    fi
    printf '%s  %s\n' "$hash" "$BEAD_ASSET_NAME" > "$MOCK_ROOT/beadrs/files/checksums.txt"
}
bead_installed() { [[ -f "$MOCK_HOME/bin/bead" ]]; }

# run_installer [--env NAME=VALUE ...] [--] [args...]
# Runs the real install.sh with an empty environment, mock curl first on
# PATH, isolated HOME/NEEDLE_INSTALL_PATH, stdin closed (the curl|bash shape).
run_installer() {
    local -a env_pairs=()
    local -a args=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --env) env_pairs+=("$2"); shift 2 ;;
            --)    shift; args=("$@"); break ;;
            *)     args+=("$1"); shift ;;
        esac
    done

    local install_path="$MOCK_HOME/bin/needle"
    local rc=0
    env -i \
        PATH="$BASE_DIR/bin:/usr/bin:/bin" \
        HOME="$MOCK_HOME" \
        NEEDLE_INSTALL_PATH="$install_path" \
        MOCK_ROOT="$MOCK_ROOT" \
        "${env_pairs[@]}" \
        bash "$INSTALL_SH" "${args[@]}" </dev/null \
        >"$LAST_OUT" 2>"$LAST_ERR" || rc=$?
    LAST_RC=$rc
}

installed() { [[ -f "$MOCK_HOME/bin/needle" ]]; }

# ---------------------------------------------------------------------------
# Assertions
# ---------------------------------------------------------------------------

record() { # record <status> <message>  (status: 0 = pass, anything else = fail)
    if [[ "$1" -eq 0 ]]; then
        echo "  ✓ $2"
        ((PASS_COUNT++)) || true
    else
        echo "  ✗ $2"
        ((FAIL_COUNT++)) || true
    fi
    ((TEST_COUNT++)) || true
}

assert_rc_zero() {
    [[ "$LAST_RC" == "0" ]]
    record $? "exit code 0 (got $LAST_RC)"
}

assert_rc_nonzero() {
    [[ "$LAST_RC" != "0" ]]
    record $? "exit code nonzero (got $LAST_RC)"
}

assert_installed() {
    installed
    record $? "binary installed at NEEDLE_INSTALL_PATH"
}

assert_not_installed() {
    if installed; then
        record 1 "binary was moved into place (should have aborted first)"
    else
        record 0 "binary NOT moved into place"
    fi
}

assert_output_contains() {
    local msg="$1" want="$2"
    if grep -qF -- "$want" "$LAST_OUT" || grep -qF -- "$want" "$LAST_ERR"; then
        record 0 "$msg"
    else
        record 1 "$msg (expected output to contain: $want)"
        echo "    ---- output ----"
        sed 's/^/    /' "$LAST_OUT" | head -10
        echo "    ---- stderr ----"
        sed 's/^/    /' "$LAST_ERR" | head -10
    fi
}

assert_output_lacks() {
    local msg="$1" banned="$2"
    if grep -qiE -- "$banned" "$LAST_OUT" || grep -qiE -- "$banned" "$LAST_ERR"; then
        record 1 "$msg (found banned pattern: $banned)"
        grep -inE -- "$banned" "$LAST_OUT" "$LAST_ERR" | head -3 | sed 's/^/    /'
    else
        record 0 "$msg"
    fi
}

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

test_valid_checksum_installs() {
    echo "TEST: valid checksum installs successfully"
    setup
    write_checksums correct
    run_installer
    assert_rc_zero
    assert_installed
    assert_output_contains "checksum verified before install" "Checksum verified"
    assert_output_contains "success message" "installed successfully"
    teardown
}

test_checksum_mismatch_aborts() {
    echo "TEST: checksum mismatch aborts before install"
    setup
    write_checksums 0000000000000000000000000000000000000000000000000000000000000000
    run_installer
    assert_rc_nonzero
    assert_not_installed
    assert_output_contains "mismatch reported" "Checksum mismatch"
    teardown
}

test_mismatch_never_skippable() {
    echo "TEST: checksum mismatch is never skippable, even with --skip-checksum"
    setup
    write_checksums 0000000000000000000000000000000000000000000000000000000000000000
    run_installer --skip-checksum
    assert_rc_nonzero
    assert_not_installed
    assert_output_contains "mismatch reported" "Checksum mismatch"
    teardown
}

test_missing_checksums_file_aborts() {
    echo "TEST: checksums.txt unavailable aborts (fail closed)"
    setup
    write_checksums missing
    run_installer
    assert_rc_nonzero
    assert_not_installed
    assert_output_contains "legible abort reason" "Could not download checksums.txt"
    teardown
}

test_skip_flag_installs_with_warning() {
    echo "TEST: --skip-checksum installs with conspicuous warning (stdin closed)"
    setup
    write_checksums missing
    run_installer --skip-checksum
    assert_rc_zero
    assert_installed
    assert_output_contains "conspicuous security warning" "SECURITY WARNING"
    assert_output_contains "skip acknowledged" "Skipping checksum verification"
    # Regression for the `read -r` at EOF bug: the prompt must not abort.
    assert_output_lacks "no prompt-induced abort" "^Error:"
    teardown
}

test_skip_env_var_installs_with_warning() {
    echo "TEST: NEEDLE_SKIP_CHECKSUM=1 installs with conspicuous warning"
    setup
    write_checksums missing
    run_installer --env NEEDLE_SKIP_CHECKSUM=1
    assert_rc_zero
    assert_installed
    assert_output_contains "conspicuous security warning" "SECURITY WARNING"
    teardown
}

test_missing_asset_entry_aborts() {
    echo "TEST: asset missing from checksums.txt aborts (fail closed)"
    setup
    write_checksums absent
    run_installer
    assert_rc_nonzero
    assert_not_installed
    assert_output_contains "legible abort reason" "Could not find checksum for"
    teardown
}

test_missing_asset_entry_skippable() {
    echo "TEST: missing asset entry + --skip-checksum installs with warning"
    setup
    write_checksums absent
    run_installer --skip-checksum
    assert_rc_zero
    assert_installed
    assert_output_contains "conspicuous security warning" "SECURITY WARNING"
    teardown
}

test_no_hash_tool_aborts() {
    echo "TEST: no local SHA-256 tool aborts (fail closed)"
    setup
    write_checksums correct

    # Restricted PATH: real tools the installer needs, mock curl, but no
    # sha256sum/shasum (and no gpg, so that branch is deterministic too).
    # `type -P` resolves PATH entries only — never shell functions — so the
    # symlinks always point at real binaries.
    local restricted="$BASE_DIR/restricted-bin"
    mkdir -p "$restricted"
    local tool
    for tool in bash awk grep sed cat cp basename uname mktemp chmod mv mkdir dirname rm; do
        local path
        path=$(type -P "$tool" 2>/dev/null) || true
        [[ -n "$path" ]] && ln -s "$path" "$restricted/$tool"
    done
    cp "$BASE_DIR/bin/curl" "$restricted/curl"

    local rc=0
    env -i \
        PATH="$restricted" \
        HOME="$MOCK_HOME" \
        NEEDLE_INSTALL_PATH="$MOCK_HOME/bin/needle" \
        MOCK_ROOT="$MOCK_ROOT" \
        bash "$INSTALL_SH" </dev/null \
        >"$LAST_OUT" 2>"$LAST_ERR" || rc=$?
    LAST_RC=$rc

    assert_rc_nonzero
    assert_not_installed
    assert_output_contains "legible abort reason" "aborted for security reasons"
    teardown
}

test_binary_download_failure_aborts() {
    echo "TEST: binary asset download failure aborts"
    setup
    rm -f "$MOCK_ROOT/files/$ASSET_NAME"
    write_checksums missing
    run_installer
    assert_rc_nonzero
    assert_not_installed
    teardown
}

test_api_fetch_failure_aborts() {
    echo "TEST: GitHub API unreachable aborts with a clear message"
    setup
    rm -f "$MOCK_ROOT/api.json"
    run_installer
    assert_rc_nonzero
    assert_not_installed
    assert_output_contains "legible abort reason" "Could not reach the GitHub API"
    teardown
}

test_version_discovery_large_payload() {
    echo "TEST: version discovery on >pipe-buffer payload, no broken-pipe diagnostic"
    setup
    write_checksums correct

    # Multi-line release document larger than the 64KiB pipe buffer with
    # tag_name near the top: an early-exiting downstream reader (the old
    # `curl | grep -m1` shape) gets its stdout closed mid-write here.
    {
        echo '{'
        echo '  "tag_name": "v9.9.9-bigpayload",'
        echo '  "assets": ['
        printf '    {"name": "%s"},\n' "$ASSET_NAME"
        local i
        for i in $(seq 1 4000); do
            printf '    {"name": "pad-asset-%d", "browser_download_url": "https://github.com/jedarden/NEEDLE/releases/download/v9.9.9-bigpayload/pad-%d"},\n' "$i" "$i"
        done
        echo '  ]'
        echo '}'
    } > "$MOCK_ROOT/api.json"

    run_installer
    assert_rc_zero
    assert_installed
    assert_output_contains "version parsed from large payload" "Latest version: v9.9.9-bigpayload"
    assert_output_lacks "no curl write-failure diagnostic" "failure writing output|broken pipe|curl: \\(23\\)"
    teardown
}

test_help_documents_security_tradeoff() {
    echo "TEST: --help documents the security tradeoff"
    setup
    run_installer --help
    assert_rc_zero
    assert_output_contains "security section present" "SECURITY NOTE"
    assert_output_contains "opt-out documented" "--skip-checksum"
    assert_output_contains "warns against opt-out" "NOT RECOMMENDED"
    teardown
}

test_unknown_option_rejected() {
    echo "TEST: unknown option rejected"
    setup
    run_installer --definitely-not-a-flag
    assert_rc_nonzero
    assert_not_installed
    assert_output_contains "usage hint" "--help"
    teardown
}

test_unsupported_architecture_fails_early() {
    echo "TEST: unsupported architecture (arm64) fails before download"
    setup
    printf '{"tag_name": "v0.1.0", "assets": [{"name": "needle-x86_64-unknown-linux-gnu"}, {"name": "checksums.txt"}]}\n' \
        > "$MOCK_ROOT/api.json"

    # Create a mock uname that reports arm64
    local mock_uname="$BASE_DIR/bin/uname"
    cat > "$mock_uname" <<'EOF'
#!/bin/bash
# Mock uname that reports arm64 to simulate unsupported architecture
case "$1" in
    -m) echo "aarch64" ;;
    -s) echo "Linux" ;;
    *)  echo "Linux" ;;
esac
EOF
    chmod +x "$mock_uname"

    # Create API response with only x86_64 assets (no aarch64)
    cat > "$MOCK_ROOT/api.json" <<'EOF'
{
  "tag_name": "v0.5.0",
  "assets": [
    {"name": "needle-x86_64-unknown-linux-gnu"},
    {"name": "checksums.txt"}
  ]
}
EOF

    # Restricted PATH with our mock uname first
    local rc=0
    env -i \
        PATH="$BASE_DIR/bin:/usr/bin:/bin" \
        HOME="$MOCK_HOME" \
        NEEDLE_INSTALL_PATH="$MOCK_HOME/bin/needle" \
        MOCK_ROOT="$MOCK_ROOT" \
        bash "$INSTALL_SH" </dev/null \
        >"$LAST_OUT" 2>"$LAST_ERR" || rc=$?
    LAST_RC=$rc

    assert_rc_nonzero
    assert_not_installed
    assert_output_contains "architecture error message" "No prebuilt binary for needle-aarch64-unknown-linux-gnu"
    assert_output_contains "build from source message" "cargo install --git https://github.com/jedarden/NEEDLE"
    assert_output_lacks "no download attempted" "Downloading"
    teardown
}

# ---------------------------------------------------------------------------
# bead backend bundling (GitHub #16)
# ---------------------------------------------------------------------------

test_bead_installed_alongside_needle() {
    echo "TEST: bead backend is installed next to needle with a verified checksum"
    setup
    write_checksums correct
    run_installer
    assert_rc_zero
    assert_installed
    record "$( bead_installed; echo $? )" "bead installed next to needle"
    assert_output_contains "bead install reported" "bead v0.2.2 installed to"
    assert_output_contains "summary names the bead version" "bead backend: v0.2.2"
    teardown
}

test_bead_checksum_tamper_aborts() {
    echo "TEST: tampered bead checksum aborts and leaves bead uninstalled"
    setup
    write_checksums correct
    write_bead_checksums "0000000000000000000000000000000000000000000000000000000000000000"
    run_installer
    assert_rc_nonzero
    record "$( ! bead_installed; echo $? )" "bead NOT moved into place"
    assert_output_contains "bead mismatch reported" "Checksum mismatch for $BEAD_ASSET_NAME"
    teardown
}

test_skip_bead_flag_leaves_bead_absent() {
    echo "TEST: --skip-bead installs needle only"
    setup
    write_checksums correct
    run_installer -- --skip-bead
    assert_rc_zero
    assert_installed
    record "$( ! bead_installed; echo $? )" "bead absent with --skip-bead"
    assert_output_contains "skip acknowledged" "Skipping bead backend install"
    teardown
}

test_skip_bead_env_leaves_bead_absent() {
    echo "TEST: NEEDLE_SKIP_BEAD=1 installs needle only"
    setup
    write_checksums correct
    run_installer --env NEEDLE_SKIP_BEAD=1
    assert_rc_zero
    assert_installed
    record "$( ! bead_installed; echo $? )" "bead absent with NEEDLE_SKIP_BEAD=1"
    teardown
}

test_existing_newer_bead_retained() {
    echo "TEST: an existing bead at or above the release version is kept"
    setup
    write_checksums correct
    printf '#!/bin/sh\necho "bead 9.9.9 (local)"\n' > "$BASE_DIR/bin/bead"
    chmod +x "$BASE_DIR/bin/bead"
    run_installer
    assert_rc_zero
    assert_installed
    record "$( ! bead_installed; echo $? )" "release bead NOT installed over a newer one"
    assert_output_contains "existing bead kept" "bead 9.9.9 already on PATH"
    assert_output_contains "summary names the kept version" "bead backend: 9.9.9"
    teardown
}

test_existing_older_bead_replaced() {
    echo "TEST: an existing bead below the release version is replaced"
    setup
    write_checksums correct
    printf '#!/bin/sh\necho "bead 0.1.3 (old)"\n' > "$BASE_DIR/bin/bead"
    chmod +x "$BASE_DIR/bin/bead"
    run_installer
    assert_rc_zero
    record "$( bead_installed; echo $? )" "release bead installed over an older one"
    assert_output_contains "bead install reported" "bead v0.2.2 installed to"
    teardown
}

test_bead_release_unreachable_is_nonfatal() {
    echo "TEST: bead-rs release unreachable warns but needle still installs"
    setup
    write_checksums correct
    rm -f "$MOCK_ROOT/beadrs/api.json"
    run_installer
    assert_rc_zero
    assert_installed
    record "$( ! bead_installed; echo $? )" "bead absent when its release is unreachable"
    assert_output_contains "legible warning" "bead not installed"
    assert_output_contains "later install path" "cargo install --git https://github.com/jedarden/bead-rs --bin bead"
    teardown
}

test_bead_no_platform_asset_is_nonfatal() {
    echo "TEST: bead-rs release without this platform's asset warns but needle still installs"
    setup
    write_checksums correct
    printf '{"tag_name": "v0.2.2", "assets": [{"name": "bead-some-other-platform"}, {"name": "checksums.txt"}]}\n' \
        > "$MOCK_ROOT/beadrs/api.json"
    run_installer
    assert_rc_zero
    assert_installed
    record "$( ! bead_installed; echo $? )" "bead absent when no platform asset exists"
    assert_output_contains "legible warning" "No prebuilt bead for"
    teardown
}

main() {
    echo "========================================="
    echo "NEEDLE Installer E2E Tests (real install.sh + mock curl)"
    echo "========================================="
    echo ""

    test_valid_checksum_installs
    test_checksum_mismatch_aborts
    test_mismatch_never_skippable
    test_missing_checksums_file_aborts
    test_skip_flag_installs_with_warning
    test_skip_env_var_installs_with_warning
    test_missing_asset_entry_aborts
    test_missing_asset_entry_skippable
    test_no_hash_tool_aborts
    test_binary_download_failure_aborts
    test_api_fetch_failure_aborts
    test_version_discovery_large_payload
    test_help_documents_security_tradeoff
    test_unknown_option_rejected
    test_unsupported_architecture_fails_early
    test_bead_installed_alongside_needle
    test_bead_checksum_tamper_aborts
    test_skip_bead_flag_leaves_bead_absent
    test_skip_bead_env_leaves_bead_absent
    test_existing_newer_bead_retained
    test_existing_older_bead_replaced
    test_bead_release_unreachable_is_nonfatal
    test_bead_no_platform_asset_is_nonfatal

    echo ""
    echo "========================================="
    echo "Results: $PASS_COUNT/$TEST_COUNT passed"
    echo "========================================="

    [[ $FAIL_COUNT -eq 0 ]]
}

main "$@"
