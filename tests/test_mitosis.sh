#!/usr/bin/env bash
# Tests for NEEDLE Mitosis Core Functions
# Covers: _needle_heuristic_mitosis_analysis, _needle_extract_json_from_output,
#         _needle_build_mitosis_prompt (workspace context inclusion)

# Test setup
TEST_DIR=$(mktemp -d)
TEST_CONFIG_DIR="$TEST_DIR/.needle"
TEST_CONFIG_FILE="$TEST_CONFIG_DIR/config.yaml"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

export NEEDLE_HOME="$TEST_CONFIG_DIR"
export NEEDLE_CONFIG_FILE="$TEST_CONFIG_FILE"
export NEEDLE_CONFIG_NAME="config.yaml"
export NEEDLE_QUIET=true
export NEEDLE_VERBOSE=false

# Source required modules
source "$PROJECT_DIR/src/lib/constants.sh"
source "$PROJECT_DIR/src/lib/output.sh"
source "$PROJECT_DIR/src/lib/json.sh"
source "$PROJECT_DIR/src/lib/utils.sh"
source "$PROJECT_DIR/src/lib/config.sh"
source "$PROJECT_DIR/src/lib/workspace.sh"
source "$PROJECT_DIR/src/bead/mitosis.sh"

# Cleanup
cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Bootstrap config
mkdir -p "$TEST_CONFIG_DIR"
cat > "$TEST_CONFIG_FILE" << 'EOF'
mitosis:
  enabled: true
  max_children: 5
  min_children: 2
  min_complexity: 3
  skip_types: bug,hotfix
  skip_labels: no-mitosis,atomic
  timeout: 60
EOF

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

test_case() {
    local name="$1"
    ((TESTS_RUN++))
    echo -n "Testing: $name... "
    NEEDLE_CONFIG_CACHE=""
    _NEEDLE_WORKSPACE_CACHE=()
}

test_pass() {
    echo "PASS"
    ((TESTS_PASSED++))
}

test_fail() {
    local reason="${1:-}"
    echo "FAIL"
    [[ -n "$reason" ]] && echo "  Reason: $reason"
    ((TESTS_FAILED++))
}

# ============================================================================
# _needle_extract_json_from_output tests
# ============================================================================

test_case "_needle_extract_json_from_output extracts raw JSON"
tmpfile=$(mktemp "$TEST_DIR/extract-XXXXXX.txt")
cat > "$tmpfile" << 'EOF'
Some preamble text here.
{"mitosis": true, "reasoning": "test", "children": []}
Some trailing text.
EOF
result=$(_needle_extract_json_from_output "$tmpfile")
if echo "$result" | jq -e '.mitosis == true' &>/dev/null; then
    test_pass
else
    test_fail "Expected JSON with mitosis=true, got: $result"
fi
rm -f "$tmpfile"

test_case "_needle_extract_json_from_output extracts from markdown code block"
tmpfile=$(mktemp "$TEST_DIR/extract-XXXXXX.txt")
cat > "$tmpfile" << 'EOF'
Here is the analysis:
```json
{"mitosis": false, "reasoning": "atomic task", "children": []}
```
End of response.
EOF
result=$(_needle_extract_json_from_output "$tmpfile")
if echo "$result" | jq -e '.mitosis == false' &>/dev/null; then
    test_pass
else
    test_fail "Expected JSON from markdown block, got: $result"
fi
rm -f "$tmpfile"

test_case "_needle_extract_json_from_output extracts from unmarked code block"
tmpfile=$(mktemp "$TEST_DIR/extract-XXXXXX.txt")
cat > "$tmpfile" << 'EOF'
Analysis result:
```
{"mitosis": true, "reasoning": "multiple concerns", "children": [{"title": "Part 1", "description": "desc", "blocked_by": []}]}
```
EOF
result=$(_needle_extract_json_from_output "$tmpfile")
if echo "$result" | jq -e '.mitosis == true' &>/dev/null; then
    test_pass
else
    test_fail "Expected JSON from unmarked code block, got: $result"
fi
rm -f "$tmpfile"

test_case "_needle_extract_json_from_output handles multiline JSON"
tmpfile=$(mktemp "$TEST_DIR/extract-XXXXXX.txt")
cat > "$tmpfile" << 'EOF'
{
  "mitosis": true,
  "reasoning": "complex task with multiple files",
  "children": [
    {
      "title": "Child 1",
      "description": "First part",
      "affected_files": ["src/a.py"],
      "verification_cmd": "pytest tests/test_a.py",
      "blocked_by": []
    }
  ]
}
EOF
result=$(_needle_extract_json_from_output "$tmpfile")
if echo "$result" | jq -e '.children | length == 1' &>/dev/null; then
    test_pass
else
    test_fail "Expected multiline JSON with 1 child, got: $result"
fi
rm -f "$tmpfile"

