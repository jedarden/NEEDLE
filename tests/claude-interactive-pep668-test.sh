#!/usr/bin/env bash
# Test that claude-interactive-install.sh handles PEP 668 (externally-managed Python) correctly
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

echo "Testing PEP 668 error handling in claude-interactive install script..."

# Create a temporary directory for the test
TEST_DIR=$(mktemp -d)
trap "rm -rf '$TEST_DIR'" EXIT

# Create a fake pip3 that prints the PEP 668 error
FAKE_PIP="$TEST_DIR/pip3"
cat > "$FAKE_PIP" <<'EOF'
#!/usr/bin/env bash
echo "error: externally-managed-environment" >&2
echo "This Python installation is under the control of the OS and cannot be modified by the user." >&2
echo "See https://github.com/pypa/pip/issues/11698 for more information." >&2
exit 1
EOF
chmod +x "$FAKE_PIP"

# Create a test environment with fake binaries
TEST_BIN="$TEST_DIR/bin"
mkdir -p "$TEST_BIN"
cp "$FAKE_PIP" "$TEST_BIN/pip3"

# Create fake python3 that doesn't have pyte
cat > "$TEST_BIN/python3" <<'EOF'
#!/usr/bin/env bash
if [[ "$1" == "-c" && "$2" == "import pyte" ]]; then
  exit 1  # pyte not found
elif [[ "$1" == "-m" && "$2" == "pip" ]]; then
  # Simulate PEP 668 error
  echo "error: externally-managed-environment" >&2
  echo "× This Python installation is under the control of the OS and cannot be modified by the user." >&2
  echo "  See https://github.com/pypa/pip/issues/11698 for more information." >&2
  exit 1
else
  # For other python commands, exit with error
  echo "Fake python3: unexpected command: $*" >&2
  exit 1
fi
EOF
chmod +x "$TEST_BIN/python3"

# Create fake claude command
cat > "$TEST_BIN/claude" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$TEST_BIN/claude"

# Create fake install locations
TEST_BIN_DIR="$TEST_DIR/install-bin"
TEST_ADAPTER_DIR="$TEST_DIR/install-adapter"
mkdir -p "$TEST_BIN_DIR" "$TEST_ADAPTER_DIR"

# Run the install script with our test environment
export PATH="$TEST_BIN:$PATH"
export HOME="$TEST_DIR"

# Run the install script and capture output and exit code
set +e  # Allow command to fail without exiting
OUTPUT=$(./plugins/claude-interactive/install.sh \
  --bin-dir "$TEST_BIN_DIR" \
  --adapter-dir "$TEST_ADAPTER_DIR" 2>&1)
EXIT_CODE=$?
set -e  # Restore strict mode

# Check that we got the expected error message
if ! echo "$OUTPUT" | grep -q "externally managed (PEP 668)"; then
  echo "FAIL: Expected PEP 668 error message not found"
  echo "Output:"
  echo "$OUTPUT"
  exit 1
fi

# Check that the helpful guidance is present
if ! echo "$OUTPUT" | grep -q "pipx: pipx install pyte"; then
  echo "FAIL: Expected pipx guidance not found"
  echo "Output:"
  echo "$OUTPUT"
  exit 1
fi

if ! echo "$OUTPUT" | grep -q "System package:.*python3-pyte"; then
  echo "FAIL: Expected system package guidance not found"
  echo "Output:"
  echo "$OUTPUT"
  exit 1
fi

if ! echo "$OUTPUT" | grep -q "Virtualenv"; then
  echo "FAIL: Expected virtualenv guidance not found"
  echo "Output:"
  echo "$OUTPUT"
  exit 1
fi

# Check that nothing was half-installed
if [[ -f "$TEST_BIN_DIR/claude-interactive" ]]; then
  echo "FAIL: Binary was installed despite PEP 668 error"
  exit 1
fi

if [[ -f "$TEST_ADAPTER_DIR/claude-interactive.yaml" ]]; then
  echo "FAIL: Adapter config was installed despite PEP 668 error"
  exit 1
fi

# Check exit code is 1
if [[ $EXIT_CODE -ne 1 ]]; then
  echo "FAIL: Expected exit code 1, got $EXIT_CODE"
  exit 1
fi

echo "✓ PEP 668 error handling test passed"
echo "✓ Clear error message displayed"
echo "✓ Installation paths provided"
echo "✓ Nothing half-installed"
echo "✓ Exit code 1"

echo ""
echo "Testing successful installation path (pyte available)..."

# Now test that when pyte IS available, installation succeeds
TEST_BIN_DIR2="$TEST_DIR/install-bin2"
TEST_ADAPTER_DIR2="$TEST_DIR/install-adapter2"
mkdir -p "$TEST_BIN_DIR2" "$TEST_ADAPTER_DIR2"

# Create a python3 that reports pyte is available
cat > "$TEST_BIN/python3" <<'EOF'
#!/usr/bin/env bash
if [[ "$1" == "-c" && "$2" == "import pyte" ]]; then
  exit 0  # pyte found
else
  exec /usr/bin/python3 "$@"
fi
EOF

export PATH="$TEST_BIN:$PATH"
export HOME="$TEST_DIR"

OUTPUT2=$(./plugins/claude-interactive/install.sh \
  --bin-dir "$TEST_BIN_DIR2" \
  --adapter-dir "$TEST_ADAPTER_DIR2" 2>&1)

EXIT_CODE2=$?

if [[ $EXIT_CODE2 -ne 0 ]]; then
  echo "FAIL: Installation failed when pyte was available"
  echo "Output:"
  echo "$OUTPUT2"
  exit 1
fi

if [[ ! -f "$TEST_BIN_DIR2/claude-interactive" ]]; then
  echo "FAIL: Binary not installed when pyte was available"
  exit 1
fi

if [[ ! -f "$TEST_ADAPTER_DIR2/claude-interactive.yaml" ]]; then
  echo "FAIL: Adapter config not installed when pyte was available"
  exit 1
fi

echo "✓ Installation succeeds when pyte is available"

echo ""
echo "All tests passed!"
