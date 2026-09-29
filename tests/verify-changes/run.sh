#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 tests/verify-changes/test_verify_changes.py "$@"
