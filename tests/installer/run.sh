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

# Run the end-to-end suite first: it exercises the real install.sh against a
# mock curl and is the authoritative regression gate.
SUITES=(
    "$SCRIPT_DIR/test_e2e_install.sh"
    "$SCRIPT_DIR/test_install.sh"
    "$SCRIPT_DIR/test_checksum_verification.sh"
)

FAILED=0
for suite in "${SUITES[@]}"; do
    echo "--- running $(basename "$suite")"
    if bash "$suite"; then
        echo ""
        echo "✓ $(basename "$suite") passed"
    else
        echo ""
        echo "✗ $(basename "$suite") FAILED"
        FAILED=1
    fi
    echo ""
done

if [[ $FAILED -eq 0 ]]; then
    echo "✓ All installer tests passed"
    exit 0
else
    echo "✗ Some installer tests failed"
    exit 1
fi
