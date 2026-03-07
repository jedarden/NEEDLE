#!/usr/bin/env bash
# NEEDLE CLI Pulse Subcommand
# Run pulse scans manually to detect codebase health issues

_needle_pulse_help() {
    _needle_print "Run pulse scans manually to detect codebase health issues

Pulse monitors codebase health metrics and creates beads for detected issues.
When run manually, pulse bypasses the normal frequency limit.

USAGE:
    needle pulse [OPTIONS]

OPTIONS:
    -w, --workspace <PATH>     Workspace to scan (default: current directory)
    -d, --detectors <LIST>     Comma-separated detectors to run
                               Available: security, dependencies, docs, coverage, todos
                               (default: all enabled detectors)
    --dry-run                  Show what would be detected without creating beads
    --max-beads <N>            Maximum beads to create per run (default: from config)
    --force                    Bypass frequency limit and run immediately
    --reset                    Reset pulse state (clears seen issues)
    -j, --json                 Output results in JSON format
    -h, --help                 Show this help message

DETECTORS:
    security      Scan for vulnerabilities in dependencies (npm audit, pip-audit)
    dependencies  Check for stale/outdated dependencies beyond threshold
    docs          Detect documentation drift (code changed, docs didn't)
    coverage      Identify test coverage gaps (new code without tests)
    todos         Find stale TODO/FIXME/HACK comments older than threshold

EXAMPLES:
    # Run all detectors on current workspace
    needle pulse

    # Run specific detectors
    needle pulse --detectors=security,dependencies

    # Preview issues without creating beads
    needle pulse --dry-run

    # Scan a specific workspace
    needle pulse --workspace=/path/to/project

    # Force immediate scan (bypass frequency limit)
    needle pulse --force

CONFIGURATION:
    Pulse behavior can be configured in .needle.yaml:

    strands:
      pulse:
        enabled: true
        frequency: 24h
        max_beads_per_run: 5
"
}

_needle_pulse() {
    local workspace=""
    local detectors=""
    local dry_run=false
    local max_beads=""
    local force=false
    local reset=false
    local json_output=false

    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -w|--workspace)
                workspace="$2"
                shift 2
                ;;
            --workspace=*)
                workspace="${1#*=}"
                shift
                ;;
            -d|--detectors)
                detectors="$2"
                shift 2
                ;;
            --detectors=*)
                detectors="${1#*=}"
                shift
                ;;
            --dry-run)
                dry_run=true
                shift
                ;;
            --max-beads)
                max_beads="$2"
                shift 2
                ;;
            --max-beads=*)
                max_beads="${1#*=}"
                shift
                ;;
            --force)
                force=true
                shift
                ;;
            --reset)
                reset=true
                shift
                ;;
            -j|--json)
                json_output=true
                shift
                ;;
            -h|--help)
                _needle_pulse_help
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            -*)
                _needle_error "Unknown option: $1"
                _needle_pulse_help
                exit $NEEDLE_EXIT_USAGE
                ;;
            *)
                # Positional argument - treat as workspace
                if [[ -z "$workspace" ]]; then
                    workspace="$1"
                else
                    _needle_error "Unexpected argument: $1"
                    _needle_pulse_help
                    exit $NEEDLE_EXIT_USAGE
                fi
                shift
                ;;
        esac
    done

    # Default workspace to current directory
    if [[ -z "$workspace" ]]; then
        workspace="$(pwd)"
    fi

    # Resolve to absolute path
    if [[ ! "$workspace" = /* ]]; then
        workspace="$(cd "$workspace" 2>/dev/null && pwd)" || {
            _needle_error "Workspace not found: $workspace"
            exit $NEEDLE_EXIT_USAGE
        }
    fi

    # Validate workspace exists
    if [[ ! -d "$workspace" ]]; then
        _needle_error "Workspace directory not found: $workspace"
        exit $NEEDLE_EXIT_USAGE
    fi

    # Source pulse strand module
    source "$NEEDLE_ROOT_DIR/src/strands/pulse.sh"

    # Handle reset
    if [[ "$reset" == "true" ]]; then
        _pulse_reset "$workspace"
        if [[ "$json_output" == "true" ]]; then
            jq -n --arg workspace "$workspace" '{reset: true, workspace: $workspace}'
        else
            _needle_success "Pulse state reset for: $workspace"
        fi
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Check frequency limit unless forced
    if [[ "$force" != "true" ]]; then
        if ! _pulse_should_run "$workspace"; then
            if [[ "$json_output" == "true" ]]; then
                jq -n --arg workspace "$workspace" '{skipped: true, reason: "frequency_limit"}'
            else
                _needle_info "Pulse scan skipped (frequency limit not reached)"
                _needle_info "Use --force to bypass the limit"
            fi
            exit $NEEDLE_EXIT_SUCCESS
        fi
    else
        # Clear rate limit when forced
        _pulse_clear_rate_limit "$workspace"
    fi

    # If specific detectors requested, we need to filter
    # For now, we run all enabled detectors and filter during collection
    # This is a simplified implementation - full implementation would filter detectors

    if [[ "$dry_run" == "true" ]]; then
        # Dry run - collect issues but don't create beads
        _needle_pulse_dry_run "$workspace" "$detectors" "$json_output"
    else
        # Normal run - collect and process issues
        _needle_pulse_run "$workspace" "$detectors" "$max_beads" "$json_output"
    fi
}

# Dry run - show what would be detected
_needle_pulse_dry_run() {
    local workspace="$1"
    local detectors="$2"
    local json_output="$3"

    local agent="pulse-cli"

    if [[ "$json_output" != "true" ]]; then
        _needle_info "Running pulse scan (dry-run) on: $workspace"
        _needle_print ""
    fi

    # Clean old seen issues
    _pulse_clean_seen_issues "$workspace"

    # Collect issues from detectors
    local issues
    issues=$(_needle_pulse_collect_filtered "$workspace" "$agent" "$detectors")

    # Count issues
    local issue_count
    issue_count=$(echo "$issues" | jq 'length' 2>/dev/null || echo 0)

    if [[ "$json_output" == "true" ]]; then
        # Output JSON
        jq -n \
            --arg workspace "$workspace" \
            --argjson issue_count "$issue_count" \
            --argjson issues "$issues" \
            --arg dry_run "true" \
            '{
                workspace: $workspace,
                dry_run: $dry_run,
                issues_found: $issue_count,
                issues: $issues
            }'
    else
        # Output human-readable
        if [[ "$issue_count" -eq 0 ]]; then
            _needle_success "No issues detected"
        else
            _needle_print_color "$NEEDLE_COLOR_BOLD" "Issues that would be detected ($issue_count):"
            _needle_print ""

            echo "$issues" | jq -r '.[] | "  [\(.severity)] \(.title)\n    Category: \(.category)\n    Fingerprint: \(.fingerprint)\n"' 2>/dev/null | while read -r line; do
                _needle_print "$line"
            done
        fi
    fi

    # Record scan (even in dry run, to update frequency tracking)
    _pulse_record_scan "$workspace"

    exit $NEEDLE_EXIT_SUCCESS
}

# Normal run - collect and create beads
_needle_pulse_run() {
    local workspace="$1"
    local detectors="$2"
    local max_beads="$3"
    local json_output="$4"

    local agent="pulse-cli"

    if [[ "$json_output" != "true" ]]; then
        _needle_info "Running pulse scan on: $workspace"
    fi

    # Clean old seen issues
    _pulse_clean_seen_issues "$workspace"

    # Collect issues from detectors
    local issues
    issues=$(_needle_pulse_collect_filtered "$workspace" "$agent" "$detectors")

    # Count issues
    local issue_count
    issue_count=$(echo "$issues" | jq 'length' 2>/dev/null || echo 0)

    if [[ -z "$issues" ]] || [[ "$issues" == "[]" ]] || [[ "$issue_count" -eq 0 ]]; then
        # Record scan even when no issues found
        _pulse_record_scan "$workspace"

        if [[ "$json_output" == "true" ]]; then
            jq -n \
                --arg workspace "$workspace" \
                --argjson issues_found 0 \
                --argjson beads_created 0 \
                '{
                    workspace: $workspace,
                    issues_found: $issues_found,
                    beads_created: $beads_created
                }'
        else
            _needle_success "No issues detected"
        fi

        exit $NEEDLE_EXIT_SUCCESS
    fi

    if [[ "$json_output" != "true" ]]; then
        _needle_info "Found $issue_count issue(s)"
    fi

    # Process issues and create beads
    local created=0
    local process_max="$max_beads"

    # Get max beads from config if not specified
    if [[ -z "$process_max" ]]; then
        process_max=$(get_config "strands.pulse.max_beads_per_run" "5")
    fi

    # Process each issue
    local idx=0
    while ((idx < issue_count)) && ((created < process_max)); do
        local issue
        issue=$(echo "$issues" | jq -c ".[$idx]" 2>/dev/null)

        local category title description fingerprint severity labels
        category=$(echo "$issue" | jq -r '.category // "maintenance"' 2>/dev/null)
        title=$(echo "$issue" | jq -r '.title // "Untitled issue"' 2>/dev/null)
        description=$(echo "$issue" | jq -r '.description // ""' 2>/dev/null)
        fingerprint=$(echo "$issue" | jq -r '.fingerprint // ""' 2>/dev/null)
        severity=$(echo "$issue" | jq -r '.severity // "medium"' 2>/dev/null)
        labels=$(echo "$issue" | jq -r '.labels // ""' 2>/dev/null)

        # Create bead
        if _pulse_create_bead "$workspace" "$category" "$title" "$description" "$fingerprint" "$severity" "$labels" 2>/dev/null; then
            ((created++))
        fi

        ((idx++))
    done

    # Record scan completion
    _pulse_record_scan "$workspace"

    if [[ "$json_output" == "true" ]]; then
        jq -n \
            --arg workspace "$workspace" \
            --argjson issues_found "$issue_count" \
            --argjson beads_created "$created" \
            '{
                workspace: $workspace,
                issues_found: $issues_found,
                beads_created: $beads_created
            }'
    else
        if [[ "$created" -gt 0 ]]; then
            _needle_success "Created $created bead(s) from pulse scan"
        else
            _needle_info "No beads created (all issues were duplicates or filtered)"
        fi
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

# Collect issues with optional detector filtering
_needle_pulse_collect_filtered() {
    local workspace="$1"
    local agent="$2"
    local detectors="$3"

    local all_issues="[]"

    # Parse detector filter
    local run_security=true
    local run_dependencies=true
    local run_docs=true
    local run_coverage=true
    local run_todos=true

    if [[ -n "$detectors" ]]; then
        # Disable all, then enable only specified
        run_security=false
        run_dependencies=false
        run_docs=false
        run_coverage=false
        run_todos=false

        IFS=',' read -ra detector_list <<< "$detectors"
        for detector in "${detector_list[@]}"; do
            detector=$(echo "$detector" | tr -d ' ')
            case "$detector" in
                security) run_security=true ;;
                dependencies|deps) run_dependencies=true ;;
                docs|documentation) run_docs=true ;;
                coverage|tests) run_coverage=true ;;
                todos|todo) run_todos=true ;;
            esac
        done
    fi

    # Run enabled detectors
    if [[ "$run_security" == "true" ]] && declare -f _pulse_detector_security &>/dev/null; then
        local security_issues
        security_issues=$(_pulse_detector_security "$workspace" "$agent")
        if [[ -n "$security_issues" ]] && [[ "$security_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$security_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    if [[ "$run_dependencies" == "true" ]] && declare -f _pulse_detector_dependencies &>/dev/null; then
        local dep_issues
        dep_issues=$(_pulse_detector_dependencies "$workspace" "$agent")
        if [[ -n "$dep_issues" ]] && [[ "$dep_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$dep_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    if [[ "$run_docs" == "true" ]] && declare -f _pulse_detector_docs &>/dev/null; then
        local doc_issues
        doc_issues=$(_pulse_detector_docs "$workspace" "$agent")
        if [[ -n "$doc_issues" ]] && [[ "$doc_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$doc_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    if [[ "$run_coverage" == "true" ]] && declare -f _pulse_detector_coverage &>/dev/null; then
        local coverage_issues
        coverage_issues=$(_pulse_detector_coverage "$workspace" "$agent")
        if [[ -n "$coverage_issues" ]] && [[ "$coverage_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$coverage_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    if [[ "$run_todos" == "true" ]] && declare -f _pulse_detector_todos &>/dev/null; then
        local todo_issues
        todo_issues=$(_pulse_detector_todos "$workspace" "$agent")
        if [[ -n "$todo_issues" ]] && [[ "$todo_issues" != "[]" ]]; then
            all_issues=$(echo "$all_issues" "$todo_issues" | jq -s 'add' 2>/dev/null || echo "$all_issues")
        fi
    fi

    echo "$all_issues"
}
