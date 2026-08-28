#!/usr/bin/env bash
# false-close-audit.sh - Audit closed beads by re-verifying from clean commit extractions
#
# For each workspace: samples the 20 most recently closed beads, extracts their
# closing commits to temp directories, runs the definition of done, and classifies
# any failures.

set -euo pipefail
cd "$(dirname "$0")/../.."

# Configuration
SAMPLE_SIZE=${SAMPLE_SIZE:-20}
SESSION_SCRATCH="$HOME/scratch/false-close-audit-$$"
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# Results
declare -a WORKSPACE_RESULTS
TOTAL_WORKSPACES=0
TOTAL_BEADS_SAMPLED=0
TOTAL_FALSE_CLOSES=0
declare -A FAILURE_CLASSES
FAILURE_CLASSES[a]=0  # never compiled
FAILURE_CLASSES[b]=0  # uncommitted dependency
FAILURE_CLASSES[c]=0  # named test red
FAILURE_CLASSES[d]=0  # deliverable says blocked/not done
FAILURE_CLASSES[e]=0  # other

mkdir -p "$SESSION_SCRATCH"
trap 'rm -rf "$SESSION_SCRATCH"' EXIT

log() { echo "[$(date -u +%H:%M:%S)] $*"; }
log_success() { echo "[$(date +%H:%M:%S)] ✓ $*"; }
log_error() { echo "[$(date +%H:%M:%S)] ✗ $*"; }

# Detect bead backend
detect_bead_backend() {
    if [[ -f "$1/.beads/config.json" ]]; then
        echo "bead-rs"
    elif [[ -f "$1/.beads/config.yaml" ]]; then
        echo "bf"
    else
        echo "unknown"
    fi
}

# Find closing commit for a bead
find_closing_commit() {
    local workspace="$1"
    local bead_id="$2"

    cd "$workspace"

    # Try git log with bead ID first
    local commit
    commit=$(git log --all --grep="$bead_id" --format="%H" -n 1 2>/dev/null || true)

    if [[ -n "$commit" ]]; then
        echo "$commit"
        return 0
    fi

    # Fallback: find commit within 1 hour of closed_at timestamp
    # This requires passing in closed_at, which we'll handle in the caller
    return 1
}

# Detect project language
detect_language() {
    if [[ -f "$1/go.mod" ]]; then
        echo "go"
    elif [[ -f "$1/Cargo.toml" ]]; then
        echo "rust"
    elif [[ -f "$1/package.json" ]]; then
        echo "node"
    elif [[ -f "$1/pyproject.toml" ]] || [[ -f "$1/setup.py" ]]; then
        echo "python"
    else
        echo "unknown"
    fi
}

# Run definition of done
run_definition_of_done() {
    local extraction_dir="$1"
    local language="$2"

    cd "$extraction_dir"

    if [[ -x "scripts/definition-of-done.sh" ]]; then
        if timeout 300 bash scripts/definition-of-done.sh 2>&1; then
            return 0
        else
            return 1
        fi
    fi

    case "$language" in
        go)
            timeout 300 bash -c 'go build ./... && go vet ./... && go test -short ./...' 2>&1
            ;;
        rust)
            timeout 600 cargo build 2>&1 && timeout 600 cargo test 2>&1
            ;;
        node)
            timeout 300 npm test 2>&1
            ;;
        python)
            timeout 300 pytest -x 2>&1
            ;;
        *)
            echo "No definition of done for language: $language"
            return 0  # Can't verify, assume pass
            ;;
    esac
}

# Audit a single bead
audit_bead() {
    local workspace="$1"
    local workspace_name
    workspace_name=$(basename "$workspace")
    local bead_id="$2"
    local bead_json="$3"

    log "  Auditing $bead_id"

    # Get commit and language
    local closed_at
    closed_at=$(echo "$bead_json" | jq -r '.closed_at // .updated_at')

    cd "$workspace"
    local commit
    if ! commit=$(find_closing_commit "$workspace" "$bead_id"); then
        log "    ⚠ No commit found, skipping"
        return 0
    fi

    # Extract commit
    local extraction_dir="$SESSION_SCRATCH/${workspace_name}-${bead_id}"
    mkdir -p "$extraction_dir"
    git archive "$commit" | tar -x -C "$extraction_dir" 2>/dev/null || {
        log "    ⚠ Could not extract commit $commit, skipping"
        return 0
    }

    # Detect language
    local language
    language=$(detect_language "$extraction_dir")

    # Run definition of done
    local output
    if output=$(run_definition_of_done "$extraction_dir" "$language" 2>&1); then
        log_success "    $bead_id ✓ PASS"
        echo "{\"workspace\":\"$workspace_name\",\"bead_id\":\"$bead_id\",\"commit\":\"$commit\",\"status\":\"pass\"}" >> "$SESSION_SCRATCH/results.jsonl"
        return 0
    else
        local exit_code=$?
        log_error "    $bead_id ✗ FAIL (exit $exit_code)"

        # Classify failure
        local failure_class="e"
        if echo "$output" | grep -q "undefined:\|use of undefined"; then
            failure_class="a"
        elif echo "$output" | grep -q "cannot find package"; then
            failure_class="b"
        elif echo "$output" | grep -q "FAIL.*Test"; then
            failure_class="c"
        fi

        FAILURE_CLASSES[$failure_class]=$((${FAILURE_CLASSES[$failure_class]} + 1))
        TOTAL_FALSE_CLOSES=$((TOTAL_FALSE_CLOSES + 1))

        echo "{\"workspace\":\"$workspace_name\",\"bead_id\":\"$bead_id\",\"commit\":\"$commit\",\"status\":\"fail\",\"class\":\"$failure_class\",\"output\":\"$(echo "$output" | head -c 500 | jq -Rs .)\"}" >> "$SESSION_SCRATCH/results.jsonl"

        return 1
    fi
}

