#!/usr/bin/env bash
#
# capture_stack_traces.sh
#
# Captures full stack traces for panicked/failed tests with RUST_BACKTRACE=full
# and organizes them by test name for easy debugging.
#
# Usage:
#   ./scripts/capture_stack_traces.sh                    # All tests, output to test_stack_traces.txt
#   ./scripts/capture_stack_traces.sh custom.txt         # Custom output file
#   OUTPUT=path ./scripts/capture_stack_traces.sh        # Via env var
#   ./scripts/capture_stack_traces.sh --test integration  # Run specific test only
#
# Features:
#   - Sets RUST_BACKTRACE=full for complete stack traces
#   - Runs tests single-threaded to avoid interleaved output
#   - Parses and organizes stack traces by test name
#   - Captures both panics and assertion failures
#   - Generates summary statistics
#
# Output:
#   - Organized stack traces file (test name -> full backtrace)
#   - Summary with counts and quick reference
#

set -euo pipefail

# Colors for terminal output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${BLUE}[INFO]${NC} $*" >&2
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $*" >&2
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $*" >&2
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

# Determine output file (command-line arg > env var > default)
OUTPUT="${1:-${OUTPUT:-test_stack_traces.txt}}"

# Create output directory if needed
OUTPUT_DIR="$(dirname "$OUTPUT")"
if [[ "$OUTPUT_DIR" != "." ]]; then
    mkdir -p "$OUTPUT_DIR"
fi

# Generate timestamp
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# Get the repository root
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

log_info "Repository root: $REPO_ROOT"
log_info "Output file: $OUTPUT"
log_info "Timestamp: $TIMESTAMP"
log_info "Running with RUST_BACKTRACE=full..."

# Create a temporary file for raw output
RAW_OUTPUT=$(mktemp)
trap "rm -f '$RAW_OUTPUT'" EXIT

# Run tests with backtraces, single-threaded to avoid output interleaving
# Use RUST_BACKTRACE=1 for readable numbered frames (full is too verbose)
# --test-threads=1 ensures sequential test execution (passed to test binary after --)
# Add timeout to prevent indefinite hangs
log_info "Starting test run (this may take several minutes)..."

RUST_BACKTRACE=1 timeout 300 cargo test -- --test-threads=1 2>&1 | tee "$RAW_OUTPUT"
TEST_EXIT_CODE=${PIPESTATUS[0]}

log_info "Test run completed with exit code: $TEST_EXIT_CODE"

