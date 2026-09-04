#!/usr/bin/env bash
# Regression test for dynamic checkpoint root staging and pre-commit checking.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p "$tmp_dir/repo/scripts" "$tmp_dir/repo/config" "$tmp_dir/repo/.githooks" \
    "$tmp_dir/repo/.beads/checkpoint/objects"
cp "$repo_root/scripts/checkpoint-publish.sh" "$tmp_dir/repo/scripts/"
cp "$repo_root/scripts/secret-scan.sh" "$tmp_dir/repo/scripts/"
cp "$repo_root/config/gitleaks.toml" "$tmp_dir/repo/config/"
cp "$repo_root/.githooks/pre-commit" "$tmp_dir/repo/.githooks/"
cat > "$tmp_dir/repo/scripts/bypass-detection.sh" <<'EOF'
needle_clear_index_state() {
    :
}
EOF
cat > "$tmp_dir/repo/scripts/definition-of-done.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "$tmp_dir/repo/scripts/fake-gitleaks" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == version ]]; then
    printf '8.30.1\n'
    exit 0
fi
exit 0
EOF
chmod +x "$tmp_dir/repo/scripts/checkpoint-publish.sh" \
    "$tmp_dir/repo/scripts/secret-scan.sh" \
    "$tmp_dir/repo/scripts/fake-gitleaks" \
    "$tmp_dir/repo/scripts/definition-of-done.sh" \
    "$tmp_dir/repo/.githooks/pre-commit"
export GITLEAKS_BIN="$tmp_dir/repo/scripts/fake-gitleaks"

cd "$tmp_dir/repo"
git init -q
git config user.name test
git config user.email test@example.invalid

write_pointer() {
    local pointer_file="$1"
    local root_file="$2"
    local root_path="objects/$(basename "$root_file")"
    local root_sha
    root_sha="$(sha256sum ".beads/checkpoint/$root_file" | awk '{print $1}')"
    python3 - "$pointer_file" "$root_path" "$root_sha" <<'PY'
import json
import sys

pointer_file, root_path, root_sha = sys.argv[1:]
with open(pointer_file, "w", encoding="utf-8") as stream:
    json.dump({"active_root": {"path": root_path, "sha256": root_sha}}, stream)
    stream.write("\n")
PY
}

printf 'old generation\n' > .beads/checkpoint/objects/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jsonl
printf 'previous generation\n' > .beads/checkpoint/objects/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.jsonl
printf 'current generation\n' > .beads/checkpoint/objects/cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc.jsonl
printf 'legacy generation\n' > .beads/checkpoint/objects/gen-ffffffffffffffffffffffffffffffff.jsonl
printf 'already removed generation\n' > .beads/checkpoint/objects/9999999999999999999999999999999999999999999999999999999999999999.jsonl
printf 'view\n' > .beads/checkpoint/forensic.jsonl
write_pointer .beads/checkpoint/current.json objects/cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc.jsonl
write_pointer .beads/checkpoint/previous.json objects/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.jsonl

git add .
git commit -qm initial

rm .beads/checkpoint/objects/9999999999999999999999999999999999999999999999999999999999999999.jsonl
printf 'new current generation\n' > .beads/checkpoint/objects/dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd.jsonl
write_pointer .beads/checkpoint/current.json objects/dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd.jsonl
printf 'new view\n' > .beads/checkpoint/forensic.jsonl
printf 'superseded generation\n' > .beads/checkpoint/objects/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.jsonl

./scripts/checkpoint-publish.sh stage

staged="$(git diff --cached --name-only)"
grep -Fxq .beads/checkpoint/current.json <<<"$staged"
grep -Fxq .beads/checkpoint/objects/dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd.jsonl <<<"$staged"
git ls-files --error-unmatch .beads/checkpoint/objects/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.jsonl >/dev/null
! git ls-files --error-unmatch .beads/checkpoint/objects/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jsonl >/dev/null 2>&1
! git ls-files --error-unmatch .beads/checkpoint/objects/9999999999999999999999999999999999999999999999999999999999999999.jsonl >/dev/null 2>&1
! git ls-files --error-unmatch .beads/checkpoint/objects/gen-ffffffffffffffffffffffffffffffff.jsonl >/dev/null 2>&1
! grep -Fxq .beads/checkpoint/objects/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.jsonl <<<"$staged"
[[ ! -e .beads/checkpoint/objects/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jsonl ]]
[[ ! -e .beads/checkpoint/objects/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.jsonl ]]
[[ ! -e .beads/checkpoint/objects/gen-ffffffffffffffffffffffffffffffff.jsonl ]]

git config core.hooksPath .githooks
git commit -qm dynamic-roots

printf 'missing root pointer\n' > .beads/checkpoint/current.json
git add .beads/checkpoint/current.json
if git commit -qm should-fail 2>"$tmp_dir/hook-error"; then
    echo 'pre-commit accepted a pointer with no staged root' >&2
    exit 1
fi
grep -Fq 'invalid checkpoint pointer' "$tmp_dir/hook-error"

echo 'checkpoint_publish.sh: PASS'