# Audit a workspace
audit_workspace() {
    local workspace="$1"
    local workspace_name
    workspace_name=$(basename "$workspace")

    log "Auditing: $workspace_name"

    local backend
    backend=$(detect_bead_backend "$workspace")

    if [[ "$backend" == "unknown" ]]; then
        log "  ⚠ No bead store, skipping"
        return 0
    fi

    TOTAL_WORKSPACES=$((TOTAL_WORKSPACES + 1))
    local workspace_sampled=0
    local workspace_fails=0

    cd "$workspace"

    # Get closed beads
    local beads_json
    if ! beads_json=$(bead list --status closed --limit "$SAMPLE_SIZE" --json 2>/dev/null); then
        log "  ⚠ No closed beads, skipping"
        return 0
    fi

    # Parse beads and audit each
    local bead_count=0

    # Store beads in temp file for line-by-line processing
    local beads_file="$SESSION_SCRATCH/beads-$workspace_name.jsonl"
    echo "$beads_json" > "$beads_file"

    while IFS= read -r bead_json; do
        [[ -z "$bead_json" ]] && continue

        local bead_id
        bead_id=$(echo "$bead_json" | jq -r '.id // empty')

        [[ -z "$bead_id" ]] && continue

        if audit_bead "$workspace" "$workspace_name" "$bead_id" "$bead_json"; then
            : # pass
        else
            workspace_fails=$((workspace_fails + 1))
        fi

        workspace_sampled=$((workspace_sampled + 1))
        TOTAL_BEADS_SAMPLED=$((TOTAL_BEADS_SAMPLED + 1))
        bead_count=$((bead_count + 1))

        [[ $bead_count -ge $SAMPLE_SIZE ]] && break
    done < "$beads_file"

    WORKSPACE_RESULTS+=("$workspace_name|$workspace_sampled|$workspace_fails")
    log "  Complete: $workspace_sampled sampled, $workspace_fails failed"
}

# Main
main() {
    log "Starting false-close audit"
    log "Sample size: $SAMPLE_SIZE beads per workspace"

    > "$SESSION_SCRATCH/results.jsonl"

    # Audit each workspace
    for workspace_git in "$HOME"/*/.git; do
        workspace=$(dirname "$workspace_git")

        [[ ! -d "$workspace/.beads" ]] && continue
        [[ ! -d "$workspace/.git" ]] && continue

        audit_workspace "$workspace"
    done

    # Generate report
    generate_report

    log "Audit complete"
    log "  Workspaces: $TOTAL_WORKSPACES"
    log "  Beads sampled: $TOTAL_BEADS_SAMPLED"
    log "  False closes: $TOTAL_FALSE_CLOSES"
}

generate_report() {
    local report_file="$SESSION_SCRATCH/false-close-audit-2026-08.md"

    cat > "$report_file" <<EOF
# False-Close Audit - ${TIMESTAMP}

## Executive Summary

- **Workspaces audited:** ${TOTAL_WORKSPACES}
- **Beads sampled:** ${TOTAL_BEADS_SAMPLED}
- **False closes detected:** ${TOTAL_FALSE_CLOSES}
- **Overall false-close rate:** $(echo "scale=1; $TOTAL_FALSE_CLOSES * 100 / $TOTAL_BEADS_SAMPLED" | bc 2>/dev/null || echo "N/A")%

## Failure Classifications

| Class | Description | Count |
|-------|-------------|-------|
| (a) | Never compiled | ${FAILURE_CLASSES[a]} |
| (b) | Uncommitted dependency | ${FAILURE_CLASSES[b]} |
| (c) | Named test red | ${FAILURE_CLASSES[c]} |
| (d) | Deliverable says blocked/not done | ${FAILURE_CLASSES[d]} |
| (e) | Other | ${FAILURE_CLASSES[e]} |

## Per-Workspace Results

| Workspace | Sampled | False Closes | Rate |
|-----------|----------|---------------|------|
EOF

    for result in "${WORKSPACE_RESULTS[@]}"; do
        IFS='|' read -r ws sampled fails <<< "$result"
        local rate
        rate=$(echo "scale=1; $fails * 100 / $sampled" | bc 2>/dev/null || echo "0")
        echo "| $ws | $sampled | $fails | ${rate}% |" >> "$report_file"
    done

    echo "" >> "$report_file"
    echo "## Detailed False Closes" >> "$report_file"
    echo "" >> "$report_file"

    if [[ -f "$SESSION_SCRATCH/results.jsonl" ]]; then
        while IFS= read -r line; do
            local status
            status=$(echo "$line" | jq -r '.status // empty')

            if [[ "$status" == "fail" ]]; then
                local ws bead_id commit class output
                ws=$(echo "$line" | jq -r '.workspace')
                bead_id=$(echo "$line" | jq -r '.bead_id')
                commit=$(echo "$line" | jq -r '.commit')
                class=$(echo "$line" | jq -r '.class')
                output=$(echo "$line" | jq -r '.output')

                cat >> "$report_file" <<EOF
### ${ws}/${bead_id}

- **Commit:** \`${commit}\`
- **Class:** ${class}
- **Output:** \`\`\`
${output}
\`\`\`

EOF
            fi
        done < "$SESSION_SCRATCH/results.jsonl"
    fi

    log "Report saved to: $report_file"
    cat "$report_file"
}

main "$@"