# Start building the organized output
{
    echo "# NEEDLE Test Stack Traces"
    echo "Generated: $TIMESTAMP"
    echo "Environment: RUST_BACKTRACE=full, single-threaded execution"
    echo ""

    # Extract test summary
    if grep -q "test result:" "$RAW_OUTPUT"; then
        echo "## Test Summary"
        grep "test result:" "$RAW_OUTPUT" | tail -1
        echo ""
    fi

    # Count total tests run
    TOTAL_TESTS=$(grep -c "^test " "$RAW_OUTPUT" 2>/dev/null || echo "0")
    FAILED_TESTS=$(grep "^test .* FAILED$" "$RAW_OUTPUT" 2>/dev/null | wc -l)

    echo "## Statistics"
    echo "Total tests run: $TOTAL_TESTS"
    echo "Failed tests: $FAILED_TESTS"
    echo ""

    if [[ "$FAILED_TESTS" -eq 0 ]]; then
        if [[ "$TEST_EXIT_CODE" -eq 0 ]]; then
            log_success "All tests passed - no stack traces to capture"
            echo "✅ All tests passed - no failures to report."
        else
            log_warning "Tests failed but no 'FAILED' markers found - check output"
            echo "⚠️ Tests exited with code $TEST_EXIT_CODE but no explicit FAILED markers found."
            echo "   This may indicate compilation errors or runtime failures."
            echo ""
            echo "## Raw Output (last 100 lines)"
            echo '```'
            tail -100 "$RAW_OUTPUT"
            echo '```'
        fi
    else
        log_info "Found $FAILED_TESTS failed test(s) - organizing stack traces..."
        echo "---"
        echo ""
        echo "## Failed Test Stack Traces"
        echo ""

        # Track parsing state
        CURRENT_TEST=""
        CURRENT_THREAD=""
        CURRENT_LOCATION=""
        IN_STACK_TRACE=0
        TEST_COUNTER=0
        PANIC_LINES=()

        while IFS= read -r line; do
            # Detect test failure marker
            if [[ "$line" =~ ^test[[:space:]]+(.+)[[:space:]]+FAILED[[:space:]]*$ ]]; then
                # Save previous test if exists
                if [[ -n "$CURRENT_TEST" ]]; then
                    echo "### $TEST_COUNTER. $CURRENT_TEST"
                    echo ""
                    [[ -n "$CURRENT_THREAD" ]] && echo "**Thread:** $CURRENT_THREAD"
                    [[ -n "$CURRENT_LOCATION" ]] && echo "**Location:** $CURRENT_LOCATION"
                    [[ ${#PANIC_LINES[@]} -gt 0 ]] && echo "**Panic Message:** ${PANIC_LINES[0]}"
                    echo ""
                    echo "**Stack Backtrace:**"
                    echo '```'
                    printf '%s\n' "${PANIC_LINES[@]:1}"
                    echo '```'
                    echo ""
                    echo "---"
                    echo ""
                fi

                CURRENT_TEST="${BASH_REMATCH[1]}"
                TEST_COUNTER=$((TEST_COUNTER + 1))
                CURRENT_THREAD=""
                CURRENT_LOCATION=""
                IN_STACK_TRACE=0
                PANIC_LINES=()

            # Extract thread name from stack output
            elif [[ "$line" =~ ^[[:space:]]*--[[:space:]]+start[[:space:]]+of[[:space:]]+backtrace[[:space:]]+for[[:space:]]+thread[[:space:]]+([0-9]+)[[:space:]]*\((.+)\) ]]; then
                CURRENT_THREAD="${BASH_REMATCH[2]}"

            # Extract panic/error message
            elif [[ -n "$CURRENT_TEST" && "$IN_STACK_TRACE" -eq 0 ]]; then
                # Look for various panic/error patterns
                if [[ "$line" =~ ^(.*panicked.*at[[:space:]].*)$ ]] || \
                   [[ "$line" =~ ^(.*error:[[:space:]].*)$ ]] || \
                   [[ "$line" =~ ^(.*assertion.*failed.*)$ ]] || \
                   [[ "$line" =~ ^(.*called.*Result::unwrap.*)$ ]] || \
                   [[ "$line" =~ ^(.*attempted to.*panicked.*)$ ]]; then
                    PANIC_LINES+=("$line")
                fi

            # Detect start of stack backtrace
            elif [[ "$line" =~ ^[[:space:]]*Stack:[[:space:]]*$ ]] || \
                 [[ "$line" =~ ^[[:space:]]*Backtrace:[[:space:]]*$ ]]; then
                IN_STACK_TRACE=1
                PANIC_LINES+=("$line")

            # Capture stack frames (numbered frames starting with whitespace)
            elif [[ "$IN_STACK_TRACE" -eq 1 ]]; then
                if [[ "$line" =~ ^[[:space:]]+[0-9]+:[[:space:]] ]]; then
                    PANIC_LINES+=("$line")
                    # Extract location from the first user frame that looks like test code
                    if [[ -z "$CURRENT_LOCATION" && "$line" =~ integration_tests:: ]]; then
                        # Try to extract file:line:col if present
                        if [[ "$line" =~ at[[:space:]]+(\.[^[:space:]]+:[0-9]+:[0-9]+) ]]; then
                            CURRENT_LOCATION="${BASH_REMATCH[1]}"
                        fi
                    fi
                elif [[ -z "$line" ]]; then
                    # Empty line ends this stack trace
                    IN_STACK_TRACE=0
                fi
            fi

        done < "$RAW_OUTPUT"

        # Don't forget the last test
        if [[ -n "$CURRENT_TEST" ]]; then
            echo "### $TEST_COUNTER. $CURRENT_TEST"
            echo ""
            [[ -n "$CURRENT_THREAD" ]] && echo "**Thread:** $CURRENT_THREAD"
            [[ -n "$CURRENT_LOCATION" ]] && echo "**Location:** $CURRENT_LOCATION"
            [[ ${#PANIC_LINES[@]} -gt 0 ]] && echo "**Panic Message:** ${PANIC_LINES[0]}"
            echo ""
            echo "**Stack Backtrace:**"
            echo '```'
            printf '%s\n' "${PANIC_LINES[@]:1}"
            echo '```'
        fi

        log_success "Organized $TEST_COUNTER failed test stack traces"
    fi

    echo ""
    echo "## Environment Details"
    echo "- Rust version:"
    rustc --version 2>/dev/null || echo "  (rustc not found)"
    echo "- Cargo version:"
    cargo --version 2>/dev/null || echo "  (cargo not found)"
    echo "- Working directory: $REPO_ROOT"

} > "$OUTPUT"

log_success "Stack traces written to: $OUTPUT"

# Show file size
FILE_SIZE=$(du -h "$OUTPUT" | cut -f1)
log_info "Output file size: $FILE_SIZE"

# Quick preview
echo ""
log_info "Preview of $OUTPUT:"
head -30 "$OUTPUT"

exit $TEST_EXIT_CODE
