#!/usr/bin/env bash
#
# NEEDLE Native Components Build Script
# Builds C/compiled components like libcheckout.so
#
# Usage:
#   ./scripts/build-native.sh              # Build all native components
#   ./scripts/build-native.sh --lib-only   # Build only libcheckout.so
#   ./scripts/build-native.sh --clean      # Remove built artifacts
#
# Output:
#   ~/.needle/lib/libcheckout.so - LD_PRELOAD library for file lock enforcement

set -euo pipefail

# -----------------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
OUTPUT_DIR="${NEEDLE_HOME:-$HOME/.needle}/lib"
LIB_NAME="libcheckout.so"
SOURCE_FILE="$ROOT_DIR/src/lock/libcheckout.c"

# -----------------------------------------------------------------------------
# Parse Arguments
# -----------------------------------------------------------------------------

LIB_ONLY=false
CLEAN=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lib-only|-l)
            LIB_ONLY=true
            shift
            ;;
        --clean|-c)
            CLEAN=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --lib-only, -l    Build only libcheckout.so"
            echo "  --clean, -c       Remove built artifacts"
            echo "  --help, -h        Show this help"
            echo ""
            echo "Output: $OUTPUT_DIR/$LIB_NAME"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

# -----------------------------------------------------------------------------
# Clean Function
# -----------------------------------------------------------------------------

do_clean() {
    echo "Cleaning native components..."
    rm -f "$OUTPUT_DIR/$LIB_NAME"
    echo "  Removed: $OUTPUT_DIR/$LIB_NAME"
    echo "Done."
}

# -----------------------------------------------------------------------------
# Build Functions
# -----------------------------------------------------------------------------

check_dependencies() {
    # Check for gcc
    if ! command -v gcc &>/dev/null; then
        echo "ERROR: gcc not found. Please install gcc to build native components." >&2
        return 1
    fi

    # Check for source file
    if [[ ! -f "$SOURCE_FILE" ]]; then
        echo "ERROR: Source file not found: $SOURCE_FILE" >&2
        return 1
    fi
}

build_libcheckout() {
    echo "Building $LIB_NAME..."

    # Create output directory
    mkdir -p "$OUTPUT_DIR"

    # Compile the shared library
    # -shared: Create a shared library
    # -fPIC: Position-independent code (required for shared libraries)
    # -o: Output file
    # -ldl: Link against libdl for dlsym
    # -O2: Optimization level 2 (good balance of speed and size)
    # -Wall -Wextra: Enable warnings
    gcc -shared -fPIC -O2 -Wall -Wextra \
        -o "$OUTPUT_DIR/$LIB_NAME" \
        "$SOURCE_FILE" \
        -ldl

    # Verify build
    if [[ -f "$OUTPUT_DIR/$LIB_NAME" ]]; then
        local size
        size=$(stat -c%s "$OUTPUT_DIR/$LIB_NAME" 2>/dev/null || stat -f%z "$OUTPUT_DIR/$LIB_NAME" 2>/dev/null)
        echo "  Built: $OUTPUT_DIR/$LIB_NAME ($size bytes)"
    else
        echo "ERROR: Build failed - $LIB_NAME not created" >&2
        return 1
    fi
}

# -----------------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------------

if [[ "$CLEAN" == "true" ]]; then
    do_clean
    exit 0
fi

echo "Building NEEDLE native components..."
echo ""

check_dependencies

if [[ "$LIB_ONLY" == "true" ]] || [[ "$#" -eq 0 ]]; then
    build_libcheckout
fi

echo ""
echo "Build complete!"
echo ""
echo "Usage:"
echo "  LD_PRELOAD=$OUTPUT_DIR/$LIB_NAME NEEDLE_BEAD_ID=nd-xxx <command>"
echo ""
echo "Environment variables:"
echo "  NEEDLE_LOCK_DIR       - Override lock directory (default: /dev/shm/needle)"
echo "  NEEDLE_PRELOAD_DEBUG  - Set to '1' to enable debug logging"
echo "  NEEDLE_BEAD_ID        - Current bead ID (locks held by this bead are allowed)"
