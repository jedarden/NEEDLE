#!/usr/bin/env bash
# Install the ZCode headless NEEDLE adapter.
set -euo pipefail

BIN_DIR="${HOME}/.local/bin"
ADAPTER_DIR="${HOME}/.config/needle/adapters"
CONFIG_DIR="${HOME}/.config/needle"
ZCODE_CLI=""

usage() {
    cat <<'EOF'
Usage: ./install.sh [options]

Options:
  --bin-dir DIR       Wrapper installation directory.
  --adapter-dir DIR   NEEDLE adapter directory.
  --config-dir DIR    NEEDLE configuration directory.
  --zcode-cli PATH    Persist the bundled ZCode CLI path without copying it.
  -h, --help          Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin-dir)
            BIN_DIR="$2"
            shift 2
            ;;
        --adapter-dir)
            ADAPTER_DIR="$2"
            shift 2
            ;;
        --config-dir)
            CONFIG_DIR="$2"
            shift 2
            ;;
        --zcode-cli)
            ZCODE_CLI="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            printf 'Unknown option: %s\n' "$1" >&2
            exit 2
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$BIN_DIR" "$ADAPTER_DIR" "$CONFIG_DIR"
install -m 0755 "$SCRIPT_DIR/needle-zcode-headless" "$BIN_DIR/needle-zcode-headless"
install -m 0644 "$SCRIPT_DIR/zcode-headless.yaml" "$ADAPTER_DIR/zcode-headless.yaml"

if [[ -n "$ZCODE_CLI" ]]; then
    [[ -f "$ZCODE_CLI" ]] || {
        printf 'ZCode CLI not found: %s\n' "$ZCODE_CLI" >&2
        exit 2
    }
    printf '%s\n' "$ZCODE_CLI" > "$CONFIG_DIR/zcode-cli-path"
    chmod 0600 "$CONFIG_DIR/zcode-cli-path"
fi

printf '%s\n' "Installed:"
printf '  %s\n' "$BIN_DIR/needle-zcode-headless"
printf '  %s\n' "$ADAPTER_DIR/zcode-headless.yaml"

if NEEDLE_CONFIG_DIR="$CONFIG_DIR" PATH="$BIN_DIR:$PATH" \
    "$BIN_DIR/needle-zcode-headless" --preflight; then
    printf '%s\n' "Run: needle test-agent zcode-headless"
else
    printf '%s\n' "Adapter installed, but ZCode is not ready." >&2
    printf '%s\n' "Install and configure ZCode, then rerun this installer with:" >&2
    printf '%s\n' "  --zcode-cli /path/to/ZCode/resources/glm/zcode.cjs" >&2
    exit 1
fi
