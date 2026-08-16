#!/usr/bin/env bash
# Regression test for dynamic checkpoint root staging and pre-commit checking.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p "$tmp_dir/repo/scripts" "$tmp_dir/repo/.githooks" \
    "$tmp_dir/repo/.beads/checkpoint/objects"
cp "$repo_root/scripts/checkpoint-publish.sh" "$tmp_dir/repo/scripts/"
cp "$repo_root/.githooks/pre-commit" "$tmp_dir/repo/.githooks/"
chmod +x "$tmp_dir/repo/scripts/checkpoint-publish.sh" "$tmp_dir/repo/.githooks/pre-commit"

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

printf 'old generation\n' > .beads/checkpoint/objects/gen-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jsonl
printf 'previous generation\n' > .beads/checkpoint/objects/gen-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.jsonl
printf 'current generation\n' > .beads/checkpoint/objects/gen-cccccccccccccccccccccccccccccccc.jsonl
printf 'view\n' > .beads/checkpoint/forensic.jsonl
write_pointer .beads/checkpoint/current.json objects/gen-cccccccccccccccccccccccccccccccc.jsonl
write_pointer .beads/checkpoint/previous.json objects/gen-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.jsonl

git add .
git commit -qm initial

printf 'new current generation\n' > .beads/checkpoint/objects/gen-dddddddddddddddddddddddddddddddd.jsonl
write_pointer .beads/checkpoint/current.json objects/gen-dddddddddddddddddddddddddddddddd.jsonl
printf 'new view\n' > .beads/checkpoint/forensic.jsonl
printf 'superseded generation\n' > .beads/checkpoint/objects/gen-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.jsonl

./scripts/checkpoint-publish.sh stage

staged="$(git diff --cached --name-only)"
grep -Fxq .beads/checkpoint/current.json <<<"$staged"
grep -Fxq .beads/checkpoint/objects/gen-dddddddddddddddddddddddddddddddd.jsonl <<<"$staged"
git ls-files --error-unmatch .beads/checkpoint/objects/gen-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.jsonl >/dev/null
! grep -Fxq .beads/checkpoint/objects/gen-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.jsonl <<<"$staged"
[[ ! -e .beads/checkpoint/objects/gen-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jsonl ]]
[[ ! -e .beads/checkpoint/objects/gen-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.jsonl ]]

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
