#!/usr/bin/env python3
"""
Extract and organize stack traces from cargo test JSON output.
Produces a human-readable file with complete stack traces grouped by test name.
"""

import json
import sys
import re
from collections import defaultdict
from pathlib import Path
from datetime import datetime


def parse_cargo_test_json(jsonl_file):
    """
    Parse cargo test JSON output and extract failures with stack traces.
    """
    failures = defaultdict(lambda: {
        "stdout": [],
        "stderr": [],
        "stack_trace": None,
        "panic_message": None,
        "location": None
    })

    current_test = None
    in_panic = False
    panic_output = []

    for line in Path(jsonl_file).read_text().splitlines():
        if not line.strip():
            continue

        try:
            event = json.loads(line)
            event_type = event.get("type", "")

            # Track which test is currently running
            if event_type == "test":
                test_name = event.get("name", "")
                if event.get("event") == "started":
                    current_test = test_name
                elif event.get("event") == "ok":
                    current_test = None
                elif event.get("event") == "failed":
                    # Test failed - keep collecting output
                    if current_test:
                        failures[current_test]["failed"] = True

            # Capture stdout/stderr
            elif event_type == "stdout" and current_test:
                failures[current_test]["stdout"].append(event.get("content", ""))

            elif event_type == "stderr" and current_test:
                stderr_content = event.get("content", "")
                failures[current_test]["stderr"].append(stderr_content)

                # Detect panic stack traces in stderr
                if "panicked at" in stderr_content:
                    in_panic = True
                    panic_output.append(stderr_content)
                elif in_panic:
                    panic_output.append(stderr_content)
                    # Stack traces end with an empty line or non-trace content
                    if not stderr_content.strip() or not any(x in stderr_content for x in ["at ", ":", "rayon", "thread"]):
                        in_panic = False
                        failures[current_test]["stack_trace"] = "\n".join(panic_output)
                        panic_output = []

            # Extract test location from failure message
            elif event_type == "test" and event.get("event") == "failed":
                # Some test failures include location in the message
                pass

        except json.JSONDecodeError:
            # Non-JSON lines (like compiler warnings or other output)
            if "panicked at" in line:
                panic_output.append(line)
            continue

    return failures


def extract_location_from_message(msg):
    """Extract file:line:col from error messages."""
    match = re.search(r'([/\w\-_]+\.rs):(\d+):(\d+)', msg)
    if match:
        return f"{match.group(1)}:{match.group(2)}:{match.group(3)}"
    return None


def format_stack_traces(failures, output_file):
    """
    Format stack traces into a readable text file organized by test name.
    """
    timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")

    with open(output_file, 'w') as f:
        f.write(f"# NEEDLE Test Stack Traces\n")
        f.write(f"# Generated: {timestamp}\n")
        f.write(f"# Total test failures with stack traces: {len(failures)}\n")
        f.write(f"#\n")
        f.write(f"# This file contains complete, untruncated stack traces for all failed/panicked tests.\n")
        f.write(f"# Stack traces are organized by test name for easy navigation.\n")
        f.write(f"#\n")
        f.write(f"#" + "=" * 76 + "\n\n")

        # Sort by test name for easier navigation
        for test_name, data in sorted(failures.items()):
            if not data.get("failed") and not data.get("stack_trace"):
                continue

            f.write(f"## Test: {test_name}\n\n")

            # Location if available
            if data.get("location"):
                f.write(f"**Location:** {data['location']}\n\n")

            # Stack trace
            if data.get("stack_trace"):
                f.write(f"**Stack Trace:**\n```\n{data['stack_trace']}\n```\n\n")
            elif data.get("stderr"):
                stderr_text = "".join(data["stderr"])
                if stderr_text.strip():
                    f.write(f"**Error Output:**\n```\n{stderr_text}\n```\n\n")

            # Stdout if it contains useful info
            stdout_text = "".join(data.get("stdout", []))
            if stdout_text.strip():
                # Only show stdout if it's not too long
                lines = stdout_text.strip().split("\n")
                if len(lines) <= 50:
                    f.write(f"**Standard Output:**\n```\n{stdout_text}\n```\n\n")
                else:
                    f.write(f"**Standard Output:** (truncated to {len(lines)} lines)\n```\n")
                    f.write("\n".join(lines[:50]))
                    f.write(f"\n... ({len(lines) - 50} more lines)\n```\n\n")

            f.write("\n" + "-" * 80 + "\n\n")


def main():
    if len(sys.argv) < 2:
        print("Usage: extract_stack_traces.py <input_jsonl> [output_file]")
        sys.exit(1)

    input_file = sys.argv[1]
    output_file = sys.argv[2] if len(sys.argv) > 2 else "test_stack_traces.txt"

    print(f"Parsing test output from {input_file}...")
    failures = parse_cargo_test_json(input_file)

    print(f"Found {len(failures)} tests with failures")
    print(f"Writing stack traces to {output_file}...")

    format_stack_traces(failures, output_file)

    print(f"✓ Stack traces saved to {output_file}")
    print(f"✓ {len(failures)} tests documented")


if __name__ == "__main__":
    main()
