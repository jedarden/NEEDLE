#!/usr/bin/env bash
# Regression tests for the version-checked, explicitly configured secret gate.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p "$tmp_dir/repo/scripts" "$tmp_dir/repo/config" "$tmp_dir/bin" \
    "$tmp_dir/scan-target"
cp "$repo_root/scripts/secret-scan.sh" "$tmp_dir/repo/scripts/"
cp "$repo_root/config/gitleaks.toml" "$tmp_dir/repo/config/"
chmod +x "$tmp_dir/repo/scripts/secret-scan.sh"

cd "$tmp_dir/repo"
git init -q
git config user.name test
git config user.email test@example.invalid
printf 'fixture\n' > tracked.txt
git add tracked.txt
git commit -qm initial
printf 'staged fixture\n' > tracked.txt
git add tracked.txt

cat > "$tmp_dir/bin/gitleaks" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == version ]]; then
    printf '%s\n' "${FAKE_GITLEAKS_VERSION:?}"
    exit 0
fi
printf '%s\n' "$@" > "${FAKE_GITLEAKS_CALLS:?}"
exit "${FAKE_GITLEAKS_SCAN_STATUS:-0}"
EOF
chmod +x "$tmp_dir/bin/gitleaks"

calls="$tmp_dir/calls"
unsupported_log="$tmp_dir/unsupported.log"
if FAKE_GITLEAKS_VERSION=8.24.9 FAKE_GITLEAKS_CALLS="$calls" \
    GITLEAKS_BIN="$tmp_dir/bin/gitleaks" \
    ./scripts/secret-scan.sh staged > /dev/null 2>"$unsupported_log"; then
    echo 'unsupported scanner version was accepted' >&2
    exit 1
fi
grep -Fq 'older than required 8.25.0' "$unsupported_log"
[[ ! -e "$calls" ]]

staged_log="$tmp_dir/staged.log"
FAKE_GITLEAKS_VERSION='gitleaks version 8.25.0' FAKE_GITLEAKS_CALLS="$calls" \
    GITLEAKS_BIN="$tmp_dir/bin/gitleaks" \
    ./scripts/secret-scan.sh staged > /dev/null 2>"$staged_log"
grep -Fxq git "$calls"
grep -Fxq -- --staged "$calls"
grep -Fxq -- --redact "$calls"
grep -Fxq -- --config "$calls"
grep -Fxq "$tmp_dir/repo/config/gitleaks.toml" "$calls"
grep -Fq 'scanner=gitleaks version=8.25.0 config_sha256=' "$staged_log"
grep -Fq 'result=0' "$staged_log"

printf 'generated candidate exists only in this temporary directory\n' \
    > "$tmp_dir/scan-target/candidate.txt"
directory_log="$tmp_dir/directory.log"
FAKE_GITLEAKS_VERSION=8.30.0 FAKE_GITLEAKS_CALLS="$calls" \
    GITLEAKS_BIN="$tmp_dir/bin/gitleaks" \
    ./scripts/secret-scan.sh directory "$tmp_dir/scan-target" \
    > /dev/null 2>"$directory_log"
grep -Fxq dir "$calls"
grep -Fxq "$tmp_dir/scan-target" "$calls"
grep -Fq 'mode=directory' "$directory_log"

if FAKE_GITLEAKS_VERSION=8.30.0 FAKE_GITLEAKS_CALLS="$calls" \
    FAKE_GITLEAKS_SCAN_STATUS=1 GITLEAKS_BIN="$tmp_dir/bin/gitleaks" \
    ./scripts/secret-scan.sh directory "$tmp_dir/scan-target" \
    > /dev/null 2>"$tmp_dir/finding.log"; then
    echo 'scanner finding status was not propagated' >&2
    exit 1
else
    scan_status=$?
fi
[[ "$scan_status" -eq 1 ]]
grep -Fq 'result=1' "$tmp_dir/finding.log"

echo 'secret_scan.sh: PASS'