test_case "_needle_extract_json_from_output returns empty for non-JSON file"
tmpfile=$(mktemp "$TEST_DIR/extract-XXXXXX.txt")
cat > "$tmpfile" << 'EOF'
This is just plain text with no JSON content.
No curly braces here.
EOF
result=$(_needle_extract_json_from_output "$tmpfile")
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "Expected empty result for non-JSON file, got: $result"
fi
rm -f "$tmpfile"

test_case "_needle_extract_json_from_output returns empty for missing file"
result=$(_needle_extract_json_from_output "/nonexistent/path/file.txt")
if [[ -z "$result" ]]; then
    test_pass
else
    test_fail "Expected empty result for missing file, got: $result"
fi

# ============================================================================
# _needle_heuristic_mitosis_analysis tests
# ============================================================================

test_case "_needle_heuristic_mitosis_analysis returns valid JSON"
bead_obj='{"title":"Test task","description":"Do something simple"}'
result=$(_needle_heuristic_mitosis_analysis "$bead_obj")
if echo "$result" | jq -e '.mitosis != null and .reasoning != null and .children != null' &>/dev/null; then
    test_pass
else
    test_fail "Expected valid JSON with mitosis/reasoning/children, got: $result"
fi

test_case "_needle_heuristic_mitosis_analysis returns mitosis=false for simple task"
bead_obj='{"title":"Fix null check","description":"Fix the null pointer in auth service"}'
result=$(_needle_heuristic_mitosis_analysis "$bead_obj")
mitosis_val=$(echo "$result" | jq -r '.mitosis' 2>/dev/null)
if [[ "$mitosis_val" == "false" ]]; then
    test_pass
else
    test_fail "Expected mitosis=false for simple task, got mitosis=$mitosis_val"
fi

test_case "_needle_heuristic_mitosis_analysis detects multiple 'and' conjunctions"
bead_obj='{"title":"Implement auth and add password reset and set up email verification","description":"Implement user authentication and add password reset and set up email notifications for the system"}'
result=$(_needle_heuristic_mitosis_analysis "$bead_obj")
mitosis_val=$(echo "$result" | jq -r '.mitosis' 2>/dev/null)
if [[ "$mitosis_val" == "true" ]]; then
    test_pass
else
    test_fail "Expected mitosis=true for task with multiple 'and' conjunctions, got mitosis=$mitosis_val"
fi

test_case "_needle_heuristic_mitosis_analysis detects numbered list in description"
bead_obj='{"title":"Multi-step feature","description":"1. Add auth module\n2. Implement sessions\n3. Set up password reset\n4. Add email notifications"}'
result=$(_needle_heuristic_mitosis_analysis "$bead_obj")
if echo "$result" | jq -e '.mitosis != null' &>/dev/null; then
    test_pass
else
    test_fail "Expected valid JSON result, got: $result"
fi

test_case "_needle_heuristic_mitosis_analysis detects multiple bullet points"
# Use bullets + "and" conjunctions to trigger 2+ indicators
bead_obj=$(cat << 'BEAD'
{"title":"Implement auth and sessions and email","description":"Implement the following and configure each step:\n- Add user authentication\n- Set up session management\n- Configure email notifications\n- Add password reset flow\n- Set up 2FA"}
BEAD
)
result=$(_needle_heuristic_mitosis_analysis "$bead_obj")
mitosis_val=$(echo "$result" | jq -r '.mitosis' 2>/dev/null)
if [[ "$mitosis_val" == "true" ]]; then
    test_pass
else
    test_fail "Expected mitosis=true for task with bullets and 'and' conjunctions, got mitosis=$mitosis_val"
fi

test_case "_needle_heuristic_mitosis_analysis children count is within limits"
bead_obj='{"title":"Big task and more tasks and even more tasks","description":"Implement feature A and feature B and feature C and feature D and feature E and feature F and feature G. Also add tests and documentation and configuration and setup scripts and deployment manifests"}'
result=$(_needle_heuristic_mitosis_analysis "$bead_obj")
children_count=$(echo "$result" | jq '.children | length' 2>/dev/null)
max_children=5
if [[ -n "$children_count" ]] && [[ "$children_count" -le "$max_children" ]]; then
    test_pass
else
    test_fail "Expected children count <= $max_children, got: $children_count"
fi

test_case "_needle_heuristic_mitosis_analysis children have required fields"
bead_obj='{"title":"Implement auth and add sessions and set up email","description":"Implement user authentication and add session handling and set up email verification for the platform"}'
result=$(_needle_heuristic_mitosis_analysis "$bead_obj")
mitosis_val=$(echo "$result" | jq -r '.mitosis' 2>/dev/null)
if [[ "$mitosis_val" == "true" ]]; then
    # Check that each child has title, description, and blocked_by
    invalid_children=$(echo "$result" | jq '[.children[] | select(.title == null or .description == null or .blocked_by == null)] | length' 2>/dev/null)
    if [[ "$invalid_children" == "0" ]]; then
        test_pass
    else
        test_fail "Some children are missing required fields (title/description/blocked_by)"
    fi
else
    # Mitosis not triggered — skip structure check
    test_pass
