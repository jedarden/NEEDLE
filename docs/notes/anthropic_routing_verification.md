# Anthropic Model Routing Verification

## Overview

This document describes the automated tests that verify Anthropic subscription models (sonnet, opus, fable, haiku) correctly route through the `claude-print` adapter in NEEDLE.

## Test Implementation

### Unit Tests

Two comprehensive Rust test suites verify the routing logic:

1. **`tests/anthropic_routing_e2e_test.rs`** - End-to-end routing validation
   - Tests adapter resolution for Anthropic subscription models
   - Verifies routing rule evaluation order (first match wins)
   - Validates default adapter fallback behavior
   - Confirms claude-print adapter configuration

2. **`tests/anthropic_routing_verification.rs`** - Detailed routing verification
   - Tests specific model patterns (sonnet, opus, fable, haiku)
   - Verifies non-Anthropic models use default adapter
   - Validates dispatcher-level adapter resolution
   - Confirms invoke template contains correct arguments

### Shell Script Test

**`tests/test_anthropic_routing_e2e.sh`** - End-to-end shell script test
- Creates a test bead requesting an Anthropic model
- Verifies claude-print binary is invoked
- Validates stream-json output format
- Generates test results report

## Configuration

### Routing Rules

The routing configuration in `.needle.yaml` defines:

```yaml
agent:
  routing:
    rules:
      - match_model: (claude-)?(sonnet|opus|fable|haiku).*
        adapter: claude-print
    default_adapter: claude-code-glm-4.7
    strict: false
```

This routes all Anthropic subscription models to `claude-print` adapter.

### claude-print Adapter

The `claude-print` adapter (`/home/coding/.config/needle/adapters/claude-print.yaml`) is configured with:

```yaml
name: claude-print
description: Claude Code interactive mode — subscription billing
agent_cli: claude-print
provider: anthropic
invoke_template: "cd {workspace} && claude-print --model {model} --max-turns 30 --output-format stream-json --dangerously-skip-permissions --no-inherit-hooks < {prompt_file}"
output_transform: needle-transform-claude
```

## Test Coverage

### Model Patterns Verified

✅ Anthropic subscription models route to `claude-print`:
- `claude-sonnet-4-6`, `sonnet-4-6`
- `claude-opus-4-7`, `opus-4-7`
- `claude-fable-5`, `fable-5`
- `claude-haiku-4-5`, `haiku-4-5`

✅ Non-Anthropic models route to default adapter:
- `glm-4.7`, `gpt-4`, `gemini-pro`

### Adapter Configuration Verified

✅ `claude-print` adapter has correct fields:
- `provider: anthropic`
- `invoke_template` contains `claude-print` binary
- `invoke_template` contains `{model}` placeholder
- `invoke_template` contains `stream-json` output format
- `output_transform: needle-transform-claude`

### Routing Logic Verified

✅ Routing rules are evaluated in order (first match wins)
✅ Default adapter is used when no rules match
✅ Dispatcher correctly resolves adapter names

## Running Tests

### Run Rust Unit Tests

```bash
# Run all Anthropic routing tests
cargo test --test anthropic_routing_e2e_test --test anthropic_routing_verification

# Run specific test
cargo test --test anthropic_routing_e2e_test anthropic_routing_e2e_sonnet_to_claude_print
```

### Run Shell Script Test

```bash
./tests/test_anthropic_routing_e2e.sh
```

## Test Results

### Latest Test Execution (2026-08-27)

All tests pass successfully:

- ✅ Prerequisites check (claude-print binary, bead CLI, routing config)
- ✅ Bead creation and configuration
- ✅ Adapter resolution for Anthropic models
- ✅ Adapter resolution for non-Anthropic models
- ✅ Routing rule evaluation order
- ✅ Default adapter fallback
- ✅ claude-print adapter configuration
- ✅ stream-json output format requested

## Conclusion

The Anthropic model routing system is **correctly configured** and **functioning as expected**:

✓ Anthropic subscription models (sonnet, opus, fable, haiku) route through `claude-print` adapter
✓ The `claude-print` binary is invoked with correct parameters
✓ The output format is `stream-json`
✓ The output transform `needle-transform-claude` is configured

## Integration with NEEDLE Workflow

When a bead requests an Anthropic subscription model:

1. NEEDLE dispatcher receives the bead with model specification
2. Routing rules match the model pattern (e.g., `claude-sonnet-4-6`)
3. Dispatcher resolves to `claude-print` adapter
4. Adapter invokes: `claude-print --model {model} --output-format stream-json`
5. Output is transformed by `needle-transform-claude`
6. Bead completes successfully

This ensures consistent routing for all Anthropic subscription models across the NEEDLE fleet.
