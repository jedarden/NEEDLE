# claude-print Routing Integration Test

## Overview

This integration test validates end-to-end model-based adapter routing (bf-2xi) on this host. The test is implemented in `tests/integration_claude_print_routing.rs`.

## Test Scenarios

### Scenario 1: Anthropic Subscription Models Route to claude-print
- **Test**: `scenario1_sonnet_routes_to_claude_print`
- **Validation**: 
  - Sonnet model resolves to claude-print adapter
  - Adapter configuration includes correct invoke template
  - Provider is set to "anthropic"
  - Model is configured for claude-sonnet-4-6

### Scenario 2: GLM-4.7 Routes to Default Adapter
- **Test**: `scenario2_glm47_routes_to_default_adapter`
- **Validation**:
  - glm-4.7 model resolves to claude-code-glm-4.7 adapter
  - Default adapter fallback works correctly

### Scenario 3: Routing Telemetry Events Emitted
- **Test**: `scenario3_routing_telemetry_events_emitted`
- **Validation**:
  - Routing decision events are emitted successfully
  - Events contain correct model, adapter, and matched rule information

### Scenario 4: Missing claude-print Binary Results in Loud Failure
- **Test**: `scenario4_missing_claude_print_binary_loud_failure`
- **Validation**:
  - When claude-print binary is missing, routing still resolves correctly
  - Adapter reports binary as missing (no silent fallback to API)
  - Test includes proper binary restoration

### Integration Test
- **Test**: `integration_end_to_end_claude_print_routing`
- **Comprehensive validation**:
  - Default routing configuration is correct
  - Multiple model patterns resolve correctly:
    - claude-sonnet-4-6 → claude-print
    - claude-opus-4-6 → claude-print
    - claude-fable-5 → claude-print
    - claude-haiku-4-5-20251001 → claude-print
    - sonnet → claude-print
    - opus → claude-print
    - glm-4.7 → claude-code-glm-4.7
    - gpt-5.6-terra → claude-code-glm-4.7
  - Telemetry events emitted successfully
  - Adapter configurations loaded correctly

## Current Routing Configuration

```yaml
agent:
  routing:
    rules:
      - match_model: (claude-)?(sonnet|opus|fable|haiku).*
        adapter: claude-print
    default_adapter: claude-code-glm-4.7
    strict: false
```

## Running the Tests

```bash
# Run all claude-print routing tests
cargo test --test integration_claude_print_routing

# Run specific scenario
cargo test --test integration_claude_print_routing scenario1

# Run integration test only
cargo test --test integration_claude_print_routing integration_end_to_end
```

## Test Prerequisites

- claude-print binary must be on PATH (some scenarios skip if not found)
- Test automatically handles binary renaming and restoration

## Acceptance Criteria

All four scenarios pass:
1. ✅ Sonnet routes to claude-print
2. ✅ GLM-4.7 routes to default adapter
3. ✅ Telemetry events emitted
4. ✅ Missing binary results in loud failure

## Documentation

The manual test procedure is documented as comments in the test file and can be used for manual verification of routing behavior.

## Implementation Notes

- Tests use `Telemetry::new()` for event emission testing
- Tests use `Dispatcher::new()` with custom routing config
- Binary restoration is guaranteed via panic hook
- Tests include proper cleanup of temporary workspaces
