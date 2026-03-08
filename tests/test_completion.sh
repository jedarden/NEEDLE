#!/usr/bin/env bash
# Tests for NEEDLE shell completion (needle completion bash/zsh)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
NEEDLE_BIN="$PROJECT_DIR/bin/needle"

TEST_DIR=$(mktemp -d)
TEST_NEEDLE_HOME="$TEST_DIR/.needle"

export NEEDLE_HOME="$TEST_NEEDLE_HOME"
export NEEDLE_CONFIG_FILE="$TEST_NEEDLE_HOME/config.yaml"

cleanup() {
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

test_case() {
    local name="$1"
    ((TESTS_RUN++))
    echo -n "Testing: $name... "
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

assert_contains() {
    local output="$1"
    local expected="$2"
    if echo "$output" | grep -qF -- "$expected"; then
        return 0
    else
        return 1
    fi
}

# --------------------------------------------------------------------------
# Tests: needle completion bash
# --------------------------------------------------------------------------

test_case "bash completion: outputs valid bash script"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "bash completion" && \
   assert_contains "$output" "_needle_completion()" && \
   assert_contains "$output" "complete -F _needle_completion needle"; then
    test_pass
else
    test_fail "bash completion script missing key elements"
fi

test_case "bash completion: includes all main commands"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
for cmd in init run list status config logs version upgrade rollback agents heartbeat attach stop restart test-agent setup pulse completion help; do
    if ! assert_contains "$output" "$cmd"; then
        test_fail "missing command: $cmd"
        break
    fi
done
[[ $? -eq 0 ]] && test_pass

test_case "bash completion: includes option flags for 'run'"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "--agent" && \
   assert_contains "$output" "--workspace" && \
   assert_contains "$output" "--count"; then
    test_pass
else
    test_fail "run options missing from bash completion"
fi

test_case "bash completion: includes option flags for 'list'"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "--all" && \
   assert_contains "$output" "--json" && \
   assert_contains "$output" "--runner" && \
   assert_contains "$output" "--provider"; then
    test_pass
else
    test_fail "list options missing from bash completion"
fi

test_case "bash completion: handles --agent dynamic agent completion"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "_needle_list_available_agents"; then
    test_pass
else
    test_fail "bash completion missing dynamic agent helper function"
fi

test_case "bash completion: agent helper reads user agents dir"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" 'NEEDLE_HOME:-$HOME/.needle' && \
   assert_contains "$output" "agents"; then
    test_pass
else
    test_fail "agent helper doesn't reference correct agents dir"
fi

test_case "bash completion: includes option flags for 'logs'"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "--follow" && \
   assert_contains "$output" "--event" && \
   assert_contains "$output" "--strand"; then
    test_pass
else
    test_fail "logs options missing from bash completion"
fi

test_case "bash completion: includes option flags for 'pulse'"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "--detectors" && \
   assert_contains "$output" "--dry-run" && \
   assert_contains "$output" "--max-beads"; then
    test_pass
else
    test_fail "pulse options missing from bash completion"
fi

test_case "bash completion: includes option flags for 'heartbeat'"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "heartbeat_cmds" && \
   assert_contains "$output" "status" && \
   assert_contains "$output" "recover"; then
    test_pass
else
    test_fail "heartbeat completion missing from bash completion"
fi

test_case "bash completion: includes option flags for 'config'"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "config_cmds" && \
   assert_contains "$output" "show" && \
   assert_contains "$output" "validate"; then
    test_pass
else
    test_fail "config completion missing from bash completion"
fi

test_case "bash completion: worker session completion for attach/stop/restart"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "needle-" && \
   assert_contains "$output" "tmux list-sessions"; then
    test_pass
else
    test_fail "worker session completion missing"
fi

test_case "bash completion: completes global options before subcommand"
output=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if assert_contains "$output" "global_opts" && \
   assert_contains "$output" "--verbose" && \
   assert_contains "$output" "--no-color"; then
    test_pass
else
    test_fail "global options missing from bash completion"
fi

# --------------------------------------------------------------------------
# Tests: needle completion zsh
# --------------------------------------------------------------------------

test_case "zsh completion: outputs valid zsh compdef script"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "#compdef needle" && \
   assert_contains "$output" "_needle()"; then
    test_pass
else
    test_fail "zsh completion missing compdef or _needle function"
fi

test_case "zsh completion: includes all main commands with descriptions"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
for cmd in init run list status config logs version upgrade rollback agents heartbeat attach stop restart test-agent setup pulse completion help; do
    if ! assert_contains "$output" "'$cmd:"; then
        test_fail "missing command with description: $cmd"
        break
    fi
done
[[ $? -eq 0 ]] && test_pass

test_case "zsh completion: includes option flags for 'run'"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "--agent" && \
   assert_contains "$output" "--workspace" && \
   assert_contains "$output" "--count"; then
    test_pass
else
    test_fail "run options missing from zsh completion"
fi

test_case "zsh completion: handles dynamic agent completion state"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "->agents" && \
   assert_contains "$output" "_needle_get_agents"; then
    test_pass
else
    test_fail "zsh completion missing dynamic agent completion"
fi

test_case "zsh completion: uses correct loop syntax for agent discovery"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" 'for f in "\$agents_dir"/\*.yaml(N\.)' || \
   assert_contains "$output" 'for f in "$agents_dir"/*.yaml(N.)'; then
    test_pass
else
    test_fail "zsh agent discovery uses incorrect glob syntax"
fi

test_case "zsh completion: includes option flags for 'list'"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "--runner" && \
   assert_contains "$output" "--provider" && \
   assert_contains "$output" "--model"; then
    test_pass
else
    test_fail "list options missing from zsh completion"
fi

test_case "zsh completion: includes option flags for 'logs'"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "--follow" && \
   assert_contains "$output" "--event" && \
   assert_contains "$output" "--strand"; then
    test_pass
else
    test_fail "logs options missing from zsh completion"
fi

test_case "zsh completion: includes option flags for 'pulse'"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "--detectors" && \
   assert_contains "$output" "--dry-run" && \
   assert_contains "$output" "--max-beads"; then
    test_pass
else
    test_fail "pulse options missing from zsh completion"
fi

test_case "zsh completion: handles worker state contexts"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "->workers" && \
   assert_contains "$output" "->identifiers" && \
   assert_contains "$output" "_needle_get_workers"; then
    test_pass
else
    test_fail "worker state contexts missing from zsh completion"
fi

test_case "zsh completion: includes global options"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "global_opts" && \
   assert_contains "$output" "--verbose" && \
   assert_contains "$output" "--no-color"; then
    test_pass
else
    test_fail "global options missing from zsh completion"
fi

test_case "zsh completion: config subcommands are listed"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "config_cmds" && \
   assert_contains "$output" "show:Display" && \
   assert_contains "$output" "validate:Validate"; then
    test_pass
else
    test_fail "config subcommands missing from zsh completion"
fi

test_case "zsh completion: heartbeat subcommands are listed"
output=$(bash "$NEEDLE_BIN" completion zsh 2>&1)
if assert_contains "$output" "heartbeat_cmds" && \
   assert_contains "$output" "recover:Trigger" && \
   assert_contains "$output" "pause:Pause"; then
    test_pass
else
    test_fail "heartbeat subcommands missing from zsh completion"
fi

# --------------------------------------------------------------------------
# Tests: needle completion error handling
# --------------------------------------------------------------------------

test_case "completion: rejects unknown shell"
output=$(bash "$NEEDLE_BIN" completion fish 2>&1)
exit_code=$?
if [[ $exit_code -ne 0 ]] && (assert_contains "$output" "Unsupported" || assert_contains "$output" "fish" || assert_contains "$output" "bash"); then
    test_pass
else
    test_fail "should reject unknown shell 'fish' (exit_code=$exit_code)"
fi

test_case "completion: defaults to bash when no shell given"
output_no_arg=$(bash "$NEEDLE_BIN" completion 2>&1)
output_bash=$(bash "$NEEDLE_BIN" completion bash 2>&1)
if [[ "$output_no_arg" == "$output_bash" ]]; then
    test_pass
else
    test_fail "completion without arg should default to bash"
fi

# --------------------------------------------------------------------------
# Summary
# --------------------------------------------------------------------------

echo ""
echo "================================"
echo "Test Summary"
echo "================================"
echo "Tests run:    $TESTS_RUN"
echo "Tests passed: $TESTS_PASSED"
echo "Tests failed: $TESTS_FAILED"
echo "================================"

[[ $TESTS_FAILED -eq 0 ]] && exit 0 || exit 1