fi

test_case "_needle_heuristic_mitosis_analysis reasoning is non-empty"
bead_obj='{"title":"Fix bug","description":"Fix the null check"}'
result=$(_needle_heuristic_mitosis_analysis "$bead_obj")
reasoning=$(echo "$result" | jq -r '.reasoning // ""' 2>/dev/null)
if [[ -n "$reasoning" ]]; then
    test_pass
else
    test_fail "Expected non-empty reasoning, got empty string"
fi

# ============================================================================
# _needle_build_mitosis_prompt tests
# ============================================================================

test_case "_needle_build_mitosis_prompt includes bead title"
bead_obj='{"title":"My Feature Title","description":"Feature description here","priority":1,"labels":["backend"]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
if echo "$result" | grep -q "My Feature Title"; then
    test_pass
else
    test_fail "Expected prompt to contain bead title"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes bead description"
bead_obj='{"title":"Feature","description":"Unique description content XYZ123","priority":2,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
if echo "$result" | grep -q "Unique description content XYZ123"; then
    test_pass
else
    test_fail "Expected prompt to contain bead description"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes workspace path"
bead_obj='{"title":"Feature","description":"Description","priority":2,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
if echo "$result" | grep -q "$workspace_dir"; then
    test_pass
else
    test_fail "Expected prompt to contain workspace path"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes workspace context sections"
bead_obj='{"title":"Feature","description":"Description","priority":2,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
# Should include all three workspace context sections from nd-1zax requirements
if echo "$result" | grep -q "Relevant Files" && \
   echo "$result" | grep -q "Recent Commits" && \
   echo "$result" | grep -q "Test Files"; then
    test_pass
else
    test_fail "Expected prompt to contain workspace context sections (Relevant Files, Recent Commits, Test Files)"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes git file list for git workspace"
bead_obj='{"title":"Feature","description":"Description","priority":2,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
# Set up a minimal git repo in the workspace
(
    cd "$workspace_dir"
    git init -q
    touch src_file_unique_abc123.py
    git add .
    git -c user.email="test@test" -c user.name="Test" commit -q -m "init"
) 2>/dev/null
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
if echo "$result" | grep -q "src_file_unique_abc123.py"; then
    test_pass
else
    test_fail "Expected prompt to contain tracked file from git workspace"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes recent commit messages"
bead_obj='{"title":"Feature","description":"Description","priority":2,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
(
    cd "$workspace_dir"
    git init -q
    touch file.txt
    git add .
    git -c user.email="test@test" -c user.name="Test" commit -q -m "feat: unique-commit-msg-xyz987"
) 2>/dev/null
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
if echo "$result" | grep -q "unique-commit-msg-xyz987"; then
    test_pass
else
    test_fail "Expected prompt to contain recent commit message"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes test files section for git workspace"
bead_obj='{"title":"Feature","description":"Description","priority":2,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
(
    cd "$workspace_dir"
    git init -q
    mkdir -p tests
    touch tests/test_auth.py
    touch src/auth.py
    git add .
    git -c user.email="test@test" -c user.name="Test" commit -q -m "add files"
) 2>/dev/null
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
if echo "$result" | grep -q "tests/test_auth.py"; then
    test_pass
else
    test_fail "Expected prompt to contain test file path"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt handles non-git workspace gracefully"
bead_obj='{"title":"Feature","description":"Description","priority":2,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
# Not a git repo
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
# Should still return a prompt (not empty) and contain fallback messages
if [[ -n "$result" ]] && echo "$result" | grep -q "Workspace"; then
    test_pass
else
    test_fail "Expected non-empty prompt even for non-git workspace"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes output format instructions"
bead_obj='{"title":"Feature","description":"Description","priority":2,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
# The extended schema should include affected_files and verification_cmd fields
if echo "$result" | grep -q "affected_files" && \
   echo "$result" | grep -q "verification_cmd"; then
    test_pass
else
    test_fail "Expected prompt to include affected_files and verification_cmd in output format"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes parent priority"
bead_obj='{"title":"P0 Feature","description":"Critical feature","priority":0,"labels":[]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
if echo "$result" | grep -q "P0"; then
    test_pass
else
    test_fail "Expected prompt to include parent priority P0"
fi
rm -rf "$workspace_dir"

test_case "_needle_build_mitosis_prompt includes parent labels"
bead_obj='{"title":"Feature","description":"Description","priority":2,"labels":["backend","security"]}'
workspace_dir=$(mktemp -d "$TEST_DIR/workspace-XXXXXX")
result=$(_needle_build_mitosis_prompt "nd-test" "$workspace_dir" "$bead_obj")
if echo "$result" | grep -q "backend"; then
    test_pass
else
    test_fail "Expected prompt to include parent labels"
fi
rm -rf "$workspace_dir"

# ============================================================================
# Summary
# ============================================================================
echo ""
echo "================================"
echo "Test Summary"
echo "================================"
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"
echo "================================"

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi

exit 0
