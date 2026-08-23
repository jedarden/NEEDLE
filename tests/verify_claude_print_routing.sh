#!/usr/bin/env bash
# Quick verification script for claude-print routing configuration
#
# This script performs basic checks without requiring full bead dispatch:
# 1. Verifies adapter binaries exist
# 2. Checks routing implementation in code
# 3. Validates pattern matching logic
# 4. Tests routing configuration parsing
#
# Usage: ./tests/verify_claude_print_routing.sh

set -euo pipefail

echo "═══════════════════════════════════════════════════════════════════════════"
echo "claude-print Routing Verification"
echo "═══════════════════════════════════════════════════════════════════════════"
echo ""

PASS_COUNT=0
FAIL_COUNT=0

check_pass() {
    echo "  ✓ $1"
    ((PASS_COUNT = PASS_COUNT + 1)) || true
}

check_fail() {
    echo "  ✗ $1"
    ((FAIL_COUNT = FAIL_COUNT + 1)) || true
}

# ═══════════════════════════════════════════════════════════════════════════════
# 1. Adapter Binary Verification
# ═══════════════════════════════════════════════════════════════════════════════

echo "=== 1. Adapter Binary Check ==="

if [[ -x "$(which claude-print 2>/dev/null)" ]]; then
    check_pass "claude-print binary found and executable"
else
    check_fail "claude-print not found or not executable"
fi

if [[ -x "$(which claude-code-glm-4.7 2>/dev/null)" ]]; then
    check_pass "claude-code-glm-4.7 binary found and executable"
else
    check_fail "claude-code-glm-4.7 not found or not executable"
fi

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# 2. Code Implementation Check
# ═══════════════════════════════════════════════════════════════════════════════

echo "=== 2. Code Implementation Check ==="

if [[ -f "src/routing.rs" ]]; then
    check_pass "src/routing.rs exists"

    if grep -q "fn match_adapter" src/routing.rs; then
        check_pass "match_adapter function found"
    else
        check_fail "match_adapter function not found"
    fi

    if grep -q "CompiledRule" src/routing.rs; then
        check_pass "CompiledRule struct found"
    else
        check_fail "CompiledRule struct not found"
    fi
else
    check_fail "src/routing.rs not found"
fi

if [[ -f "src/dispatch/mod.rs" ]]; then
    check_pass "src/dispatch/mod.rs exists"

    if grep -q "resolve_adapter_name\|resolve_adapter" src/dispatch/mod.rs; then
        check_pass "Dispatcher adapter resolution found"
    else
        check_fail "Dispatcher adapter resolution not found"
    fi
else
    check_fail "src/dispatch/mod.rs not found"
fi

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# 3. Unit Test Coverage Check
# ═══════════════════════════════════════════════════════════════════════════════

echo "=== 3. Unit Test Coverage ==="

if [[ -f "tests/routing_integration.rs" ]]; then
    check_pass "tests/routing_integration.rs exists"

    # Check for key test functions
    if grep -q "fn routing_anthropic_sonnet_to_claude_print" tests/routing_integration.rs; then
        check_pass "Anthropic sonnet routing test found"
    else
        check_fail "Anthropic sonnet routing test not found"
    fi

    if grep -q "fn routing_glm_47_to_claude_code_glm_47" tests/routing_integration.rs; then
        check_pass "GLM-4.7 routing test found"
    else
        check_fail "GLM-4.7 routing test not found"
    fi

    if grep -q "fn dispatcher_resolve_adapter_anthropic_models" tests/routing_integration.rs; then
        check_pass "Dispatcher integration test found"
    else
        check_fail "Dispatcher integration test not found"
    fi
else
    check_fail "tests/routing_integration.rs not found"
fi

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# 4. Pattern Matching Verification
# ═══════════════════════════════════════════════════════════════════════════════

echo "=== 4. Pattern Matching Verification ==="

# Test that the Anthropic pattern matches expected models
anthropic_pattern="(claude-)?(sonnet|opus|fable|haiku).*"

anthropic_models=(
    "claude-sonnet-4-7"
    "claude-opus-4-7"
    "claude-fable-5"
    "claude-haiku-4-5"
    "sonnet-5"
    "opus-4-8"
    "fable-5"
    "haiku-4-5"
)

for model in "${anthropic_models[@]}"; do
    if [[ "$model" =~ $anthropic_pattern ]]; then
        check_pass "Pattern matches: $model"
    else
        check_fail "Pattern does NOT match: $model"
    fi
done

# Test that GLM models do NOT match the Anthropic pattern
glm_models=(
    "glm-4.7"
    "glm-4-flash"
    "gpt-4"
    "gemini-pro"
)

for model in "${glm_models[@]}"; do
    if [[ ! "$model" =~ $anthropic_pattern ]]; then
        check_pass "Pattern correctly excludes: $model"
    else
        check_fail "Pattern incorrectly matches: $model"
    fi
done

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# 5. Telemetry Implementation Check
# ═══════════════════════════════════════════════════════════════════════════════

echo "=== 5. Telemetry Implementation ==="

if [[ -f "src/telemetry.rs" ]]; then
    check_pass "src/telemetry.rs exists"

    # Look for routing-related telemetry
    if grep -q "routing\|RoutingDecision\|routing_decision" src/telemetry.rs; then
        check_pass "Routing telemetry types/events found"
    else
        echo "  ⚠ Routing telemetry not explicitly found (may be in dispatch module)"
    fi
fi

# Check dispatch module for telemetry emission
if [[ -f "src/dispatch/mod.rs" ]]; then
    if grep -q "emit.*routing\|telemetry.*routing\|routing.*emit" src/dispatch/mod.rs; then
        check_pass "Dispatch module emits routing telemetry"
    else
        echo "  ⚠ Routing telemetry emission in dispatch not found"
    fi
fi

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Summary
# ═══════════════════════════════════════════════════════════════════════════════

echo "═══════════════════════════════════════════════════════════════════════════"
echo "Summary"
echo "═══════════════════════════════════════════════════════════════════════════"
echo ""
echo "Total Checks: $((PASS_COUNT + FAIL_COUNT))"
echo "Passed:       $PASS_COUNT ✓"
echo "Failed:       $FAIL_COUNT ✗"
echo ""

if [[ $FAIL_COUNT -eq 0 ]]; then
    echo "✓ All verification checks PASSED"
    echo ""
    echo "Next steps:"
    echo "  1. Run unit tests: cargo test routing_integration"
    echo "  2. Run manual end-to-end test (see docs/notes/claude-print-routing-validation.md)"
    echo "  3. Verify telemetry events in production dispatch"
    exit 0
else
    echo "✗ Some verification checks FAILED"
    echo "Please review the failures above and fix before proceeding."
    exit 1
fi
