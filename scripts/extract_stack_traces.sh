#!/usr/bin/env bash
#
# Extract and organize stack traces from cargo test output.
# Produces a well-formatted file with complete stack traces grouped by test name.

set -euo pipefail

OUTPUT_FILE="${1:-test_stack_traces.txt}"
RAW_OUTPUT="${2:-/tmp/test_output_full.txt}"

if [[ ! -f "$RAW_OUTPUT" ]]; then
    echo "Error: Raw test output file not found: $RAW_OUTPUT"
    echo "Usage: $0 [output_file] [raw_test_output]"
    exit 1
fi

echo "Extracting stack traces from $RAW_OUTPUT..."
echo "Output will be saved to $OUTPUT_FILE"

# Create the output file with header
{
    echo "# NEEDLE Test Stack Traces"
    echo "# Generated: $(date -u +"%Y-%m-%d %H:%M:%S UTC")"
    echo "#"
    echo "# This file contains complete, untruncated stack traces for all failed/panicked tests."
    echo "# Stack traces are organized by test name for easy navigation."
    echo "#"
    echo "# Legend:"
    echo "#   - 'panicked at' indicates a panic with location"
    echo "#   - 'Stack:' indicates the full backtrace"
    echo "#   - 'Error:' indicates the assertion or test failure message"
    echo ""
    echo ""
} > "$OUTPUT_FILE"

# Process the test output to extract stack traces
current_test=""
in_test_output=false
in_stack_trace=false
test_output=()
stack_trace=()

parse_output() {
    local line="$1"

    # Detect test start: "test name::test_name ... "
    if [[ "$line" =~ ^[[:space:]]*test[[:space:]]([^[:space:]]+[[:space:]]+[^\.\.\.]+)\.\.\. ]]; then
        current_test="${BASH_REMATCH[1]}"
        current_test="${current_test#tests::}"
        current_test="${current_test#src::}"
        in_test_output=true
        test_output=()
        stack_trace=()
        in_stack_trace=false
        return
    fi

    # Detect test result: "ok", "FAILED", or "panicked"
    if [[ "$line" =~ ^[[:space:]]*test[[:space:]]+[^\.\.\.]+[[:space:]]+\.\.\.[[:space:]]+(.*) ]]; then
        local result="${BASH_REMATCH[1]}"

        if [[ "$result" == "FAILED" ]] || [[ "$result" == "panicked" ]]; then
            # Write this test's failure info
            {
                echo "## Test: $current_test"
                echo ""
                echo "**Status:** $result"
                echo ""

                # Write test output if any
                if [[ ${#test_output[@]} -gt 0 ]]; then
                    echo "**Test Output:**"
                    echo '```'
                    printf '%s\n' "${test_output[@]}"
                    echo '```'
                    echo ""
                fi

                # Write stack trace if any
                if [[ ${#stack_trace[@]} -gt 0 ]]; then
                    echo "**Stack Trace:**"
                    echo '```'
                    printf '%s\n' "${stack_trace[@]}"
                    echo '```'
                    echo ""
                fi

                echo ""
                echo "-" "$(printf '%*s' 80 '' | tr ' ' '-')"
                echo ""
            } >> "$OUTPUT_FILE"
        fi

        # Reset for next test
        current_test=""
        in_test_output=false
        in_stack_trace=false
        test_output=()
        stack_trace=()
        return
    fi

    # Collect output for the current test
    if [[ "$in_test_output" == "true" ]] && [[ -n "$current_test" ]]; then
        # Detect start of stack trace
        if [[ "$line" =~ (panicked at|thread.*panicked at|Stack:|\s+at ) ]] || [[ "$in_stack_trace" == "true" ]]; then
            in_stack_trace=true
            stack_trace+=("$line")
        else
            test_output+=("$line")
        fi
    fi
}

# Read the raw output line by line
while IFS= read -r line || [[ -n "$line" ]]; do
    parse_output "$line"
done < "$RAW_OUTPUT"

# Also capture any failures that might appear in a different format
# Some tests output failures in a different way
{
    echo ""
    echo ""
    echo "# Additional Failure Details (from error summaries)"
    echo ""
} >> "$OUTPUT_FILE"

# Extract error messages that might have been missed
grep -A 20 "panicked at" "$RAW_OUTPUT" | while IFS= read -r line; do
    echo "$line" >> "$OUTPUT_FILE"
done

echo ""
echo "✓ Stack traces extracted and saved to $OUTPUT_FILE"
echo ""

# Show summary
total_tests=$(grep -c "^test .* \.\.\." "$RAW_OUTPUT" 2>/dev/null || echo 0)
failed_tests=$(grep -c "^test .* \.\.\. FAILED" "$RAW_OUTPUT" 2>/dev/null || echo 0)
panicked_tests=$(grep -c "^test .* \.\.\. panicked" "$RAW_OUTPUT" 2>/dev/null || echo 0)

echo "Summary:"
echo "  Total tests run: $total_tests"
echo "  Failed tests: $failed_tests"
echo "  Panicked tests: $panicked_tests"
echo ""
echo "View the full stack traces in: $OUTPUT_FILE"
