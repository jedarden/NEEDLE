#!/bin/bash
#
# Test runner for installer tests
# Can be invoked standalone or integrated into CI
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel)"

echo "========================================="
echo "NEEDLE Installer Test Suite"
echo "========================================="
echo ""

# Run the comprehensive installer tests
if bash "$SCRIPT_DIR/test_install.sh"; then
    echo ""
    echo "✓ All installer tests passed"
    exit 0
else
    echo ""
    echo "✗ Some installer tests failed"
    exit 1
fi
