#!/usr/bin/env bash
# Regression coverage for verification bypass detection and atomic logging.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p "$tmp_dir/repo/.githooks" "$tmp_dir/repo/.beads" "$tmp_dir/repo/scripts"
cp "$repo_root/scripts/bypass-detection.sh" "$tmp_dir/repo/scripts/"
cp "$repo_root/.githooks/post-commit" "$tmp_dir/repo/.githooks/"
cp "$repo_root/.githooks/pre-commit" "$tmp_dir/repo/.githooks/"
chmod +x "$tmp_dir/repo/.githooks/post-commit" "$tmp_dir/repo/.githooks/pre-commit"

# Use lightweight stand-ins for the expensive verification and checkpoint
# commands.  The real hooks and logger remain under test.
cat > "$tmp_dir/repo/scripts/definition-of-done.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bypass-detection.sh"
if needle_bypass_requested; then
    pattern="$(needle_bypass_pattern)"
    needle_warn_bypass "$pattern" fast
    needle_mark_bypass fast "$pattern"
else
    needle_mark_verified fast
fi
EOF
cat > "$tmp_dir/repo/scripts/checkpoint-publish.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$tmp_dir/repo/scripts/definition-of-done.sh" "$tmp_dir/repo/scripts/checkpoint-publish.sh"

cd "$tmp_dir/repo"
git init -q
git config user.name test-user
git config user.email test@example.invalid
git config core.hooksPath .githooks
printf 'initial\n' > README.md
git add README.md
git commit --quiet -m initial

# Normal pre-commit verification must not create a bypass event.
printf 'verified\n' >> README.md
git add README.md
git commit --quiet -m verified
[[ ! -s .beads/bypasses.jsonl ]]

# SKIP_CHECKS=1 is detected by pre-commit and logged after the final SHA exists.
printf 'environment bypass\n' >> README.md
git add README.md
SKIP_CHECKS=1 git commit --quiet -m env-bypass 2>env-warning
grep -q 'Definition of Done bypass detected' env-warning

# --no-verify prevents pre-commit, so post-commit detects the missing marker.
printf 'flag bypass\n' >> README.md
git add README.md
git commit --quiet --no-verify -m flag-bypass 2>flag-warning
grep -q 'Definition of Done bypass detected' flag-warning

# The verification script also recognizes --no-verify when invoked directly.
NEEDLE_BYPASS_LOG="$tmp_dir/direct-bypass.jsonl" \
    "$repo_root/scripts/definition-of-done.sh" --fast --no-verify 2>direct-warning
grep -q 'Definition of Done bypass detected' direct-warning
python3 - "$tmp_dir/direct-bypass.jsonl" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    event = json.loads(stream.readline())
assert event["pattern"] == "--no-verify"
assert event["lanes_skipped"] == ["fast"]
PY

python3 - .beads/bypasses.jsonl "$(git rev-parse HEAD)" <<'PY'
import json
import sys

path, latest_sha = sys.argv[1:]
with open(path, encoding="utf-8") as stream:
    events = [json.loads(line) for line in stream if line.strip()]
assert len(events) == 2, events
assert events[0]["lanes_skipped"] == ["fast"]
assert events[1]["lanes_skipped"] == ["fast"]
assert events[1]["commit_sha"] == latest_sha
for event in events:
    for field in ("timestamp", "commit_sha", "hostname", "username", "lanes_skipped"):
        assert event[field], (field, event)
PY

# Concurrent writers must leave one valid JSON object per line.
source "$repo_root/scripts/bypass-detection.sh"
export REPO_ROOT="$tmp_dir/repo"
for i in $(seq 1 40); do
    record="$(needle_json_event "2026-01-01T00:00:00Z" "concurrent-$i" fast test concurrent "${PWD}")"
    needle_append_bypass_event "$record" &
done
wait

python3 - .beads/bypasses.jsonl <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    events = [json.loads(line) for line in stream if line.strip()]
assert len(events) == 42, len(events)
assert sum(event["commit_sha"].startswith("concurrent-") for event in events) == 40
PY

echo 'bypass_detection.sh: PASS'
