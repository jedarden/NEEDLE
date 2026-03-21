#!/bin/bash
# Launch needle worker with home workspace
#
# Usage:
#   ./launch-worker.sh              # Launch with defaults
#   TMUX=fake ./launch-worker.sh    # Launch without tmux (for testing)
#   WORKER_ID=bravo ./launch-worker.sh  # Use custom worker ID
#
# Environment variables:
#   TMUX        - Set to "fake" to disable tmux session creation (for testing)
#   RUST_LOG    - Set log level (info, debug, trace)
#   WORKER_ID   - Custom worker identifier (default: home-alpha)
#   WORKSPACE   - Custom workspace path (default: /home/coding/NEEDLE)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="${WORKSPACE:-$SCRIPT_DIR}"
NEEDLE_BIN="$PROJECT_ROOT/target/release/needle"
WORKER_ID="${WORKER_ID:-home-alpha}"

# Check if binary exists
if [ ! -x "$NEEDLE_BIN" ]; then
    echo "Error: needle binary not found at $NEEDLE_BIN"
    echo "Run: cargo build --release"
    exit 1
fi

# Set TMUX to fake if not already set (avoids tmux session creation)
export TMUX="${TMUX:-fake}"

# Enable logging if not already set
export RUST_LOG="${RUST_LOG:-info}"

# Launch the worker
echo "Launching needle worker with home workspace: $PROJECT_ROOT"
echo "Binary: $NEEDLE_BIN"
echo "Worker ID: $WORKER_ID"
echo "TMUX: $TMUX"
echo ""

exec "$NEEDLE_BIN" run \
    --workspace "$PROJECT_ROOT" \
    --count 1 \
    --identifier "$WORKER_ID"
