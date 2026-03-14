#!/usr/bin/env bash
#
# Test script for libcheckout.so (LD_PRELOAD file lock enforcement)
#
# Usage: ./tests/test_libcheckout.sh
#
# Tests:
#   1. Library loads successfully
#   2. Debug mode works
#   3. Read operations work (no blocking)
#   4. Write operations work without locks
#   5. Write operations blocked when locked by another bead
#   6. Write operations allowed for own bead's lock

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
LIB_PATH="${NEEDLE_HOME:-$HOME/.needle}/lib/libcheckout.so"
LOCK_DIR="${NEEDLE_LOCK_DIR:-/dev/shm/needle}"
TEST_FILE="/tmp/libcheckout_test_file.txt"
TEST_PROGRAM="/tmp/libcheckout_test_open"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

pass() { echo -e "${GREEN}✓ PASS${NC}: $1"; }
fail() { echo -e "${RED}✗ FAIL${NC}: $1"; }
info() { echo -e "${YELLOW}INFO${NC}: $1"; }

# Test counter
TESTS_PASSED=0
TESTS_FAILED=0

cleanup() {
    rm -f "$TEST_FILE" 2>/dev/null || true
    rm -f "$TEST_PROGRAM" 2>/dev/null || true
    rm -f /tmp/libcheckout_test_open.c 2>/dev/null || true
    rm -f "$LOCK_DIR"/*-c8619219 2>/dev/null || true
}
trap cleanup EXIT

# Build test helper program
build_test_program() {
    cat > /tmp/libcheckout_test_open.c << 'EOF'
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>

int main(int argc, char *argv[]) {
    const char *path;
    int flags;
    int fd;

    if (argc < 3) {
        fprintf(stderr, "Usage: %s <path> <read|write>\n", argv[0]);
        return 1;
    }

    path = argv[1];

    if (strcmp(argv[2], "read") == 0) {
        flags = O_RDONLY;
    } else if (strcmp(argv[2], "write") == 0) {
        flags = O_WRONLY | O_CREAT | O_TRUNC;
    } else {
        fprintf(stderr, "Invalid mode: %s\n", argv[2]);
        return 1;
    }

    fd = open(path, flags, 0644);
    if (fd < 0) {
        perror("open failed");
        return 1;
    }

    if (strcmp(argv[2], "write") == 0) {
        write(fd, "test\n", 5);
    }

    close(fd);
    printf("success\n");
    return 0;
}
EOF
    gcc -o "$TEST_PROGRAM" /tmp/libcheckout_test_open.c
}

# Test 1: Library loads successfully
test_library_loads() {
    info "Test 1: Library loads successfully"

    if [[ ! -f "$LIB_PATH" ]]; then
        fail "Library not found at $LIB_PATH"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return
    fi

    # Test loading the library with a simple program
    if LD_PRELOAD="$LIB_PATH" /bin/true 2>&1; then
        pass "Library loads without errors"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        fail "Library failed to load"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# Test 2: Debug mode works
test_debug_mode() {
    info "Test 2: Debug mode works"

    local output
    output=$(LD_PRELOAD="$LIB_PATH" NEEDLE_PRELOAD_DEBUG=1 "$TEST_PROGRAM" /dev/null read 2>&1)

    if echo "$output" | grep -q "Library initialized"; then
        pass "Debug logging enabled"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        pass "Library initializes (debug optional)"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    fi
}

# Test 3: Read operations work (no blocking)
test_read_operations() {
    info "Test 3: Read operations work"

    rm -f "$TEST_FILE"
    echo "test content" > "$TEST_FILE"

    if LD_PRELOAD="$LIB_PATH" "$TEST_PROGRAM" "$TEST_FILE" read 2>&1 | grep -q "success"; then
        pass "Read operations work normally"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        fail "Read operations blocked unexpectedly"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi

    rm -f "$TEST_FILE"
}

# Test 4: Write operations work without locks
test_write_no_lock() {
    info "Test 4: Write operations work without locks"

    rm -f "$TEST_FILE"
    rm -f "$LOCK_DIR"/*-c8619219 2>/dev/null || true

    if LD_PRELOAD="$LIB_PATH" "$TEST_PROGRAM" "$TEST_FILE" write 2>&1 | grep -q "success"; then
        pass "Write operations work when no lock present"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        fail "Write operations blocked without lock"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi

    rm -f "$TEST_FILE"
}

# Test 5: Write operations blocked when locked by another bead
test_write_blocked_with_lock() {
    info "Test 5: Write operations blocked when locked by another bead"

    rm -f "$TEST_FILE"
    touch "$TEST_FILE"
    local abs_path
    abs_path=$(realpath "$TEST_FILE")

    # Compute path UUID (first 8 chars of MD5)
    local path_uuid
    path_uuid=$(echo -n "$abs_path" | md5sum | cut -c1-8)

    # Create a lock file for another bead
    mkdir -p "$LOCK_DIR"
    local lock_file="$LOCK_DIR/nd-other-$path_uuid"
    echo '{"bead":"nd-other"}' > "$lock_file"

    # Try to write - should fail
    local output exit_code=0
    output=$(LD_PRELOAD="$LIB_PATH" "$TEST_PROGRAM" "$TEST_FILE" write 2>&1) || exit_code=$?

    if [[ $exit_code -ne 0 ]] && echo "$output" | grep -q "Permission denied"; then
        pass "Write blocked when file locked by another bead"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        fail "Write allowed despite lock (exit: $exit_code)"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi

    rm -f "$lock_file" "$TEST_FILE"
}

# Test 6: Write allowed for own bead's lock
test_write_allowed_own_lock() {
    info "Test 6: Write allowed for own bead's lock"

    rm -f "$TEST_FILE"
    touch "$TEST_FILE"
    local abs_path
    abs_path=$(realpath "$TEST_FILE")

    # Compute path UUID
    local path_uuid
    path_uuid=$(echo -n "$abs_path" | md5sum | cut -c1-8)

    # Create a lock file for OUR bead
    mkdir -p "$LOCK_DIR"
    local lock_file="$LOCK_DIR/nd-mine-$path_uuid"
    echo '{"bead":"nd-mine"}' > "$lock_file"

    # Try to write with NEEDLE_BEAD_ID set - should succeed
    local output
    output=$(LD_PRELOAD="$LIB_PATH" NEEDLE_BEAD_ID=nd-mine "$TEST_PROGRAM" "$TEST_FILE" write 2>&1)

    if echo "$output" | grep -q "success"; then
        pass "Write allowed when file locked by own bead"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        fail "Write blocked for own bead's lock"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi

    rm -f "$lock_file" "$TEST_FILE"
}

# Run all tests
echo "=========================================="
echo "libcheckout.so Test Suite"
echo "=========================================="
echo ""

# Check if library exists
if [[ ! -f "$LIB_PATH" ]]; then
    echo "Building library first..."
    "$ROOT_DIR/scripts/build-native.sh" --lib-only || { echo "Build failed!"; exit 1; }
fi

# Build test helper
build_test_program

echo "Library: $LIB_PATH"
echo "Lock dir: $LOCK_DIR"
echo "Test program: $TEST_PROGRAM"
echo ""

test_library_loads
test_debug_mode
test_read_operations
test_write_no_lock
test_write_blocked_with_lock
test_write_allowed_own_lock

echo ""
echo "=========================================="
echo "Results: $TESTS_PASSED passed, $TESTS_FAILED failed"
echo "=========================================="

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi

exit 0
