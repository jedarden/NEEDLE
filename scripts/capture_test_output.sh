#!/bin/bash
# capture_test_output.sh
#
# Captures full stdout and stderr from cargo test to a single output file.
# Useful for debugging test failures or archiving test results.
#
# Usage:
#   ./scripts/capture_test_output.sh                    # Uses default output: test_output.txt
#   ./scripts/capture_test_output.sh custom_output.txt  # Uses custom output file
#   OUTPUT=path/to/file.txt ./scripts/capture_test_output.sh  # Via environment variable
#
# Output file is determined by (in order of precedence):
#   1. Command-line argument
#   2. OUTPUT environment variable
#   3. Default: test_output.txt
#
# The script runs cargo test and redirects both stdout and stderr to the
# output file using the shell redirection syntax: > "$OUTPUT" 2>&1
#
# Returns:
#   - Exit code 0: All tests passed
#   - Exit code 101: Tests failed (cargo test's exit code for failures)
#   - Other exit codes: Compilation or runtime errors

set -euo pipefail

# Determine output file path (command-line arg > env var > default)
OUTPUT="${1:-${OUTPUT:-test_output.txt}}"

# Create output directory if it doesn't exist
OUTPUT_DIR="$(dirname "$OUTPUT")"
if [[ "$OUTPUT_DIR" != "." ]]; then
    mkdir -p "$OUTPUT_DIR"
fi

echo "Capturing cargo test output to: $OUTPUT"
echo "Running: cargo test"
echo "---"

# Run cargo test, capturing both stdout and stderr to the output file
# Using > "$OUTPUT" 2>&1 redirects stdout to the file, then stderr to the same place
cargo test > "$OUTPUT" 2>&1

CARGO_EXIT=$?

echo "---"
echo "Test output captured to: $OUTPUT"

# Return the actual cargo exit code for downstream tooling
exit $CARGO_EXIT
