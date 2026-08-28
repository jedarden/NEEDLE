#!/usr/bin/env bash
#
# Comprehensive stack trace capture for NEEDLE test failures
# Runs tests with full backtraces and extracts organized, readable output

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

OUTPUT_DIR="${1:-/tmp/needle_test_results}"
TIMESTAMP=$(date -u +"%Y%m%d_%H%M%S")
OUTPUT_BASE="${OUTPUT_DIR}/stack_traces_${TIMESTAMP}"

mkdir -p "$OUTPUT_DIR"

echo -e "${YELLOW}Capturing NEEDLE test stack traces${NC}"
echo "Output directory: ${OUTPUT_BASE}"
echo ""

# Step 1: Run tests with full backtraces
echo -e "${YELLOW}[1/3] Running tests with full backtraces...${NC}"

RUST_BACKTRACE=full timeout 600 cargo test --no-fail-fast -- --test-threads=1 \
    2>&1 | tee "${OUTPUT_BASE}_raw.txt" \
    || true

echo ""

# Step 2: Parse and organize stack traces
echo -e "${YELLOW}[2/3] Extracting and organizing stack traces...${NC}"

cat > "${OUTPUT_BASE}_organized.txt" << 'EOF'
# NEEDLE Test Stack Traces
#
# This file contains complete, untruncated stack traces for all failed/panicked tests.
# Stack traces are organized by test name for easy navigation.
#
# Quick navigation: Search for "## Test:" to jump to specific test failures
EOF

echo "" >> "${OUTPUT_BASE}_organized.txt"
echo "Generated: $(date -u +"%Y-%m-%d %H:%M:%S UTC")" >> "${OUTPUT_BASE}_organized.txt"
echo "" >> "${OUTPUT_BASE}_organized.txt"
echo "=" "$(printf '%*s' 76 '' | tr ' ' '-')" >> "${OUTPUT_BASE}_organized.txt"
echo "" >> "${OUTPUT_BASE}_organized.txt"

# Parse the test output
current_test=""
test_failed=false
test_panicked=false
in_output=false
output_buffer=()
stack_buffer=()

process_test_line() {
    local line="$1"

    # Detect test: "test tests::module::test_name ... "
    if [[ "$line" =~ ^[[:space:]]*test[[:space:]]+([^[:space:]]+::[^[:space:]]+|[^\s:]+::[^\s:]+)[[:space:]]+\.\.\. ]]; then
        # Save previous test if it failed
        if [[ "$test_failed" == true ]] || [[ "$test_panicked" == true ]]; then
            write_test_entry
        fi

        current_test="${BASH_REMATCH[1]}"
        current_test="${current_test#tests::}"
        current_test="${current_test#src::}"
        test_failed=false
        test_panicked=false
        in_output=false
        output_buffer=()
        stack_buffer=()
        return
    fi

    # Detect test result
    if [[ "$line" =~ ^[[:space:]]*test[[:space:]]+.*\.\.\.[[:space:]]+(.*) ]]; then
        local result="${BASH_REMATCH[1]}"

        if [[ "$result" == "FAILED" ]]; then
            test_failed=true
        elif [[ "$result" == "panicked" ]]; then
            test_panicked=true
        fi

        # Save this test if it failed
        if [[ "$test_failed" == true ]] || [[ "$test_panicked" == true ]]; then
            write_test_entry
        fi

        # Reset for next test
        current_test=""
        test_failed=false
        test_panicked=false
        in_output=false
        output_buffer=()
        stack_buffer=()
        return
    fi

    # Capture output for current test
    if [[ -n "$current_test" ]]; then
        # Detect panic/stack trace indicators
        if [[ "$line" =~ (panicked at|thread.*panicked|Stack:|backtrace|[0-9]+:[[:space:]]+.*\.rs:[0-9]+) ]]; then
            stack_buffer+=("$line")
            in_output=true
        elif [[ "$in_output" == "true" ]]; then
            # Continue capturing output
            output_buffer+=("$line")
            # Also add to stack if we're in a stack trace
            if [[ "$line" =~ ^[[:space:]]+[0-9]+: ]]; then
                stack_buffer+=("$line")
            fi
        fi
    fi
}

write_test_entry() {
    if [[ -z "$current_test" ]]; then
        return
    fi

    {
        echo "## Test: $current_test"
        echo ""

        if [[ "$test_panicked" == true ]]; then
            echo "**Status:** PANICKED"
        elif [[ "$test_failed" == true ]]; then
            echo "**Status:** FAILED"
        fi

        echo ""

        # Write test output
        if [[ ${#output_buffer[@]} -gt 0 ]]; then
            echo "**Test Output:**"
            echo '```'
            printf '%s\n' "${output_buffer[@]}" | head -100
            if [[ ${#output_buffer[@]} -gt 100 ]]; then
                echo ""
                echo "... (output truncated, see raw file for complete output)"
            fi
            echo '```'
            echo ""
        fi

        # Write stack trace
        if [[ ${#stack_buffer[@]} -gt 0 ]]; then
            echo "**Stack Trace:**"
            echo '```'
            printf '%s\n' "${stack_buffer[@]}"
            echo '```'
            echo ""
        fi

        echo ""
        echo "-" "$(printf '%*s' 80 '' | tr ' ' '-')"
        echo ""
    } >> "${OUTPUT_BASE}_organized.txt"
}

# Process each line of the raw output
while IFS= read -r line || [[ -n "$line" ]]; do
    process_test_line "$line"
done < "${OUTPUT_BASE}_raw.txt"

# Step 3: Create summary
echo -e "${YELLOW}[3/3] Creating summary...${NC}"

total_tests=$(grep -c "^test .* \.\.\." "${OUTPUT_BASE}_raw.txt" 2>/dev/null || echo 0)
failed_tests=$(grep -c "^test .* \.\.\. FAILED" "${OUTPUT_BASE}_raw.txt" 2>/dev/null || echo 0)
panicked_tests=$(grep -c "^test .* \.\.\. panicked" "${OUTPUT_BASE}_raw.txt" 2>/dev/null || echo 0)

cat > "${OUTPUT_BASE}_summary.txt" << EOF
NEEDLE Test Stack Trace Summary
===============================
Generated: $(date -u +"%Y-%m-%d %H:%M:%S UTC")

Test Results:
-------------
Total tests run:  $total_tests
Failed tests:     $failed_tests
Panicked tests:  $panicked_tests

Files Generated:
----------------
1. ${OUTPUT_BASE}_raw.txt           - Complete raw test output
2. ${OUTPUT_BASE}_organized.txt     - Organized stack traces by test name
3. ${OUTPUT_BASE}_summary.txt       - This summary file

Quick Navigation:
-----------------
- Open ${OUTPUT_BASE}_organized.txt
- Search for "## Test:" followed by the test name
- Each test failure includes:
  * Status (FAILED or PANICKED)
  * Test output (truncated to 100 lines)
  * Complete stack trace

Next Steps:
-----------
1. Review ${OUTPUT_BASE}_organized.txt for detailed failure information
2. Use raw.txt for complete untruncated output
3. Focus on tests with actual stack traces (not just assertion failures)
EOF

cat "${OUTPUT_BASE}_summary.txt"

echo ""
echo -e "${GREEN}✓ Stack traces captured successfully!${NC}"
echo ""
echo "Files created:"
echo "  - ${OUTPUT_BASE}_organized.txt (organized by test name)"
echo "  - ${OUTPUT_BASE}_raw.txt (complete raw output)"
echo "  - ${OUTPUT_BASE}_summary.txt (summary)"
echo ""
echo "View organized traces: cat ${OUTPUT_BASE}_organized.txt"
