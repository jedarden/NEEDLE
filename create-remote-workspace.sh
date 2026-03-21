#!/bin/bash
set -e

WORKSPACE="/tmp/needle-remote-e2e"
BR="/home/coding/.local/bin/br"

echo "Creating remote workspace at: $WORKSPACE"
mkdir -p "$WORKSPACE"

echo "Initializing br workspace..."
cd "$WORKSPACE"
"$BR" init

echo "Creating test bead..."
"$BR" create --title "Remote workspace test bead" --body "Test bead for E2E testing"

echo "Listing beads..."
"$BR" list

echo "Remote workspace created successfully!"
echo "Workspace path: $WORKSPACE"
