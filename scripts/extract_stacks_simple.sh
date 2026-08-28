#!/usr/bin/env bash
#
# Simple stack trace extraction from cargo test output
# This script extracts complete stack traces for all test failures

set -euo pipefail

INPUT_FILE="${1:-/tmp/test_output_full.txt}"
OUTPUT_FILE="${2:-test_stack_traces.txt}"

if [[ ! -f "$INPUT_FILE" ]]; then
    echo "Error: Input file not found: $INPUT_FILE"
    echo "Usage: $0 [input_file] [output_file]"
    exit 1
fi

echo "Extracting stack traces from $INPUT_FILE..."
echo "Output will be saved to $OUTPUT_FILE"

# Create the output file with header
{
    echo "# NEEDLE Test Stack Traces"
    echo "# Generated: $(date -u +"%Y-%m-%d %H:%M:%S UTC")"
    echo "#"
    echo "# This file contains complete, untruncated stack traces for all test failures."
    echo "# Stack traces are organized by test name for easy navigation."
    echo ""
    echo ""
} > "$OUTPUT_FILE"

# Track state
current_test=""
in_stack_trace=false
declare -a stack_lines
declare -a error_lines

# Process the file line by line
while IFS= read -r line || [[ -n "$line" ]]; do

    # Detect test result line: "test tests::name::test ... FAILED"
    if [[ "$line" =~ ^[[:space:]]*test[[:space:]]+([^[:space:]]+)[[:space:]]+\.\.\.[[:space:]]+(FAILED|panicked) ]]; then
        test_name="${BASH_REMATCH[1]}"
        test_name="${test_name#tests::}"
        test_name="${test_name#src::}"
        result="${BASH_REMATCH[2]}"

        # Write previous test if we have one
        if [[ -n "$current_test" ]]; then
            write_test_entry "$current_test" "$result"
        fi

        current_test="$test_name"
        stack_lines=()
        error_lines=()
        in_stack_trace=false
        continue
    fi

    # If we're in a test, collect output
    if [[ -n "$current_test" ]]; then
        # Detect start of stack trace
        if [[ "$line" =~ (panicked at|thread.*panicked|Stack:|backtrace) ]]; then
            in_stack_trace=true
            stack_lines+=("$line")
        # Collect stack trace lines
        elif [[ "$in_stack_trace" == "true" ]]; then
            # Stack trace lines typically have frame numbers or file:line references
            if [[ "$line" =~ ^[[:space:]]*[0-9]+:[[:space:]] ]] || [[ "$line" =~ \.rs:[0-9]+ ]] || [[ "$line" =~ ^[[:space:]]+at ]] || [[ "$line" =~ ^[[:space:]]*$ ]]; then
                stack_lines+=("$line")
            # Empty line or non-stack content ends the stack trace
            elif [[ "$line" =~ ^[[:space:]]*$ ]]; then
                stack_lines+=("$line")
            else
                in_stack_trace=false
            fi
        fi
    fi

done < "$INPUT_FILE"

# Write the last test if there is one
if [[ -n "$current_test" ]]; then
    write_test_entry "$current_test" "FAILED"
fi

echo ""
echo "✓ Stack traces extracted to $OUTPUT_FILE"
echo ""

# Count results
total_found=$(grep -c "^## Test:" "$OUTPUT_FILE" 2>/dev/null || echo 0)
echo "Found $total_found test failures with stack traces"

write_test_entry() {
    local test_name="$1"
    local result="$2"

    {
        echo "## Test: $test_name"
        echo ""
        echo "**Status:** $result"
        echo ""

        # Write stack trace if we have one
        if [[ ${#stack_lines[@]} -gt 0 ]]; then
            echo "**Stack Trace:**"
            echo '```'
            printf '%s\n' "${stack_lines[@]}"
            echo '```'
            echo ""
        fi

        echo ""
        echo "-" "$(printf '%*s' 80 '' | tr ' ' '-')"
        echo ""
    } >> "$OUTPUT_FILE"

    stack_lines=()
    error_lines=()
}
