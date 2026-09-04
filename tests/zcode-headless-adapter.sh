#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLUGIN_DIR="$REPO_ROOT/plugins/zcode-headless"
TEST_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEST_ROOT"' EXIT

FAKE_ZCODE="$TEST_ROOT/zcode.cjs"
FAKE_ARGS="$TEST_ROOT/args"
FAKE_PROMPT="$TEST_ROOT/prompt"
WORKSPACE="$TEST_ROOT/workspace"
PROMPT_FILE="$TEST_ROOT/prompt.md"
SETTINGS_FILE="$TEST_ROOT/settings.json"
mkdir -p "$WORKSPACE"

cat > "$FAKE_ZCODE" <<'EOF'
const fs = require("fs");
const args = process.argv.slice(2);
if (args.length === 1 && args[0] === "--version") {
  process.stdout.write("0.16.5\n");
  process.exit(0);
}
if (args.length === 1 && args[0] === "--help") {
  process.stdout.write("zcode 0.16.5\n\nUsage: zcode --prompt --cwd --surface --mode --max-turns --json --settings\n");
  process.exit(0);
}
fs.writeFileSync(process.env.FAKE_ARGS, args.map((value) => JSON.stringify(value)).join("\n"));
const promptIndex = args.indexOf("--prompt");
if (promptIndex >= 0) fs.writeFileSync(process.env.FAKE_PROMPT, args[promptIndex + 1]);
process.stdout.write('{"type":"result","model":"glm-5.3-flash","ok":true}\n');
process.exit(Number(process.env.FAKE_EXIT_CODE || "0"));
EOF

printf 'Line one\nLine two with $(printf not-executed) and `backticks`\n' > "$PROMPT_FILE"
printf '{}\n' > "$SETTINGS_FILE"

export NEEDLE_ZCODE_CLI="$FAKE_ZCODE"
export FAKE_ARGS FAKE_PROMPT
export ZCODE_API_KEY="credential-must-not-appear"

version="$($PLUGIN_DIR/needle-zcode-headless --version)"
[[ "$version" == "zcode 0.16.5" ]]

output="$($PLUGIN_DIR/needle-zcode-headless \
    --prompt-file "$PROMPT_FILE" \
    --workspace "$WORKSPACE" \
    --mode edit \
    --max-turns 17 \
    --settings "$SETTINGS_FILE")"
[[ "$output" == *'"model":"glm-5.3-flash"'* ]]
cmp "$PROMPT_FILE" "$FAKE_PROMPT"
grep -Fx -- '"--cwd"' "$FAKE_ARGS" >/dev/null
grep -Fx -- "\"$WORKSPACE\"" "$FAKE_ARGS" >/dev/null
grep -Fx -- '"--mode"' "$FAKE_ARGS" >/dev/null
grep -Fx -- '"edit"' "$FAKE_ARGS" >/dev/null
grep -Fx -- '"--max-turns"' "$FAKE_ARGS" >/dev/null
grep -Fx -- '"17"' "$FAKE_ARGS" >/dev/null
grep -Fx -- '"--json"' "$FAKE_ARGS" >/dev/null
grep -Fx -- '"--no-color"' "$FAKE_ARGS" >/dev/null
grep -Fx -- '"--settings"' "$FAKE_ARGS" >/dev/null
grep -Fx -- "\"$SETTINGS_FILE\"" "$FAKE_ARGS" >/dev/null
if grep -Fq "$ZCODE_API_KEY" "$FAKE_ARGS"; then
    printf '%s\n' "credential leaked into ZCode argv" >&2
    exit 1
fi

set +e
FAKE_EXIT_CODE=23 "$PLUGIN_DIR/needle-zcode-headless" \
    --prompt-file "$PROMPT_FILE" --workspace "$WORKSPACE" >/dev/null
exit_code=$?
set -e
[[ "$exit_code" -eq 23 ]]

INSTALL_BIN="$TEST_ROOT/install/bin"
INSTALL_ADAPTERS="$TEST_ROOT/install/adapters"
INSTALL_CONFIG="$TEST_ROOT/install/config"
"$PLUGIN_DIR/install.sh" \
    --bin-dir "$INSTALL_BIN" \
    --adapter-dir "$INSTALL_ADAPTERS" \
    --config-dir "$INSTALL_CONFIG" \
    --zcode-cli "$FAKE_ZCODE" >/dev/null

[[ -x "$INSTALL_BIN/needle-zcode-headless" ]]
[[ -f "$INSTALL_ADAPTERS/zcode-headless.yaml" ]]
[[ "$(stat -c '%a' "$INSTALL_CONFIG/zcode-cli-path")" == "600" ]]
[[ "$(<"$INSTALL_CONFIG/zcode-cli-path")" == "$FAKE_ZCODE" ]]
NEEDLE_ZCODE_CLI= NEEDLE_CONFIG_DIR="$INSTALL_CONFIG" \
    "$INSTALL_BIN/needle-zcode-headless" --preflight >/dev/null

printf '%s\n' "zcode-headless adapter tests passed"
