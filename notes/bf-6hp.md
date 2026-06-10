# Agent Routing Config Implementation (bf-6hp)

## Summary

Implemented comprehensive agent routing configuration in NEEDLE's config system, enabling model-pattern-based adapter selection with workspace and environment variable overrides.

## Implementation Details

### Core Structures (src/config/mod.rs:28-56)

```rust
/// A single routing rule mapping model patterns to adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Regex or glob pattern to match against model names.
    pub match_model: String,
    /// Adapter to use for matching models (e.g., `claude-print`, `claude-code-glm-4.7`).
    pub adapter: String,
}

/// Agent routing configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Ordered list of routing rules (first match wins).
    #[serde(default)]
    pub rules: Vec<RoutingRule>,
    /// Fallback adapter when no rules match (defaults to `agent.default`).
    #[serde(default)]
    pub default_adapter: Option<String>,
}
```

### Integration Points

1. **AgentConfig** (line 79): Added `routing: Option<RoutingConfig>` field
2. **WorkspaceAgentOverrides** (line 1524): Added `routing: Option<RoutingConfig>` for workspace overrides
3. **Workspace override application** (lines 1715-1718): Handles routing config from `.needle.yaml`
4. **Environment variable overrides** (lines 1794-1800): `NEEDLE_AGENT__ROUTING__DEFAULT_ADAPTER`

### Validation

Regex validation with field path tracking (lines 2027-2044):
- Invalid regex patterns produce errors with field path `agent.routing.rules[N].match_model`
- Empty adapter names produce errors with field path `agent.routing.rules[N].adapter`

## Configuration Example

### Global Config (~/.config/needle/config.yaml)

```yaml
agent:
  default: claude
  routing:
    rules:
      - match_model: "(claude-)?(sonnet|opus).*"
        adapter: claude-print
      - match_model: "fable.*"
        adapter: fable-fast
      - match_model: "haiku.*"
        adapter: claude-code-glm-4.7
    default_adapter: claude-code-glm-4.7
```

### Workspace Config (.needle.yaml)

```yaml
agent:
  routing:
    rules:
      - match_model: "opus"
        adapter: opus-optimized
    default_adapter: workspace-default
```

### Environment Variable

```bash
export NEEDLE_AGENT__ROUTING__DEFAULT_ADAPTER=env-fallback
```

## Resolution Order

1. Built-in defaults (routing = None)
2. Global config file
3. Workspace config file (.needle.yaml)
4. Environment variables (NEEDLE_AGENT__ROUTING__*)
5. CLI arguments (highest precedence)

## Test Coverage

Comprehensive unit tests (86 total in config module, 8 routing-specific):

1. **Parsing tests**:
   - `routing_config_with_rules_parses`: Full YAML parsing
   - `routing_config_empty_rules_list_is_valid`: Empty rules list
   - `routing_config_yaml_roundtrip`: Serde roundtrip

2. **Validation tests**:
   - `invalid_regex_in_routing_rule_fails_validation`: Regex error with field path
   - `empty_adapter_in_routing_rule_fails_validation`: Empty adapter check
   - `valid_regex_in_routing_rule_passes_validation`: Valid config
   - `multiple_validation_errors_in_routing`: Multiple errors

3. **Override tests**:
   - `workspace_config_routing_override`: Workspace override
   - `env_var_routing_default_adapter_override`: Env var override
   - `env_var_routing_override_beats_workspace`: Precedence (env > workspace)

4. **Pattern tests**:
   - `routing_patterns_match_bare_aliases`: Bare aliases ('sonnet', 'opus')
   - `routing_patterns_match_full_model_ids`: Full IDs ('claude-sonnet-4-6')
   - `routing_patterns_match_with_wildcards`: Wildcard patterns

## MSRV Compliance

All code follows MSRV 1.75:
- No `unwrap()` or `expect()` outside test code
- All public functions return `Result<T>`
- Exhaustive match arms on outcome enums
- Proper error handling with `anyhow::Context`

## Requirements Met

✅ Ordered rule list with first-match-wins semantics
✅ Regex patterns for matching bare aliases and full model IDs
✅ `default_adapter` falls back to `agent.default` when None
✅ Workspace overrides via `.needle.yaml`
✅ Environment variable overrides via `NEEDLE_AGENT__ROUTING__*`
✅ Serde defaults (absent section = current behavior)
✅ Unit tests for parse, precedence, invalid regex with field paths
✅ MSRV 1.75, no unwrap/expect outside tests, exhaustive matches
