#!/usr/bin/env bash
# Install the claude-interactive NEEDLE adapter.
# Usage: ./install.sh [--bin-dir DIR] [--adapter-dir DIR]
set -euo pipefail

BIN_DIR="${HOME}/.local/bin"
ADAPTER_DIR="${HOME}/.config/needle/adapters"

while [[ $# -gt 0 ]]; do
  case $1 in
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --adapter-dir) ADAPTER_DIR="$2"; shift 2 ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Verify dependencies
if ! command -v claude &>/dev/null; then
  echo "Error: 'claude' CLI not found. Install Claude Code first: https://claude.ai/code" >&2
  exit 1
fi
if ! python3 -c "import pyte" &>/dev/null; then
  echo "Installing pyte (required Python dependency)..."
  pip3 install --quiet pyte
fi

mkdir -p "$BIN_DIR" "$ADAPTER_DIR"

install -m 755 "$SCRIPT_DIR/claude-interactive" "$BIN_DIR/claude-interactive"
install -m 644 "$SCRIPT_DIR/claude-interactive.yaml" "$ADAPTER_DIR/claude-interactive.yaml"

echo "Installed:"
echo "  $BIN_DIR/claude-interactive"
echo "  $ADAPTER_DIR/claude-interactive.yaml"
echo ""
echo "Ensure $BIN_DIR is on your PATH, then run:"
echo "  needle run --agent claude-interactive --workspace ."
