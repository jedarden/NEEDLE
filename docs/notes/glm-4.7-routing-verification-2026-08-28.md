# GLM-4.7 Routing Verification Test

## Overview

Implemented and validated a negative control test for GLM-4.7 model routing through the NEEDLE dispatch system. This test verifies that beads requesting GLM-4.7 models correctly route through the `claude-code-glm-4.7` adapter (not `claude-print`).

## Implementation

**Test File:** `tests/dispatch_model_routing_validation.rs`
**Test Function:** `resolve_adapter_glm_4_7_routing_negative_control`

### Test Design

The test follows a negative control pattern:
- **Positive assertion:** GLM-4.7 models route to `claude-code-glm-4.7`
- **Negative assertion:** GLM-4.7 models do NOT route to `claude-print`

This validates that the routing logic correctly distinguishes between:
- Anthropic subscription models (→ `claude-print`)
- Other models like GLM-4.7 (→ `claude-code-glm-4.7`)

### Routing Configuration

```rust
RoutingConfig {
    rules: vec![make_rule(
        "(claude-)?(sonnet|opus|fable|haiku).*",
        "claude-print",
    )],
    default_adapter: Some("claude-code-glm-4.7".to_string()),
    strict: false,
}
```

### Test Coverage

The test validates routing for:
- `glm-4.7` → `claude-code-glm-4.7`
- `glm-4.7-turbo` → `claude-code-glm-4.7`
- `glm-4.7-vision` → `claude-code-glm-4.7`

Each assertion includes a descriptive failure message explaining the routing expectation.

## Test Results

**Status:** ✅ PASSED

**Execution:**
```bash
cargo test --test dispatch_model_routing_validation resolve_adapter_glm_4_7_routing_negative_control
```

**Output:**
```
running 1 test
test resolve_adapter_glm_4_7_routing_negative_control ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.01s
```

## Verification Confirmation

The test confirms that:
1. GLM-4.7 models correctly resolve to the `claude-code-glm-4.7` adapter
2. GLM-4.7 models do NOT route to the `claude-print` adapter (negative control)
3. Multiple GLM-4.7 model variants all route correctly
4. The routing logic properly applies regex patterns for model matching

## Integration with Existing Tests

This test complements the existing `resolve_adapter_real_world_anthropic_subscription` test:
- **Existing test:** Validates Anthropic models → `claude-print`
- **New test:** Validates GLM-4.7 models → `claude-code-glm-4.7` (negative control)

Together, they provide comprehensive coverage of the routing logic for both subscription and API-billed models.

## Implementation Date

2026-08-28

## Related Files

- Test implementation: `tests/dispatch_model_routing_validation.rs`
- Dispatch module: `src/dispatch/mod.rs`
- Configuration types: `src/config.rs`
