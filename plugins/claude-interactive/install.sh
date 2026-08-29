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

# Check for pyte and install if needed
if ! python3 -c "import pyte" &>/dev/null; then
  echo "pyte not found. Attempting user-space installation..."
  # Capture output to detect PEP 668 error (allow command to fail)
  INSTALL_OUTPUT=$(python3 -m pip install --user pyte 2>&1 || true)
  INSTALL_EXIT=$?

  if echo "$INSTALL_OUTPUT" | grep -q "externally-managed-environment"; then
    # PEP 668 error - provide clear guidance
    echo "Error: Python environment is externally managed (PEP 668)." >&2
    echo "Install pyte via one of these methods:" >&2
    echo "  1. pipx: pipx install pyte" >&2
    echo "  2. System package: sudo apt install python3-pyte  # Debian/Ubuntu" >&2
    echo "  3. Virtualenv on PATH: python3 -m venv ~/.venv && source ~/.venv/bin/activate && pip install pyte" >&2
    exit 1
  elif [[ $INSTALL_EXIT -ne 0 ]]; then
    # Other error - show the output and fail
    echo "$INSTALL_OUTPUT" >&2
    exit 1
  fi
  # Success - pyte installed
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
