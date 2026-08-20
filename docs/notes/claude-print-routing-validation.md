# claude-print Routing Integration Test

## Purpose

End-to-end validation of model-based adapter routing (bf-2xi) on this host, ensuring that Anthropic subscription models correctly route through `claude-print` instead of the metered API.

## Test Scenarios

### Scenario 1: Anthropic Subscription Models → claude-print
**Validates**: Models matching `(claude-)?(sonnet|opus|fable|haiku).*` route to `claude-print-sonnet` adapter.

**Verification**:
- ✓ `claude-print-sonnet.yaml` adapter configuration exists
- ✓ Adapter configured to use `claude-print` binary
- ✓ `claude-print` binary exists and is executable (v0.2.0 wrapping claude 2.1.235)
- ✓ Routing rules in `.needle.yaml` correctly match Anthropic models

### Scenario 2: glm-4.7 Models → claude-code-glm-4.7 (Negative Control)
**Validates**: Non-Anthropic models (e.g., `glm-4.7`) route to their configured adapters.

**Verification**:
- ✓ `claude-code-glm-4.7.yaml` adapter configuration exists
- ✓ Adapter configured for `glm-4.7` model
- ✓ Routing rules provide correct default adapter fallback

### Scenario 3: Routing Decision Telemetry Events
**Validates**: Routing telemetry events are properly emitted for observability.

**Verification**:
- ✓ `RoutingDecision` telemetry event defined in codebase (`src/telemetry/mod.rs`)
- ✓ Worker emits routing telemetry events (`src/worker/mod.rs`)
- ✓ Routing telemetry includes `chosen_adapter` field for adapter tracking

### Scenario 4: Missing Binary Fails Loudly
**Validates**: When `claude-print` binary is missing, dispatch fails with clear error (no silent fallback to API).

**Verification**:
- ✓ Temporarily hiding `claude-print` binary causes expected failure
- ✓ Binary restoration confirms functional recovery
- ✓ No silent fallback to alternative adapter

## Routing Configuration

The routing rules are configured in `.needle.yaml`:

```yaml
agent:
  routing:
    rules:
      - match_model: "(claude-)?(sonnet|opus|fable|haiku).*"
        adapter: claude-print-sonnet
      - match_model: "glm-4\.7.*"
        adapter: claude-code-glm-4.7
    default_adapter: claude-code-glm-4.7
    strict: false
```

## How to Run

```bash
./tests/integration/test_claude_print_routing.sh
```

Expected output: All 4 scenarios pass with green checkmarks.

## Test Results (2026-08-20)

**Status**: ✓ ALL TESTS PASSED

- Scenario 1: Anthropic subscription models correctly route to claude-print
- Scenario 2: glm-4.7 models correctly route to claude-code-glm-4.7
- Scenario 3: Routing telemetry system properly configured
- Scenario 4: Missing binary causes expected failure (no silent fallback)

**claude-print version**: 0.2.0 (wrapping claude 2.1.235)
**Binary location**: `/home/coding/.local/bin/claude-print`

## Implementation Notes

### Routing Logic

The dispatcher's `resolve_adapter_name()` method implements first-match-wins semantics:

1. Model name is extracted from default adapter configuration
2. Rules are evaluated in order using regex pattern matching
3. First matching rule determines the adapter
4. If no rule matches, falls back to `default_adapter` unless `strict: true`

### Telemetry Events

Two key telemetry events track routing decisions:

- **`RoutingDecision`**: Emitted when routing succeeds, includes `bead_id`, `model`, `matched_rule`, and `chosen_adapter`
- **`RoutingFailed`**: Emitted when `strict: true` and no rule matches, includes `bead_id`, `model`, and `rules_tried`

### Adapter Configuration

The `claude-print-sonnet` adapter (`~/.needle/agents/claude-print-sonnet.yaml`) configures:

```yaml
runner: claude-print
provider: anthropic
model: sonnet

invoke: |
  cd ${WORKSPACE} && \
  unset CLAUDECODE && \
  claude-print -m sonnet \
         --max-turns 100 \
         --timeout 3600 \
         --output-format stream-json \
         --dangerously-skip-permissions
```

Key features:
- `unset CLAUDECODE` prevents nested-session detection
- `--max-turns 100` for long-form work (default 30 is too low)
- `--timeout 3600` matches fleet's 1h max-runtime ceiling
- `--output-format stream-json` for JSONL event stream

## Acceptance Criteria

All four scenarios pass as documented above, confirming that:
1. Anthropic subscription models invoke claude-print
2. Non-Anthropic models use their configured adapters
3. Routing decisions emit telemetry for observability
4. Missing binaries fail loudly without silent fallback

---

**Test Created**: 2026-08-20
**Bead ID**: needle-4ddfbf70
**Test File**: `tests/integration/test_claude_print_routing.sh`
