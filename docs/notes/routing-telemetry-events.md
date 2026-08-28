# Routing Decision Telemetry Events

## Overview

Routing decision telemetry events are emitted when the NEEDLE worker determines which adapter to use for a given model name. These events provide visibility into the routing decision process and help debug routing configuration issues.

## Event Structure

### EventKind::RoutingDecision

```rust
EventKind::RoutingDecision {
    bead_id: BeadId,           // The bead being processed
    model: String,              // The model name that was matched (e.g., "claude-sonnet-4-6")
    matched_rule: String,       // The routing rule pattern that matched or "default"
    chosen_adapter: String,    // The adapter that was selected (e.g., "claude-print")
}
```

### Event Kind String

When serialized to JSON, the event has kind: `"agent.routing_decision"`.

## Event Examples

### Anthropic Sonnet Routing

When a bead specifies `model: "claude-sonnet-4-6"`, the routing decision event is:

```json
{
  "kind": "agent.routing_decision",
  "timestamp": "2026-08-28T12:34:56.789Z",
  "bead_id": "needle-abc123",
  "model": "claude-sonnet-4-6",
  "matched_rule": "(claude-)?(sonnet|opus|fable|haiku).*",
  "chosen_adapter": "claude-print"
}
```

**Key points:**
- `model`: The exact model name specified in the bead
- `matched_rule`: The routing pattern that matched (Anthropic subscription models)
- `chosen_adapter`: The adapter that was selected (`claude-print` for subscription billing)

### GLM-4.7 Default Fallback

When a bead specifies `model: "glm-4.7"`, the routing decision event is:

```json
{
  "kind": "agent.routing_decision",
  "timestamp": "2026-08-28T12:34:56.789Z",
  "bead_id": "needle-def456",
  "model": "glm-4.7",
  "matched_rule": "default",
  "chosen_adapter": "claude-code-glm-4.7"
}
```

**Key points:**
- `model`: The model name (GLM-4.7 in this case)
- `matched_rule`: `"default"` indicates no explicit routing rule matched
- `chosen_adapter`: The default adapter from routing configuration

## Field Descriptions

### bead_id

The identifier of the bead being processed. This is the full bead ID (e.g., `needle-abc123`) that uniquely identifies the work item.

**Format:** String (BeadId)

**Validation:** Must be a valid bead ID that exists in the bead store.

### model

The model name that was being routed. This is the exact model string specified in the bead or workspace configuration.

**Format:** String (non-empty)

**Examples:**
- `"claude-sonnet-4-6"`
- `"claude-opus-4-7"`
- `"glm-4.7"`
- `"gpt-4"`

**Validation:** Must be non-empty.

### matched_rule

The routing rule pattern that matched the model name, or `"default"` if no explicit rule matched.

**Format:** String

**Possible values:**
- A regex pattern from `agent.routing.rules` that matched the model
- `"default"` when no explicit rule matched and the default adapter was used

**Examples:**
- `"(claude-)?(sonnet|opus|fable|haiku).*"` - Anthropic subscription model pattern
- `"glm-.*"` - GLM model pattern (if explicitly configured)
- `"default"` - No explicit rule matched

**Routing behavior:**
- Rules are evaluated in order (first match wins)
- If no rule matches, the `default_adapter` from routing config is used
- In strict mode with no match and no default, routing fails

### chosen_adapter

The name of the adapter that was selected for this model.

**Format:** String

**Examples:**
- `"claude-print"` - Anthropic subscription adapter
- `"claude-code-glm-4.7"` - Default GLM adapter
- `"workspace-custom-adapter"` - Workspace-specific adapter

**Validation:** The adapter must exist in the dispatcher's adapter registry.

## Emission Location

Routing decision events are emitted in `src/worker/mod.rs` around line 5389:

```rust
self.telemetry.emit(EventKind::RoutingDecision {
    bead_id: id,
    model,
    matched_rule: matched_rule.clone(),
    chosen_adapter: chosen_adapter_name.clone(),
})?;
```

This happens during the dispatch phase when the worker is preparing to invoke the adapter for a bead.

## Routing Configuration

Routing is configured in `.needle.yaml`:

```yaml
agent:
  default: claude
  routing:
    rules:
      - match_model: "(claude-)?(sonnet|opus|fable|haiku).*"
        adapter: claude-print
    default_adapter: claude-code-glm-4.7
    strict: false
```

### Configuration Fields

- `rules`: List of routing rules, evaluated in order
- `match_model`: Regex pattern to match against model names
- `adapter`: Adapter name to use when the pattern matches
- `default_adapter`: Fallback adapter when no rules match
- `strict`: If `true`, fail loudly when no rule matches (default: `false`)

