#!/usr/bin/env bash
# NEEDLE Bead Mitosis Module
# Automatic bead decomposition for complex tasks
#
# This module implements the mitosis system that:
# - Detects when a bead represents multiple tasks
# - Splits complex beads into child beads with dependencies
# - Parent auto-completes when children finish
# - Enables parallel work and better success rates
#
# Usage:
#   source "$NEEDLE_SRC/bead/mitosis.sh"
#   if _needle_check_mitosis "$bead_id" "$workspace" "$agent"; then
#       # Mitosis performed, children created
#   else
#       # No mitosis needed, process bead normally
#   fi
#
# Return values:
#   0 - Mitosis performed (bead was split)
#   1 - No mitosis needed or disabled

# Source dependencies (if not already loaded)
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

if [[ -z "${_NEEDLE_CONFIG_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/config.sh"
fi

if [[ -z "${_NEEDLE_JSON_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/json.sh"
fi

if [[ -z "${_NEEDLE_WORKSPACE_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/workspace.sh"
fi

if [[ -z "${_NEEDLE_CLAIM_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/claim.sh"
fi

# ============================================================================
# Mitosis Configuration
# ============================================================================

# Default mitosis settings (can be overridden via config.yaml)
NEEDLE_MITOSIS_ENABLED="${NEEDLE_MITOSIS_ENABLED:-true}"
NEEDLE_MITOSIS_SKIP_TYPES="${NEEDLE_MITOSIS_SKIP_TYPES:-bug,hotfix}"
NEEDLE_MITOSIS_SKIP_LABELS="${NEEDLE_MITOSIS_SKIP_LABELS:-no-mitosis,atomic}"
NEEDLE_MITOSIS_MAX_CHILDREN="${NEEDLE_MITOSIS_MAX_CHILDREN:-5}"
NEEDLE_MITOSIS_MIN_CHILDREN="${NEEDLE_MITOSIS_MIN_CHILDREN:-2}"
NEEDLE_MITOSIS_MIN_COMPLEXITY="${NEEDLE_MITOSIS_MIN_COMPLEXITY:-3}"
NEEDLE_MITOSIS_TIMEOUT="${NEEDLE_MITOSIS_TIMEOUT:-60}"

# ============================================================================
# Configuration Accessors
# ============================================================================

# Get mitosis configuration value with fallback
# Supports workspace-level overrides via .needle.yaml
# Usage: _needle_mitosis_config <key> [default] [workspace]
# Example: _needle_mitosis_config "enabled" "true" "/home/user/project"
_needle_mitosis_config() {
    local key="$1"
    local default="${2:-}"
    local workspace="${3:-}"
    local value

    # If workspace is provided, try workspace config first (respects overrides)
    if [[ -n "$workspace" ]] && [[ -f "$workspace/.needle.yaml" ]]; then
        value=$(get_workspace_setting "$workspace" "mitosis.$key" 2>/dev/null)
        if [[ "$value" != "null" ]] && [[ -n "$value" ]]; then
            echo "$value"
            return 0
        fi
    fi

    # Fall back to global config file
    value=$(get_config "mitosis.$key" 2>/dev/null)

    # Handle null/empty values
    if [[ "$value" == "null" ]] || [[ -z "$value" ]]; then
        echo "$default"
    else
        echo "$value"
    fi
}

# Check if mitosis is enabled
# Usage: _needle_mitosis_is_enabled [workspace]
# Returns: 0 if enabled, 1 if disabled
_needle_mitosis_is_enabled() {
    local workspace="${1:-}"
    local enabled
    enabled=$(_needle_mitosis_config "enabled" "$NEEDLE_MITOSIS_ENABLED" "$workspace")

    case "$enabled" in
        true|True|TRUE|yes|Yes|YES|1)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# Get skip types list
# Usage: _needle_mitosis_get_skip_types [workspace]
# Returns: Comma-separated list of types to skip
_needle_mitosis_get_skip_types() {
    local workspace="${1:-}"
    _needle_mitosis_config "skip_types" "$NEEDLE_MITOSIS_SKIP_TYPES" "$workspace"
}

# Get skip labels list
# Usage: _needle_mitosis_get_skip_labels [workspace]
# Returns: Comma-separated list of labels to skip
_needle_mitosis_get_skip_labels() {
    local workspace="${1:-}"
    _needle_mitosis_config "skip_labels" "$NEEDLE_MITOSIS_SKIP_LABELS" "$workspace"
}

# Get min_complexity threshold
# Usage: _needle_mitosis_get_min_complexity [workspace]
# Returns: Minimum complexity (description lines) to consider mitosis
_needle_mitosis_get_min_complexity() {
    local workspace="${1:-}"
    _needle_mitosis_config "min_complexity" "$NEEDLE_MITOSIS_MIN_COMPLEXITY" "$workspace"
}

# ============================================================================
# Mitosis Detection
# ============================================================================

# Check if a bead should undergo mitosis
# This is the main entry point for mitosis detection
#
# Usage: _needle_check_mitosis <bead_id> <workspace> <agent>
# Arguments:
#   bead_id   - The bead ID to check
#   workspace - The workspace path
#   agent     - The agent name to use for analysis
#
# Return values:
#   0 - Mitosis performed (bead was split into children)
#   1 - No mitosis needed (process bead normally)
#
# Example:
#   if _needle_check_mitosis "nd-100" "/home/user/project" "claude-anthropic-sonnet"; then
#       echo "Bead was split into children"
#   else
#       echo "Process bead normally"
#   fi
_needle_check_mitosis() {
    local bead_id="$1"
    local workspace="$2"
    local agent="$3"

    # Validate inputs
    if [[ -z "$bead_id" ]]; then
        _needle_error "Mitosis check requires bead_id"
        return 1
    fi

    if [[ -z "$workspace" ]]; then
        _needle_error "Mitosis check requires workspace"
        return 1
    fi

    # Check if mitosis is enabled (respect workspace override)
    if ! _needle_mitosis_is_enabled "$workspace"; then
        _needle_debug "Mitosis is disabled"
        return 1
    fi

    # Get bead details
    # NOTE: br show must run in workspace context to find bead
    local bead_json
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        bead_json=$(cd "$workspace" && br show "$bead_id" --json 2>/dev/null)
    else
        bead_json=$(br show "$bead_id" --json 2>/dev/null)
    fi

    if [[ -z "$bead_json" ]] || [[ "$bead_json" == "null" ]]; then
        _needle_debug "Could not retrieve bead: $bead_id"
        return 1
    fi

    # Handle array or single object response
    local bead_object
    if echo "$bead_json" | jq -e 'type == "array"' &>/dev/null; then
        bead_object=$(echo "$bead_json" | jq -c '.[0]')
    else
        bead_object="$bead_json"
    fi

    # Extract bead properties
    local bead_type labels description
    bead_type=$(echo "$bead_object" | jq -r '.type // .issue_type // "task"')
    labels=$(echo "$bead_object" | jq -r '.labels | if type == "array" then join(",") else . // "" end')
    description=$(echo "$bead_object" | jq -r '.description // ""')

    _needle_debug "Checking mitosis for bead $bead_id (type: $bead_type, labels: $labels)"

    # Check if bead type should be skipped (respect workspace override)
    local skip_types
    skip_types=$(_needle_mitosis_get_skip_types "$workspace")
    if [[ -n "$skip_types" ]] && [[ ",$skip_types," == *",$bead_type,"* ]]; then
        _needle_debug "Skipping mitosis for bead type: $bead_type"
        return 1
    fi

    # Check for skip labels (respect workspace override)
    local skip_labels
    skip_labels=$(_needle_mitosis_get_skip_labels "$workspace")
    if [[ -n "$labels" ]] && [[ -n "$skip_labels" ]]; then
        IFS=',' read -ra label_array <<< "$labels"
        IFS=',' read -ra skip_array <<< "$skip_labels"

        for label in "${label_array[@]}"; do
            for skip in "${skip_array[@]}"; do
                if [[ "$label" == "$skip" ]]; then
                    _needle_debug "Skipping mitosis due to label: $label"
                    return 1
                fi
            done
        done
    fi

    # Check minimum complexity (description lines)
    local min_complexity description_lines
    min_complexity=$(_needle_mitosis_get_min_complexity "$workspace")
    description_lines=$(echo "$description" | wc -l)

    if [[ "$description_lines" -lt "$min_complexity" ]]; then
        _needle_debug "Skipping mitosis: description too short ($description_lines lines < $min_complexity minimum)"
        return 1
    fi

    # Emit mitosis check event
    _needle_emit_event "bead.mitosis.check" \
        "Checking if bead needs mitosis" \
        "bead_id=$bead_id"

    # Build analysis prompt and run mitosis analysis
    local analysis
    analysis=$(_needle_analyze_for_mitosis "$bead_id" "$workspace" "$agent" "$bead_object")

    if [[ -z "$analysis" ]]; then
        _needle_debug "Mitosis analysis returned empty result"
        return 1
    fi

    # Parse analysis result
    local should_split
    should_split=$(echo "$analysis" | jq -r '.mitosis // false' 2>/dev/null)

    if [[ "$should_split" != "true" ]]; then
        _needle_debug "Mitosis not recommended for bead $bead_id"
        return 1
    fi

    # Perform mitosis
    _needle_perform_mitosis "$bead_id" "$workspace" "$analysis"
}

# ============================================================================
# Mitosis Analysis
# ============================================================================

# Build the mitosis analysis prompt
# Usage: _needle_build_mitosis_prompt <bead_id> <workspace> <bead_object>
# Returns: Formatted prompt string
_needle_build_mitosis_prompt() {
    local bead_id="$1"
    local workspace="$2"
    local bead_object="$3"

    # Extract bead details
    local title description priority parent_labels
    title=$(echo "$bead_object" | jq -r '.title // "Untitled"')
    description=$(echo "$bead_object" | jq -r '.description // ""')
    priority=$(echo "$bead_object" | jq -r '.priority // 2')
    parent_labels=$(echo "$bead_object" | jq -r '.labels // []' | jq -r 'join(",")')

    # Get max children config (respect workspace override)
    local max_children
    max_children=$(_needle_mitosis_config "max_children" "$NEEDLE_MITOSIS_MAX_CHILDREN" "$workspace")

    # Gather workspace context
    local relevant_files recent_commits test_files

    # Get relevant files from workspace (limit to 50 for context)
    if [[ -d "$workspace" ]]; then
        relevant_files=$(cd "$workspace" && git ls-files 2>/dev/null | head -50 | sed 's/^/  - /')
        if [[ -z "$relevant_files" ]]; then
            relevant_files="  (No git repository or no files found)"
        fi

        # Get recent commits for context
        recent_commits=$(cd "$workspace" && git log --oneline -10 2>/dev/null | sed 's/^/  - /')
        if [[ -z "$recent_commits" ]]; then
            recent_commits="  (No git history available)"
        fi

        # Find existing test files
        test_files=$(cd "$workspace" && git ls-files 2>/dev/null | grep -E 'test[_-]|spec|tests/' | head -20 | sed 's/^/  - /')
        if [[ -z "$test_files" ]]; then
            test_files="  (No test files found)"
        fi
    else
        relevant_files="  (Workspace not accessible)"
        recent_commits="  (Not in a workspace)"
        test_files="  (Not in a workspace)"
    fi

    cat <<MITOSIS_PROMPT
# Mitosis Analysis Task

Analyze the following task to determine if it should be split into smaller subtasks (mitosis).

## Task Details
- **ID:** $bead_id
- **Title:** $title
- **Priority:** P${priority}
- **Parent Labels:** ${parent_labels:-<none>}
- **Description:**
$description

## Workspace
$workspace

## Workspace Context

### Relevant Files (first 50)
${relevant_files}

### Recent Commits (last 10)
${recent_commits}

### Test Files
${test_files}

## Mitosis Criteria
A task should be split (mitosis = true) if it meets ANY of these criteria:
1. **Multiple files**: Involves changes to more than 5 files
2. **Unrelated concerns**: Contains multiple distinct tasks that could be worked independently
3. **Explicit markers**: Contains "and", numbered lists, or multiple distinct items
4. **Size estimate**: Estimated implementation would exceed 500 lines of code

## Constraints
- Maximum number of child tasks: $max_children
- Minimum number of child tasks: 2 (if mitosis is triggered)
- Each child should be independently actionable
- Children may have sequential dependencies (blocked_by)

## Output Format
Respond with ONLY a JSON object (no markdown, no code blocks):

{
  "mitosis": true/false,
  "reasoning": "Brief explanation of why mitosis should or should not occur",
  "children": [
    {
      "title": "Child task title",
      "description": "Detailed, file-specific description referencing actual paths from the workspace context",
      "affected_files": ["src/auth.py", "tests/test_auth.py"],
      "verification_cmd": "pytest tests/test_auth.py -q",
      "labels": ["optional-domain-label"],
      "blocked_by": []
    }
  ]
}

## Important Notes
- **affected_files**: List actual file paths from the workspace context that this child will modify
- **verification_cmd**: Provide a specific test command to validate this child's work (e.g., "pytest tests/X.py", "npm test -- path/to/test")
- **description**: Must reference actual files and include specific implementation details
- **labels**: Optional list of domain-specific labels to apply to this child (do NOT include "mitosis-child" or "parent-*" — these are added automatically)

## Examples

### Example 1: Task needing mitosis with workspace context
Input: "Implement user authentication and add password reset functionality and set up email verification"
Workspace files include: src/auth.py, tests/test_auth.py, src/email.py, tests/test_email.py
Output:
{
  "mitosis": true,
  "reasoning": "Three distinct features that can be implemented independently",
  "children": [
    {
      "title": "Implement user authentication",
      "description": "Add login/logout functionality to src/auth.py. Implement password hashing using bcrypt. Add session management.",
      "affected_files": ["src/auth.py", "tests/test_auth.py"],
      "verification_cmd": "pytest tests/test_auth.py -q",
      "blocked_by": []
    },
    {
      "title": "Add password reset",
      "description": "Implement password reset flow in src/auth.py. Generate reset tokens and send via email using src/email.py.",
      "affected_files": ["src/auth.py", "src/email.py", "tests/test_auth.py"],
      "verification_cmd": "pytest tests/test_auth.py::test_password_reset -q",
      "blocked_by": ["previous"]
    },
    {
      "title": "Set up email verification",
      "description": "Add email verification on signup. Modify src/auth.py to store verified flag. Use src/email.py for sending verification emails.",
      "affected_files": ["src/auth.py", "src/email.py", "tests/test_auth.py", "tests/test_email.py"],
      "verification_cmd": "pytest tests/test_auth.py::test_email_verification -q",
      "blocked_by": ["previous"]
    }
  ]
}

### Example 2: Atomic task (no mitosis)
Input: "Fix the null pointer exception in UserService.java"
Output:
{
  "mitosis": false,
  "reasoning": "Single focused bug fix in one file",
  "children": []
}

Now analyze the task and respond with the JSON.
MITOSIS_PROMPT
}

# Analyze a bead for mitosis using an agent
# Usage: _needle_analyze_for_mitosis <bead_id> <workspace> <agent> <bead_object>
# Returns: JSON analysis result
_needle_analyze_for_mitosis() {
    local bead_id="$1"
    local workspace="$2"
    local agent="$3"
    local bead_object="$4"

    # Build the analysis prompt
    local prompt
    prompt=$(_needle_build_mitosis_prompt "$bead_id" "$workspace" "$bead_object")

    # Get timeout from config (respect workspace override)
    local timeout
    timeout=$(_needle_mitosis_config "timeout" "$NEEDLE_MITOSIS_TIMEOUT" "$workspace")

    _needle_debug "Running mitosis analysis with agent: $agent (timeout: ${timeout}s)"

    # Source agent dispatcher if available
    local dispatch_script
    dispatch_script="$(dirname "${BASH_SOURCE[0]}")/../agent/dispatch.sh"

    if [[ -f "$dispatch_script" ]]; then
        source "$dispatch_script"

        # Create temp file for output
        local output_file
        output_file=$(mktemp "${TMPDIR:-/tmp}/needle-mitosis-${bead_id}-XXXXXXXX.json")

        # Dispatch to agent for analysis
        local result
        result=$(_needle_dispatch_agent "$agent" "$workspace" "$prompt" "$bead_id" "mitosis-check" "$timeout")
        local dispatch_exit=$?

        if [[ $dispatch_exit -ne 0 ]]; then
            _needle_warn "Mitosis analysis dispatch failed"
            rm -f "$output_file" 2>/dev/null
            return 1
        fi

        # Parse result (last line only — prior lines are agent stdout via tee)
        local exit_code duration output_path
        local last_line
        last_line=$(tail -n 1 <<< "$result")
        IFS='|' read -r exit_code duration output_path <<< "$last_line"

        if [[ ! -f "$output_path" ]]; then
            _needle_warn "Mitosis analysis output file not found"
            return 1
        fi

        # Extract JSON from output (handle markdown code blocks)
        local analysis
        analysis=$(_needle_extract_json_from_output "$output_path")

        # Cleanup
        rm -f "$output_path" 2>/dev/null

        if [[ -z "$analysis" ]]; then
            _needle_warn "Could not extract JSON from mitosis analysis"
            return 1
        fi

        # Validate JSON
        if ! echo "$analysis" | jq -e '.mitosis' &>/dev/null; then
            _needle_warn "Invalid mitosis analysis JSON"
            return 1
        fi

        echo "$analysis"
        return 0
    else
        # Fallback: use simple heuristic-based analysis
        _needle_debug "Agent dispatcher not available, using heuristic analysis"
        _needle_heuristic_mitosis_analysis "$bead_object"
    fi
}

# Extract JSON from agent output (handles markdown code blocks and raw JSON)
# Usage: _needle_extract_json_from_output <file_path>
# Returns: Clean JSON string
_needle_extract_json_from_output() {
    local file_path="$1"

    if [[ ! -f "$file_path" ]]; then
        return 1
    fi

    # Method 1: Extract content from a markdown code block (handles multiline JSON)
    local extracted
    extracted=$(awk '/^```(json)?[[:space:]]*$/{f=1;next} /^```[[:space:]]*$/{if(f){f=0}} f' "$file_path")
    if [[ -n "$extracted" ]] && echo "$extracted" | jq -e '.' &>/dev/null 2>&1; then
        echo "$extracted"
        return 0
    fi

    # Method 2: Find the first balanced JSON object in the file (handles raw multiline JSON)
    local json_obj
    json_obj=$(python3 - "$file_path" <<'PYEOF' 2>/dev/null
import sys, json

with open(sys.argv[1], 'r') as f:
    content = f.read()

start = content.find('{')
if start < 0:
    sys.exit(1)

depth = 0
for i, c in enumerate(content[start:], start):
    if c == '{':
        depth += 1
    elif c == '}':
        depth -= 1
        if depth == 0:
            try:
                obj = json.loads(content[start:i+1])
                print(json.dumps(obj))
            except Exception:
                pass
            break

PYEOF
)
    if [[ -n "$json_obj" ]]; then
        echo "$json_obj"
        return 0
    fi

    return 1
}

# Heuristic-based mitosis analysis (fallback when agent unavailable)
# Usage: _needle_heuristic_mitosis_analysis <bead_object> [workspace]
# Returns: JSON analysis result
_needle_heuristic_mitosis_analysis() {
    local bead_object="$1"
    local workspace="${2:-}"

    local title description
    title=$(echo "$bead_object" | jq -r '.title // ""')
    description=$(echo "$bead_object" | jq -r '.description // ""')

    local combined="$title $description"
    local split_indicators=0
    local children="[]"
    local reasoning="Heuristic analysis: "

    # Check for "and" conjunctions
    local and_count
    and_count=$(echo "$combined" | grep -oiE ' and ' | wc -l)
    if [[ $and_count -ge 2 ]]; then
        ((split_indicators++))
        reasoning+="Multiple 'and' conjunctions detected ($and_count). "
    fi

    # Check for numbered lists
    if echo "$description" | grep -qE '^[0-9]+\.'; then
        ((split_indicators++))
        reasoning+="Numbered list detected. "
    fi

    # Check for bullet points
    local bullet_count
    bullet_count=$(echo "$description" | grep -cE '^\s*[-*]')
    if [[ $bullet_count -ge 3 ]]; then
        ((split_indicators++))
        reasoning+="Multiple bullet points detected ($bullet_count). "
    fi

    # Check for multiple file mentions
    local file_count
    file_count=$(echo "$description" | grep -oE '[a-zA-Z0-9_/.-]+\.(py|js|ts|go|rs|sh|yaml|yml|json|md)' | sort -u | wc -l)
    if [[ $file_count -gt 5 ]]; then
        ((split_indicators++))
        reasoning+="Many files mentioned ($file_count). "
    fi

    # Check for implementation/feature/add keywords suggesting multiple features
    if echo "$combined" | grep -qiE '(implement|add|create|build|set up).*(implement|add|create|build|set up)'; then
        ((split_indicators++))
        reasoning+="Multiple implementation verbs detected. "
    fi

    # Determine if mitosis should occur
    if [[ $split_indicators -ge 2 ]]; then
        # Build simple children based on split indicators
        # This is a simplified heuristic - real mitosis should use agent analysis
        local max_children
        max_children=$(_needle_mitosis_config "max_children" "$NEEDLE_MITOSIS_MAX_CHILDREN" "$workspace")

        # Limit children to max
        local child_count=$((split_indicators > max_children ? max_children : split_indicators))

        children="["
        for ((i=1; i<=child_count; i++)); do
            if [[ $i -gt 1 ]]; then
                children+=","
            fi
            children+="{\"title\":\"Task part $i\",\"description\":\"Part $i of the original task\",\"blocked_by\":[]}"
        done
        children+="]"

        cat <<HEURISTIC_RESULT
{
  "mitosis": true,
  "reasoning": "${reasoning}Recommend splitting into $child_count subtasks.",
  "children": $children
}
HEURISTIC_RESULT
    else
        cat <<HEURISTIC_RESULT
{
  "mitosis": false,
  "reasoning": "${reasoning}Task appears to be atomic.",
  "children": []
}
HEURISTIC_RESULT
    fi
}

# ============================================================================
# Mitosis Execution
# ============================================================================

# Perform mitosis - split a bead into children
# Usage: _needle_perform_mitosis <parent_id> <workspace> <analysis_json>
# Returns: 0 on success, 1 on failure
_needle_perform_mitosis() {
    local parent_id="$1"
    local workspace="$2"
    local analysis="$3"

    _needle_info "Performing mitosis on bead: $parent_id"

    # Validate analysis JSON
    if ! echo "$analysis" | jq -e '.children | length' &>/dev/null; then
        _needle_error "Invalid mitosis analysis JSON"
        return 1
    fi

    # Get children array
    local children_count
    children_count=$(echo "$analysis" | jq '.children | length')

    local min_children
    min_children=$(_needle_mitosis_config "min_children" "$NEEDLE_MITOSIS_MIN_CHILDREN")

    if [[ $children_count -lt $min_children ]]; then
        _needle_warn "Mitosis produced only $children_count children (minimum: $min_children), skipping"
        return 1
    fi

    local max_children
    max_children=$(_needle_mitosis_config "max_children" "$NEEDLE_MITOSIS_MAX_CHILDREN")

    if [[ $children_count -gt $max_children ]]; then
        _needle_warn "Mitosis produced $children_count children, limiting to $max_children"
        children_count=$max_children
    fi

    # Fetch parent bead details for field inheritance (priority, labels)
    local parent_json_raw parent_obj
    if [[ -n "$workspace" && -d "$workspace" ]]; then
        parent_json_raw=$(cd "$workspace" && br show "$parent_id" --json 2>/dev/null)
    else
        parent_json_raw=$(br show "$parent_id" --json 2>/dev/null)
    fi
    if echo "$parent_json_raw" | jq -e 'type == "array"' &>/dev/null; then
        parent_obj=$(echo "$parent_json_raw" | jq -c '.[0]')
    else
        parent_obj="$parent_json_raw"
    fi

    # Inherit priority from parent (default 2 if unavailable)
    local parent_priority
    parent_priority=$(echo "$parent_obj" | jq -r '.priority // 2' 2>/dev/null)
    parent_priority="${parent_priority:-2}"

    # Inherit non-system labels from parent (exclude mitosis-child and parent-* labels)
    local parent_inherited_labels
    parent_inherited_labels=$(echo "$parent_obj" | jq -r \
        '.labels // [] | map(select(. != "mitosis-child" and (startswith("parent-") | not))) | .[]' \
        2>/dev/null)

    # Extract parent's verification_cmd for potential propagation
    # Check both direct field and label format (verification_cmd:<command>)
    local parent_verification_cmd
    parent_verification_cmd=$(echo "$parent_obj" | jq -r '.verification_cmd // ""' 2>/dev/null)

    # If not in direct field, check labels for verification_cmd label
    if [[ -z "$parent_verification_cmd" ]]; then
        local parent_labels_json
        parent_labels_json=$(echo "$parent_obj" | jq -r '.labels // []' 2>/dev/null)
        parent_verification_cmd=$(echo "$parent_labels_json" | jq -r 'map(select(startswith("verification_cmd:"))) | .[]' | sed 's/^verification_cmd://' | head -1)
    fi

    # Emit mitosis started event
    _needle_emit_event "bead.mitosis.started" \
        "Starting mitosis for bead $parent_id" \
        "parent_id=$parent_id" \
        "children_count=$children_count"

    # Array to collect child IDs
    local -a child_ids=()
    local prev_id=""

    # Process each child
    local child_num=0
    while IFS= read -r child; do
        ((child_num++))

        # Skip if we've hit max children
        if [[ $child_num -gt $max_children ]]; then
            break
        fi

        # Extract child details
        local title description blocked_by
        title=$(echo "$child" | jq -r '.title // "Subtask"')
        description=$(echo "$child" | jq -r '.description // ""')
        blocked_by=$(echo "$child" | jq -r '.blocked_by // [] | join(",")')

        # Extract optional rich fields from extended child schema
        local affected_files verification_cmd child_labels
        affected_files=$(echo "$child" | jq -r '.affected_files // [] | join(", ")' 2>/dev/null)
        verification_cmd=$(echo "$child" | jq -r '.verification_cmd // ""' 2>/dev/null)

        # If child doesn't specify verification_cmd, try to adapt from parent
        if [[ -z "$verification_cmd" ]] && [[ -n "$parent_verification_cmd" ]]; then
            # Try to make parent verification_cmd more specific to this child
            # If child has affected_files, try to match tests to those files
            if [[ -n "$affected_files" ]]; then
                # Extract first file from affected_files as hint
                local first_file
                first_file=$(echo "$affected_files" | cut -d',' -f1 | xargs)

                # If parent cmd is a general pytest/npm test, try to narrow it
                if [[ "$parent_verification_cmd" =~ ^(pytest|npm test|npm run test) ]]; then
                    # Check if we can extract a test file name from affected_files
                    local test_file
                    test_file=$(echo "$affected_files" | grep -oE '[a-zA-Z0-9_/-]+test[_-]?[a-zA-Z0-9_-]*\.(py|js|ts|sh)' | head -1)

                    if [[ -n "$test_file" ]]; then
                        # Build more specific verification command
                        if [[ "$parent_verification_cmd" == pytest* ]]; then
                            verification_cmd="pytest $test_file -q"
                        else
                            # For npm, keep the parent cmd but mention the test file
                            verification_cmd="$parent_verification_cmd -- $test_file"
                        fi
                    else
                        # No specific test file found, use parent's cmd
                        verification_cmd="$parent_verification_cmd"
                    fi
                else
                    # Parent cmd is specific, use it as-is
                    verification_cmd="$parent_verification_cmd"
                fi
            else
                # No affected_files to guide adaptation, use parent's cmd as-is
                verification_cmd="$parent_verification_cmd"
            fi

            _needle_debug "Adapted parent verification_cmd for child: $verification_cmd"
        fi

        child_labels=$(echo "$child" | jq -r \
            '.labels // [] | map(select(. != "mitosis-child" and (startswith("parent-") | not))) | .[]' \
            2>/dev/null)

        # Append affected_files and verification_cmd to description when present
        # (for human readability)
        if [[ -n "$affected_files" ]]; then
            description+=$'\n\n'"**Affected files:** ${affected_files}"
        fi
        if [[ -n "$verification_cmd" ]]; then
            description+=$'\n'"**Verification:** \`${verification_cmd}\`"
        fi

        # Truncate title if too long (br CLI may have limits)
        if [[ ${#title} -gt 100 ]]; then
            title="${title:0:97}..."
        fi

        _needle_debug "Creating child $child_num: $title (priority: $parent_priority)"

        # Build label args: system labels + labels inherited from parent + per-child labels from LLM
        local label_args=("--label" "mitosis-child" "--label" "parent-$parent_id")
        if [[ -n "$parent_inherited_labels" ]]; then
            while IFS= read -r plabel; do
                [[ -n "$plabel" ]] && label_args+=("--label" "$plabel")
            done <<< "$parent_inherited_labels"
        fi
        if [[ -n "$child_labels" ]]; then
            while IFS= read -r clabel; do
                [[ -n "$clabel" ]] && label_args+=("--label" "$clabel")
            done <<< "$child_labels"
        fi
        # Add verification_cmd as a label for verify.sh to pick it up (format: verification_cmd:<command>)
        if [[ -n "$verification_cmd" ]]; then
            label_args+=("--label" "verification_cmd:${verification_cmd}")
        fi

        # Create child bead using wrapper (handles unassigned_by_default)
        local child_id=""

        child_id=$(_needle_create_bead \
            --workspace "$workspace" \
            --title "$title" \
            --description "$description" \
            --priority "$parent_priority" \
            --type task \
            "${label_args[@]}" \
            --silent 2>/dev/null)

        if [[ $? -ne 0 ]] || [[ -z "$child_id" ]]; then
            _needle_warn "Failed to create child bead"
            continue
        fi

        child_ids+=("$child_id")

        # Set blocking relationship if needed
        if [[ -n "$prev_id" ]] && [[ "$blocked_by" == *"previous"* ]]; then
            _needle_debug "Setting $child_id blocked by $prev_id"
            br update "$child_id" --blocked-by "$prev_id" 2>/dev/null || true
        fi

        prev_id="$child_id"

        # Emit child created event
        _needle_emit_event "bead.mitosis.child_created" \
            "Created child bead from mitosis" \
            "parent_id=$parent_id" \
            "child_id=$child_id" \
            "title=$title"

        _needle_verbose "Created child bead: $child_id - $title"

    done < <(echo "$analysis" | jq -c '.children[]')

    # Check if any children were created
    if [[ ${#child_ids[@]} -eq 0 ]]; then
        _needle_error "Mitosis failed: no children created"
        _needle_emit_event "bead.mitosis.failed" \
            "Mitosis failed: no children created" \
            "parent_id=$parent_id"
        return 1
    fi

    # Mark parent as blocked by all children
    _needle_debug "Setting parent $parent_id blocked by ${child_ids[*]}"
    for child_id in "${child_ids[@]}"; do
        br update "$parent_id" --blocked-by "$child_id" 2>/dev/null || true
    done

    # Release any claim on parent (children will be worked instead)
    br update "$parent_id" --release --reason "mitosis" 2>/dev/null || true

    # Add mitosis-parent label to parent
    br update "$parent_id" --label "mitosis-parent" 2>/dev/null || true

    # Emit mitosis complete event
    local children_list
    children_list=$(_needle_json_array "${child_ids[@]}")

    _needle_emit_event "bead.mitosis.complete" \
        "Mitosis complete: created ${#child_ids[@]} children" \
        "parent_id=$parent_id" \
        "children_count=${#child_ids[@]}" \
        "children=$children_list"

    _needle_success "Mitosis complete: $parent_id -> ${#child_ids[@]} children (${child_ids[*]})"

    return 0
}

# ============================================================================
# Utility Functions
# ============================================================================

# Check if a bead is a mitosis parent
# Usage: _needle_is_mitosis_parent <bead_id>
# Returns: 0 if parent, 1 if not
_needle_is_mitosis_parent() {
    local bead_id="$1"

    local bead_json
    bead_json=$(br show "$bead_id" --json 2>/dev/null)

    if [[ -z "$bead_json" ]]; then
        return 1
    fi

    local labels
    labels=$(echo "$bead_json" | jq -r '.labels | if type == "array" then join(",") else . // "" end' 2>/dev/null)

    [[ "$labels" == *"mitosis-parent"* ]]
}

# Check if a bead is a mitosis child
# Usage: _needle_is_mitosis_child <bead_id>
# Returns: 0 if child, 1 if not
_needle_is_mitosis_child() {
    local bead_id="$1"

    local bead_json
    bead_json=$(br show "$bead_id" --json 2>/dev/null)

    if [[ -z "$bead_json" ]]; then
        return 1
    fi

    local labels
    labels=$(echo "$bead_json" | jq -r '.labels | if type == "array" then join(",") else . // "" end' 2>/dev/null)

    [[ "$labels" == *"mitosis-child"* ]]
}

# Get parent ID for a mitosis child
# Usage: _needle_get_mitosis_parent <child_id>
# Returns: Parent bead ID or empty string
_needle_get_mitosis_parent() {
    local child_id="$1"

    local bead_json
    bead_json=$(br show "$child_id" --json 2>/dev/null)

    if [[ -z "$bead_json" ]]; then
        return 1
    fi

    local labels
    labels=$(echo "$bead_json" | jq -r '.labels | if type == "array" then join(",") else . // "" end' 2>/dev/null)

    # Extract parent ID from label
    if [[ "$labels" =~ parent-([a-z0-9-]+) ]]; then
        echo "${BASH_REMATCH[1]}"
    fi
}

# Get all children of a mitosis parent
# Usage: _needle_get_mitosis_children <parent_id>
# Returns: JSON array of child bead IDs
_needle_get_mitosis_children() {
    local parent_id="$1"

    # Search for beads with parent label
    local children
    children=$(br list --label "parent-$parent_id" --json 2>/dev/null)

    if [[ -z "$children" ]] || [[ "$children" == "[]" ]]; then
        echo "[]"
        return 0
    fi

    # Extract just the IDs
    echo "$children" | jq -c '[.[].id]'
}

# ============================================================================
# Direct Execution Support (for testing)
# ============================================================================

# Allow running this module directly for testing
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        check)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 check <bead_id> <workspace> <agent>"
                exit 1
            fi
            _needle_check_mitosis "$2" "$3" "${4:-}"
            ;;
        is-parent)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 is-parent <bead_id>"
                exit 1
            fi
            if _needle_is_mitosis_parent "$2"; then
                echo "true"
            else
                echo "false"
            fi
            ;;
        is-child)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 is-child <bead_id>"
                exit 1
            fi
            if _needle_is_mitosis_child "$2"; then
                echo "true"
            else
                echo "false"
            fi
            ;;
        get-parent)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 get-parent <child_id>"
                exit 1
            fi
            _needle_get_mitosis_parent "$2"
            ;;
        get-children)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 get-children <parent_id>"
                exit 1
            fi
            _needle_get_mitosis_children "$2" | jq .
            ;;
        analyze)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 analyze <bead_id> <workspace> [agent]"
                exit 1
            fi
            local bead_json
            bead_json=$(br show "$2" --json 2>/dev/null)
            _needle_analyze_for_mitosis "$2" "$3" "${4:-}" "$bead_json" | jq .
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  check <bead_id> <workspace> <agent>  Check if bead needs mitosis"
            echo "  is-parent <bead_id>                  Check if bead is a mitosis parent"
            echo "  is-child <bead_id>                   Check if bead is a mitosis child"
            echo "  get-parent <child_id>                Get parent ID for a mitosis child"
            echo "  get-children <parent_id>             Get children of a mitosis parent"
            echo "  analyze <bead_id> <workspace> [agent] Analyze bead for mitosis"
            echo ""
            echo "Environment variables:"
            echo "  NEEDLE_MITOSIS_ENABLED      Enable/disable mitosis (default: true)"
            echo "  NEEDLE_MITOSIS_SKIP_TYPES   Comma-separated types to skip"
            echo "  NEEDLE_MITOSIS_SKIP_LABELS  Comma-separated labels to skip"
            echo "  NEEDLE_MITOSIS_MAX_CHILDREN Maximum children per mitosis (default: 5)"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi
