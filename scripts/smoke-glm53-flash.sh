#!/usr/bin/env bash
# Credential-safe smoke test for a managed GLM-5.3-Flash adapter profile
# (needle-0810eb14).
#
# Renders the REAL invoke_template from the profile's YAML (never a copied
# command line), runs it once against a one-line prompt in a scratch
# workspace, and reports the model identity the provider returned in the
# stream — the proxy is expected to answer "glm-5.3-flash" (Z.AI routes
# retired aliases silently, so the model field is the only trustworthy
# evidence of what was served).
#
# No credential value is printed or stored: the template carries only the
# externalized ${NEEDLE_ZAI_AUTH_TOKEN:-proxy-handles-auth} reference, and
# the real authentication happens at the proxy.
#
# Usage:
#   scripts/smoke-glm53-flash.sh [profile-name] [--adapters-dir DIR]
#
#   profile-name    adapter name (default: claude-code-glm-5.3-flash; the
#                   bare suffixes 1m/low/high are accepted)
#   --adapters-dir  read the profile from an installed adapters directory
#                   instead of the repo source in fleet/ex44/adapters
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="claude-code-glm-5.3-flash"
SOURCE_DIR="$ROOT/fleet/ex44/adapters"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --adapters-dir) SOURCE_DIR="$2"; shift 2 ;;
        -*) echo "unknown argument: $1" >&2; exit 1 ;;
        *) PROFILE="$1"; shift ;;
    esac
done

case "$PROFILE" in
    1m|low|high) PROFILE="claude-code-glm-5.3-flash-$PROFILE" ;;
esac

FILE="$SOURCE_DIR/$PROFILE.yaml"
[[ -f "$FILE" ]] || { echo "profile not found: $FILE" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 required to render the template" >&2; exit 1; }

SCRATCH="$(mktemp -d)"
PROMPT="$SCRATCH/prompt.txt"
printf 'Reply with exactly the token SMOKE-OK and nothing else. Do not use any tools.\n' > "$PROMPT"

RENDERED="$(python3 - "$FILE" "$SCRATCH" "$PROMPT" <<'PY'
import sys, yaml
profile_file, workspace, prompt_file = sys.argv[1:4]
template = yaml.safe_load(open(profile_file))["invoke_template"]
for key, value in {
    "{workspace}": workspace,
    "{prompt_file}": prompt_file,
    "{bead_id}": "smoke",
    "{model}": str(yaml.safe_load(open(profile_file)).get("model") or ""),
}.items():
    template = template.replace(key, value)
print(template)
PY
)"

echo "== smoke: $PROFILE"
echo "== rendered invocation (placeholders substituted, env unchanged):"
echo "$RENDERED"

OUT="$SCRATCH/stream.jsonl"
set +e
timeout --signal=TERM --kill-after=10 300 bash -c "$RENDERED" > "$OUT" 2> "$SCRATCH/stderr.txt"
STATUS=$?
set -e

echo "== agent exit status: $STATUS"
if [[ -s "$SCRATCH/stderr.txt" ]]; then
    # Claude/provider diagnostics may echo request material. Keep the smoke
    # credential-safe by reporting presence only; never copy stderr to stdout.
    echo "== stderr: present (not displayed; may contain request material)"
fi

MODELS="$(grep -o '"model":"[^"]*"' "$OUT" | sort -u | head -5)"
if [[ -n "$MODELS" ]]; then
    echo "== provider-returned model identity:"
    echo "$MODELS"
else
    echo "== no model identity found in the stream"
fi

RESULT_LINE="$(grep '"type":"result"' "$OUT" | tail -1)"
if [[ -n "$RESULT_LINE" ]]; then
    echo "== result envelope: observed (payload withheld)"
fi

if grep -q '"model":"glm-5.3-flash' "$OUT"; then
    echo "== SMOKE PASS: effective model is glm-5.3-flash (family)"
    rm -rf "$SCRATCH"
    exit 0
fi
echo "== SMOKE FAIL: no glm-5.3-flash model identity in the stream"
echo "== retaining scratch dir for diagnosis: $SCRATCH"
exit 1