## Routing Semantics

### First-Match-Wins

When multiple patterns match a model name, the **first** matching rule wins:

```yaml
rules:
  - match_model: "claude-.*"           # Matches first
    adapter: first-adapter
  - match_model: "claude-sonnet.*"    # More specific, but comes second
    adapter: second-adapter
```

For `claude-sonnet-4-6`, both patterns match, but `first-adapter` is chosen.

### Default Fallback

When no rules match, the `default_adapter` is used:

```yaml
rules:
  - match_model: "(claude-)?(sonnet|opus).*"
    adapter: claude-print
default_adapter: claude-code-glm-4.7
```

For `glm-4.7`, no rules match, so `claude-code-glm-4.7` is chosen (matched_rule = `"default"`).

### Strict Mode

With `strict: true` and no `default_adapter`, routing returns `None` for unmatched models:

```yaml
rules:
  - match_model: "claude-.*"
    adapter: claude-print
strict: true
# No default_adapter
```

For `glm-4.7`, routing fails with a `RoutingFailed` event instead of falling back.

## Related Events

### EventKind::RoutingFailed

When routing fails in strict mode, a `RoutingFailed` event is emitted:

```rust
EventKind::RoutingFailed {
    bead_id: BeadId,
    model: String,
    rules_tried: u32,
}
```

Example:

```json
{
  "kind": "agent.routing_failed",
  "timestamp": "2026-08-28T12:34:56.789Z",
  "bead_id": "needle-xyz789",
  "model": "unknown-model",
  "rules_tried": 1
}
```

## Verification Tests

See `tests/routing_telemetry_verification.rs` for comprehensive tests that verify:

1. ✅ Anthropic Sonnet routing emits correct events
2. ✅ Anthropic Opus routing emits correct events
3. ✅ Anthropic Fable routing emits correct events
4. ✅ Anthropic Haiku routing emits correct events
5. ✅ GLM-4.7 routing emits correct events with default fallback
6. ✅ Event metadata completeness (all fields present and valid)
7. ✅ Event structure documentation

## Debugging Routing Issues

### Check Telemetry Logs

Routing decision events are written to the worker's telemetry log:

```bash
# Find routing events for a specific bead
jq 'select(.kind == "agent.routing_decision" and .bead_id == "needle-abc123")' \
  ~/.needle/logs/worker-*.jsonl

# Find all routing events that used default fallback
jq 'select(.kind == "agent.routing_decision" and .matched_rule == "default")' \
  ~/.needle/logs/worker-*.jsonl

# Find routing events for a specific adapter
jq 'select(.kind == "agent.routing_decision" and .chosen_adapter == "claude-print")' \
  ~/.needle/logs/worker-*.jsonl
```

### Common Issues

**Issue:** Model routes to wrong adapter

**Debug:** Check the routing configuration and verify which rule matched:
```bash
# Check what matched_rule was used
jq 'select(.kind == "agent.routing_decision" and .model == "your-model")' \
  ~/.needle/logs/worker-*.jsonl
```

**Issue:** No routing event emitted

**Debug:** Check for `RoutingFailed` events:
```bash
jq 'select(.kind == "agent.routing_failed")' \
  ~/.needle/logs/worker-*.jsonl
```

**Issue:** Default adapter always used

**Debug:** Check if routing rules are correctly ordered and patterns match:
```bash
# Check if matched_rule is "default" when it shouldn't be
jq 'select(.kind == "agent.routing_decision" and .matched_rule == "default")' \
  ~/.needle/logs/worker-*.jsonl
```

## Historical Context

### June 15, 2026 Deadline

The routing feature was implemented to maximize Anthropic subscription credit value before the June 15, 2026 deadline, when Anthropic's credit split changed:

- **Before June 15, 2026:** `claude -p` (claude-print) consumed subscription credits
- **After June 15, 2026:** `claude -p` switched to API credits

Routing Anthropic models to `claude-print` before the deadline maximized subscription value. Non-Anthropic models defaulted to `claude-code-glm-4.7`.

See `tests/routing_integration.rs::routing_june_15_deadline_rationale` for the full context.

## Summary

Routing decision telemetry events provide complete visibility into:

1. **Which models** are being processed (`model` field)
2. **Which rules** matched (`matched_rule` field)
3. **Which adapters** were selected (`chosen_adapter` field)
4. **Why** a routing decision was made (explicit rule vs. default fallback)

These events are essential for debugging routing issues, verifying configuration correctness, and understanding the dispatch flow in production NEEDLE fleets.
